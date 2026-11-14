//! Direct, bounded capability discovery: deterministic recall followed by Luna relevance matching.

use crate::discovery_scheduling as scheduling;
pub(crate) mod decision_checkpoint;
mod metrics;
use plasm_core::prerequisites::{
    prerequisite_closure, CapabilityRef, PrerequisiteClosure, PrerequisiteError,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::discovery_matcher::{self as matcher, CapabilityMatchReceipt};
use crate::discovery_store::{
    BacklogKey, DiscoveryAuthorization, DiscoverySessionPin, DiscoveryStore, FrontierKey,
    RetrievalReceipt, SearchAdmission, SearchKey,
};

pub use crate::discovery_recovery::{DiscoveryRecovery, RECOVERY_GUIDANCE};

type Result<T> = std::result::Result<T, DiscoveryServiceError>;

/// Scheduling evidence only: no model judgments or permissions cross intents.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeferredBacklog {
    revision: usize,
    last_intent: String,
    last_receipt: String,
    entries: Vec<DeferredBacklogEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeferredBacklogEntry {
    reference: CapabilityRef,
    age: usize,
}

impl DeferredBacklog {
    fn priorities(
        &self,
        intent: &str,
        allowed: &DiscoveryAuthorization,
    ) -> std::collections::BTreeMap<CapabilityRef, usize> {
        if self.last_intent == intent {
            return Default::default();
        }
        self.entries
            .iter()
            .filter(|entry| allowed.permits(&entry.reference))
            .map(|entry| (entry.reference.clone(), entry.age))
            .collect()
    }

    fn observe(
        &mut self,
        revision: usize,
        intent: String,
        receipt: &RoutingReceipt,
        receipt_id: String,
    ) {
        if revision < self.revision || self.last_receipt == receipt_id {
            return;
        }
        let old: std::collections::BTreeMap<_, _> = self
            .entries
            .iter()
            .map(|entry| (entry.reference.clone(), entry.age))
            .collect();
        self.entries = receipt
            .search
            .deferred
            .iter()
            .filter(|item| {
                matches!(
                    item.reason,
                    matcher::online::DeferredReason::InputBytes
                        | matcher::online::DeferredReason::ProjectionLimit
                        | matcher::online::DeferredReason::NotNominated
                ) && receipt.authorization.permits(&item.reference)
                    && !receipt.search.exposed.contains(&item.reference)
            })
            .take(crate::discovery_store::CANDIDATE_LIMIT)
            .map(|item| DeferredBacklogEntry {
                reference: item.reference.clone(),
                age: old
                    .get(&item.reference)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1),
            })
            .collect();
        self.last_intent = intent;
        self.last_receipt = receipt_id;
        self.revision = revision;
    }
}

fn deferred_backlog_key(
    session: &str,
    generation: &str,
    allowed: &DiscoveryAuthorization,
    model: &str,
) -> Result<String> {
    Ok(plasm_core::catalog_discovery::content_hash(
        &serde_json::to_vec(&(
            "deferred-backlog",
            matcher::whole_intent::POLICY,
            session,
            generation,
            allowed,
            model,
        ))?,
    ))
}

/// Keep current request fragments ahead of historical recall, while retaining
/// every distinct source span as a query. Ordering nominates candidates only.
fn ordered_recall_queries<'a>(
    current: &'a str,
    spans: impl IntoIterator<Item = &'a str>,
) -> Vec<&'a str> {
    let mut recent = Vec::new();
    let mut historical = Vec::new();
    for span in spans {
        if crate::intent_provenance::current_request_contains_span(current, span) {
            recent.push(span);
        } else {
            historical.push(span);
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    std::iter::once(current)
        .chain(recent)
        .chain(historical)
        .filter(|query| seen.insert(*query))
        .collect()
}

/// Completed searches advance priority once within a self-contained request.
/// No negative relevance decisions are applied to exposure or permissions.
#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct SearchFrontier {
    completed: std::collections::BTreeMap<String, std::collections::BTreeSet<CapabilityRef>>,
}

impl SearchFrontier {
    fn visits(&self) -> std::collections::BTreeMap<CapabilityRef, usize> {
        let mut visits = std::collections::BTreeMap::new();
        for references in self.completed.values() {
            for reference in references {
                *visits.entry(reference.clone()).or_default() += 1;
            }
        }
        visits
    }

    fn record(&mut self, key: &str, receipt: &RoutingReceipt) {
        self.completed.entry(key.to_owned()).or_insert_with(|| {
            receipt
                .retrieval
                .candidates
                .iter()
                .filter(|candidate| {
                    receipt
                        .matching
                        .matches
                        .iter()
                        .any(|m| m.capability_id == candidate.id)
                })
                .map(|candidate| candidate.reference.clone())
                .collect()
        });
    }
}

fn frontier_key(
    pin: &str,
    generation: &str,
    allowed: &DiscoveryAuthorization,
    model: &str,
    current: &str,
) -> Result<String> {
    Ok(plasm_core::catalog_discovery::content_hash(
        &serde_json::to_vec(&(
            "request-frontier",
            matcher::whole_intent::POLICY,
            pin,
            generation,
            allowed,
            model,
            current,
        ))?,
    ))
}

/// Supplemental requests share a visit frontier within the authorized session,
/// even as the agent refines its wording. Visits affect ordering only; every
/// admitted capability is judged again for the current intent.
fn supplemental_frontier_key(
    session: &str,
    generation: &str,
    allowed: &DiscoveryAuthorization,
    model: &str,
) -> Result<String> {
    Ok(plasm_core::catalog_discovery::content_hash(
        &serde_json::to_vec(&(
            "supplemental-frontier",
            matcher::whole_intent::POLICY,
            session,
            generation,
            allowed,
            model,
        ))?,
    ))
}

fn search_key(
    generation: &str,
    provenance: &crate::intent_provenance::IntentProvenance,
    allowed: &DiscoveryAuthorization,
    model: &str,
    exposed: &[CapabilityRef],
) -> Result<String> {
    Ok(plasm_core::catalog_discovery::content_hash(
        &serde_json::to_vec(&(
            matcher::online::SEARCH_POLICY,
            generation,
            provenance,
            allowed,
            model,
            exposed.iter().collect::<std::collections::BTreeSet<_>>(),
        ))?,
    ))
}

// Keep model-free allocation experiments on the production frontier identity.
#[cfg(test)]
pub(crate) fn diagnostic_frontier_key(current: &str, allowed: &DiscoveryAuthorization) -> String {
    frontier_key(
        "diagnostic-pin",
        "frozen",
        allowed,
        "fixture-model",
        current,
    )
    .unwrap()
}

#[cfg(test)]
mod additive_identity_tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn equal_current_requests_keep_distinct_ancestry_in_search_identity() {
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let first =
            crate::intent_provenance::IntentProvenance::from_turns(["Inspect".into()]).unwrap();
        let revised = crate::intent_provenance::IntentProvenance::from_turns([
            "Prepare".into(),
            "Inspect".into(),
        ])
        .unwrap();
        assert_eq!(first.current(), revised.current());
        assert_ne!(
            search_key("generation", &first, &allowed, "model", &[]).unwrap(),
            search_key("generation", &revised, &allowed, "model", &[]).unwrap(),
        );
    }

    #[test]
    fn search_identity_includes_canonical_exposure() {
        let intent =
            crate::intent_provenance::IntentProvenance::from_turns(["Inspect measurements".into()])
                .unwrap();
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let a = CapabilityRef {
            catalog: "matrix".into(),
            capability: "read".into(),
        };
        let b = CapabilityRef {
            catalog: "matrix".into(),
            capability: "prepare".into(),
        };
        let key = |refs: &[CapabilityRef]| {
            search_key("generation", &intent, &allowed, "model", refs).unwrap()
        };
        assert_ne!(key(&[]), key(std::slice::from_ref(&a)));
        assert_ne!(key(std::slice::from_ref(&a)), key(&[a.clone(), b.clone()]));
        assert_eq!(key(&[a.clone(), b.clone()]), key(&[b, a.clone(), a]));
    }
}

#[derive(Debug, Error)]
pub enum DiscoveryServiceError {
    #[error("OPENROUTER_API_KEY is required for capability discovery")]
    MissingApiKey(#[source] std::env::VarError),
    #[error("discovery selector API key must not be empty")]
    EmptyApiKey,
    #[error("discovery requires the Luna Decisions model or a dated revision")]
    InvalidDecisionModel,
    #[error("an exposed capability is no longer authorized")]
    ExposedCapabilityUnauthorized,
    #[error("an exposed capability is absent from the pinned generation")]
    ExposedCapabilityMissing,
    #[error("supplemental frontier requires supplemental discovery")]
    ExpectedSupplementalLane,
    #[error("supplemental discovery generation does not match the session pin")]
    SupplementalGenerationMismatch,
    #[error("declared prerequisite closure contains an unauthorized capability")]
    UnauthorizedClosure,
    #[error(
        "this discovery search was interrupted or failed; its spent work budget cannot be replayed"
    )]
    SearchAlreadyStarted,
    #[error("discovery search was already admitted: {0}")]
    SearchInterrupted(Box<matcher::online::SearchProgress>),
    #[error("the discovery index cache is poisoned")]
    SearchIndexPoisoned,
    #[error(transparent)]
    Store(#[from] crate::discovery_store::DiscoveryStoreError),
    #[error(transparent)]
    Prerequisite(#[from] PrerequisiteError),
    #[error(transparent)]
    Retrieval(#[from] crate::discovery_store::RetrievalValidationError),
    #[error(transparent)]
    Matcher(Box<matcher::DiscoveryMatcherError>),
    #[error(transparent)]
    Operation(#[from] matcher::operations::OperationError),
    #[error("whole-intent selection failed: {0}")]
    WholeIntent(String),
    #[error(transparent)]
    ProviderTransport(#[from] plasm_runtime::provider_transport::ProviderTransportError),
    #[error(transparent)]
    DecisionRecord(#[from] plasm_runtime::provider_transport::ProviderRecordError),
    #[error(transparent)]
    HttpClient(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("discovery diagnostic file operation failed")]
    Io(#[from] std::io::Error),
}

impl From<matcher::DiscoveryMatcherError> for DiscoveryServiceError {
    fn from(error: matcher::DiscoveryMatcherError) -> Self {
        Self::Matcher(Box::new(error))
    }
}

impl From<matcher::whole_intent::WholeIntentError> for DiscoveryServiceError {
    fn from(error: matcher::whole_intent::WholeIntentError) -> Self {
        Self::WholeIntent(error.to_string())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingReceipt {
    pub intent_provenance: crate::intent_provenance::IntentProvenance,
    pub authorization: DiscoveryAuthorization,
    #[serde(default)]
    pub intent_analysis: String,
    pub intent: String,
    pub pin_id: String,
    pub retrieval: RetrievalReceipt,
    pub matching: CapabilityMatchReceipt,
    pub search: matcher::online::SearchReceipt,
    pub closure: Option<PrerequisiteClosure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<DiscoveryRecovery>,
}

pub struct RouteTurn<'a> {
    pub new_generation: &'a str,
    pub intent_provenance: &'a crate::intent_provenance::IntentProvenance,
    pub logical_session: Option<&'a str>,
    pub allowed: &'a DiscoveryAuthorization,
    pub exposed: &'a [CapabilityRef],
    pub expires_at: std::time::SystemTime,
}

struct RouteWork<'a> {
    key: &'a str,
    generation: &'a str,
    provenance: &'a crate::intent_provenance::IntentProvenance,
    allowed: &'a DiscoveryAuthorization,
    exposed: &'a [CapabilityRef],
    visits: &'a std::collections::BTreeMap<CapabilityRef, usize>,
    deferred_priorities: &'a std::collections::BTreeMap<CapabilityRef, usize>,
}

pub struct DiscoveryService {
    store: DiscoveryStore,
    transport: std::sync::Arc<dyn plasm_runtime::provider_transport::ProviderTransport>,
    match_model: String,
    rejection_dir: Option<std::path::PathBuf>,
    guided_capabilities: std::collections::BTreeSet<CapabilityRef>,
    lane: DiscoveryLane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum DiscoveryLane {
    Ordinary,
    Supplemental,
}

impl DiscoveryService {
    #[cfg(test)]
    pub(crate) fn cached_test_service(store: DiscoveryStore) -> Self {
        Self {
            store,
            transport: std::sync::Arc::new(
                plasm_runtime::provider_transport::ReqwestProviderTransport::new(
                    reqwest::Client::builder()
                        .proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap())
                        .timeout(std::time::Duration::from_secs(1))
                        .build()
                        .unwrap(),
                    matcher::whole_intent::ENDPOINT,
                    "no-provider-calls-permitted",
                ),
            ),
            match_model: matcher::whole_intent::DEFAULT_MODEL.into(),
            rejection_dir: None,
            guided_capabilities: Default::default(),
            lane: DiscoveryLane::Ordinary,
        }
    }
    pub fn from_env(store: DiscoveryStore) -> Result<Self> {
        let api_key =
            std::env::var("OPENROUTER_API_KEY").map_err(DiscoveryServiceError::MissingApiKey)?;
        if api_key.trim().is_empty() {
            return Err(DiscoveryServiceError::EmptyApiKey);
        }
        let model = std::env::var("PLASM_DISCOVERY_CAPABILITY_MATCH_MODEL")
            .unwrap_or_else(|_| matcher::whole_intent::DEFAULT_MODEL.into());
        if !crate::decision_codec::model_matches(matcher::RELEVANCE_MODEL, &model) {
            return Err(DiscoveryServiceError::InvalidDecisionModel);
        }
        Ok(Self {
            guided_capabilities: Default::default(),
            lane: DiscoveryLane::Ordinary,
            rejection_dir: std::env::var_os("PLASM_DISCOVERY_REJECTION_DIR")
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from),
            store,
            transport: std::sync::Arc::new(scheduling::ScheduledProviderTransport::new(
                std::sync::Arc::new(
                    plasm_runtime::provider_transport::ReqwestProviderTransport::new(
                        reqwest::Client::builder()
                            .timeout(std::time::Duration::from_secs(300))
                            .build()?,
                        matcher::whole_intent::ENDPOINT,
                        api_key,
                    ),
                ),
                scheduling::Workload::DiscoveryDecision,
            )),
            match_model: model,
        })
    }

    /// Run guidance in its own bounded matcher packet, without consuming the
    /// ordinary discovery packet's capacity or changing session exposure.
    pub fn supplemental(mut self, capabilities: impl IntoIterator<Item = CapabilityRef>) -> Self {
        self.lane = DiscoveryLane::Supplemental;
        self.guided_capabilities = capabilities.into_iter().take(32).collect();
        self
    }

    /// Shared new/extend state machine; hosts supply authenticated scope and expiry.
    pub async fn route_turn(&self, request: RouteTurn<'_>) -> Result<RoutingReceipt> {
        let key = request
            .logical_session
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let budget = self.store.provider_budgets.for_session(&key).await;
        scheduling::in_session(budget, self.route_turn_inner(request)).await
    }

    async fn route_turn_inner(&self, request: RouteTurn<'_>) -> Result<RoutingReceipt> {
        let pin_id = request
            .logical_session
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let generation = if request.logical_session.is_some() {
            let generation = self.store.pinned_generation(&pin_id).await?;
            self.store
                .renew_session_pin(
                    &DiscoverySessionPin {
                        pin_id: pin_id.clone(),
                        generation: generation.clone(),
                        authorization: request.allowed.clone(),
                    },
                    request.expires_at.into(),
                )
                .await?;
            generation
        } else {
            self.store
                .pin_generation(&pin_id, request.new_generation, request.expires_at.into())
                .await?;
            request.new_generation.to_owned()
        };
        let policy_models = self.match_model.clone();
        let selection_request = request.intent_provenance.selection_request();
        let intent_id = plasm_core::catalog_discovery::content_hash(selection_request.as_bytes());
        let backlog_key =
            deferred_backlog_key(&pin_id, &generation, request.allowed, &policy_models)?;
        let backlog = self
            .store
            .searches()
            .backlog(BacklogKey(&backlog_key))
            .await?
            .unwrap_or_default();
        let deferred_priorities = if self.lane == DiscoveryLane::Ordinary {
            backlog.priorities(&intent_id, request.allowed)
        } else {
            Default::default()
        };
        let frontier_key = frontier_key(
            &pin_id,
            &generation,
            request.allowed,
            &policy_models,
            &selection_request,
        )?;
        let search = search_key(
            &generation,
            request.intent_provenance,
            request.allowed,
            &policy_models,
            request.exposed,
        )?;
        let base_key =
            if self.guided_capabilities.is_empty() && self.lane == DiscoveryLane::Ordinary {
                plasm_core::catalog_discovery::content_hash(
                    format!("{frontier_key}:{search}").as_bytes(),
                )
            } else {
                plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&(
                    &frontier_key,
                    &search,
                    &self.guided_capabilities,
                    self.lane,
                ))?)
            };
        let key = if deferred_priorities.is_empty() {
            base_key
        } else {
            let ranks: Vec<_> = deferred_priorities.iter().collect();
            plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&(base_key, ranks))?)
        };
        let mut receipt = self
            .route_frontier(
                &frontier_key,
                &key,
                request.exposed,
                |visits, effective_key| {
                    let generation = &generation;
                    async move {
                        self.route_uncached(RouteWork {
                            key: &effective_key,
                            generation,
                            provenance: request.intent_provenance,
                            allowed: request.allowed,
                            exposed: request.exposed,
                            visits: &visits,
                            deferred_priorities: &deferred_priorities,
                        })
                        .await
                    }
                },
            )
            .await?;
        receipt.pin_id = pin_id;
        self.store
            .commit_discovery_progress(
                &receipt.pin_id,
                request.intent_provenance,
                request.logical_session.is_some(),
            )
            .await?;
        if self.lane == DiscoveryLane::Ordinary {
            let receipt_id =
                plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&receipt)?);
            self.store
                .searches()
                .with_backlog(BacklogKey(&backlog_key), || async {
                    let mut latest = self
                        .store
                        .searches()
                        .backlog(BacklogKey(&backlog_key))
                        .await?
                        .unwrap_or_default();
                    latest.observe(
                        request.intent_provenance.turns().count(),
                        intent_id,
                        &receipt,
                        receipt_id,
                    );
                    self.store
                        .searches()
                        .checkpoint_backlog(BacklogKey(&backlog_key), &latest)
                        .await?;
                    Ok::<_, DiscoveryServiceError>(())
                })
                .await??;
        }
        Ok(receipt)
    }

    /// Deterministic retrieval bounds the candidate universe; contribution judgment independently
    /// judges relevance over typed candidates. Selection never certifies task completion.
    pub async fn route(
        &self,
        generation: &str,
        provenance: &crate::intent_provenance::IntentProvenance,
        allowed: &DiscoveryAuthorization,
        exposed: &[CapabilityRef],
    ) -> Result<RoutingReceipt> {
        if !exposed.iter().all(|reference| allowed.permits(reference)) {
            return Err(DiscoveryServiceError::ExposedCapabilityUnauthorized);
        }
        let policy_models = self.match_model.clone();
        let base = search_key(generation, provenance, allowed, &policy_models, exposed)?;
        let key = if self.guided_capabilities.is_empty() && self.lane == DiscoveryLane::Ordinary {
            base
        } else {
            plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&(
                base,
                &self.guided_capabilities,
                self.lane,
            ))?)
        };
        self.route_cached(&key, exposed, || async {
            self.route_uncached(RouteWork {
                key: &key,
                generation,
                provenance,
                allowed,
                exposed,
                visits: &Default::default(),
                deferred_priorities: &Default::default(),
            })
            .await
        })
        .await
    }

    /// Advance the session's supplemental frontier without committing a new
    /// conversational intent. Each ticket is judged independently; only prior
    /// visit counts are shared between turns.
    pub async fn route_supplemental_turn(
        &self,
        session: &str,
        ticket: uuid::Uuid,
        generation: &str,
        provenance: &crate::intent_provenance::IntentProvenance,
        allowed: &DiscoveryAuthorization,
        exposed: &[CapabilityRef],
    ) -> Result<RoutingReceipt> {
        let budget = self.store.provider_budgets.for_session(session).await;
        scheduling::in_session(
            budget,
            self.route_supplemental_turn_inner(
                session, ticket, generation, provenance, allowed, exposed,
            ),
        )
        .await
    }

    async fn route_supplemental_turn_inner(
        &self,
        session: &str,
        ticket: uuid::Uuid,
        generation: &str,
        provenance: &crate::intent_provenance::IntentProvenance,
        allowed: &DiscoveryAuthorization,
        exposed: &[CapabilityRef],
    ) -> Result<RoutingReceipt> {
        if self.lane != DiscoveryLane::Supplemental {
            return Err(DiscoveryServiceError::ExpectedSupplementalLane);
        }
        if self.store.pinned_generation(session).await? != generation {
            return Err(DiscoveryServiceError::SupplementalGenerationMismatch);
        }
        if !exposed.iter().all(|reference| allowed.permits(reference)) {
            return Err(DiscoveryServiceError::ExposedCapabilityUnauthorized);
        }
        let policy_models = self.match_model.clone();
        let frontier = supplemental_frontier_key(session, generation, allowed, &policy_models)?;
        let request = search_key(generation, provenance, allowed, &policy_models, exposed)?;
        let key = plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&(
            &frontier,
            request,
            &self.guided_capabilities,
            ticket,
        ))?);
        self.route_frontier(
            &frontier,
            &key,
            exposed,
            |visits, effective_key| async move {
                self.route_uncached(RouteWork {
                    key: &effective_key,
                    generation,
                    provenance,
                    allowed,
                    exposed,
                    visits: &visits,
                    deferred_priorities: &Default::default(),
                })
                .await
            },
        )
        .await
    }

    async fn route_frontier<F, Fut>(
        &self,
        frontier_key: &str,
        key: &str,
        exposed: &[CapabilityRef],
        work: F,
    ) -> Result<RoutingReceipt>
    where
        F: FnOnce(std::collections::BTreeMap<CapabilityRef, usize>, String) -> Fut,
        Fut: std::future::Future<Output = Result<RoutingReceipt>>,
    {
        // One request-frontier owner, not nested database leases. A duplicate completion
        // can repair a failed frontier write without repeating provider work.
        let wait_started = std::time::Instant::now();
        let _same_request = self.store.pending_decisions.claim(frontier_key).await;
        metrics::record(metrics::Stage::FrontierLockWait, wait_started.elapsed());
        self.store
            .searches()
            .with_frontier(FrontierKey(frontier_key), || async {
                let mut frontier = self
                    .store
                    .searches()
                    .frontier(FrontierKey(frontier_key))
                    .await?
                    .unwrap_or_default();
                let visits = frontier.visits();
                let receipt = self
                    .route_owned(key, exposed, || work(visits, key.to_owned()))
                    .await?;
                frontier.record(key, &receipt);
                self.store
                    .searches()
                    .checkpoint_frontier(FrontierKey(frontier_key), &frontier)
                    .await?;
                Ok(receipt)
            })
            .await?
    }

    async fn route_cached<F, Fut>(
        &self,
        key: &str,
        exposed: &[CapabilityRef],
        work: F,
    ) -> Result<RoutingReceipt>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<RoutingReceipt>>,
    {
        let wait_started = std::time::Instant::now();
        let _same_search = self.store.pending_decisions.claim(key).await;
        metrics::record(metrics::Stage::SearchLockWait, wait_started.elapsed());
        self.store
            .searches()
            .with_search(SearchKey(key), || async {
                self.route_owned(key, exposed, work).await
            })
            .await?
    }

    /// Caller holds the corresponding search or session ownership lease.
    async fn route_owned<F, Fut>(
        &self,
        key: &str,
        exposed: &[CapabilityRef],
        work: F,
    ) -> Result<RoutingReceipt>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<RoutingReceipt>>,
    {
        if let Some(receipt) = self.store.searches().completion(SearchKey(key)).await? {
            if exposed
                .iter()
                .any(|reference| !receipt.search.exposed.contains(reference))
            {
                return Err(DiscoveryServiceError::ExposedCapabilityMissing);
            }
            return Ok(receipt);
        }
        let receipt = work().await?;
        self.store
            .searches()
            .complete(SearchKey(key), &receipt)
            .await?;
        Ok(receipt)
    }

    async fn route_uncached(&self, work: RouteWork<'_>) -> Result<RoutingReceipt> {
        let RouteWork {
            key,
            generation,
            provenance,
            allowed,
            exposed,
            visits,
            deferred_priorities,
        } = work;
        if !exposed.iter().all(|r| allowed.permits(r)) {
            return Err(DiscoveryServiceError::ExposedCapabilityUnauthorized);
        }
        let stage_started = std::time::Instant::now();
        let (catalogs, bindings) = self
            .store
            .load_discovery_generation(generation, allowed)
            .await?;
        metrics::record(metrics::Stage::CatalogueLoad, stage_started.elapsed());
        if !exposed.iter().all(|r| {
            catalogs
                .get(&r.catalog)
                .is_some_and(|cgs| cgs.capabilities.contains_key(r.capability.as_str()))
        }) {
            return Err(DiscoveryServiceError::ExposedCapabilityMissing);
        }
        let index_key = plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&(
            generation, allowed,
        ))?);
        let stage_started = std::time::Instant::now();
        let cached_index = self
            .store
            .search_index
            .lock()
            .map_err(|_| DiscoveryServiceError::SearchIndexPoisoned)?
            .as_ref()
            .filter(|(key, _)| *key == index_key)
            .map(|(_, index)| index.clone());
        let index = match cached_index {
            Some(index) => index,
            None => {
                let index = std::sync::Arc::new(matcher::catalogue_search::CatalogueSearch::new(
                    &catalogs, &bindings, allowed,
                )?);
                *self
                    .store
                    .search_index
                    .lock()
                    .map_err(|_| DiscoveryServiceError::SearchIndexPoisoned)? =
                    Some((index_key, index.clone()));
                index
            }
        };
        metrics::record(metrics::Stage::IndexLookupOrBuild, stage_started.elapsed());
        // Validate the pinned catalogue and authorization before durable admission.
        // Once admitted, interruption must not silently repeat provider work.
        match self.store.searches().reserve(SearchKey(key)).await? {
            SearchAdmission::Fresh => {}
            SearchAdmission::PreviouslyStarted(Some(progress)) => {
                return Err(DiscoveryServiceError::SearchInterrupted(progress));
            }
            SearchAdmission::PreviouslyStarted(None) => {
                return Err(DiscoveryServiceError::SearchAlreadyStarted);
            }
        }
        let references = self
            .guided_capabilities
            .iter()
            .filter(|r| {
                allowed.permits(r)
                    && catalogs
                        .get(&r.catalog)
                        .is_some_and(|c| c.capabilities.contains_key(r.capability.as_str()))
            })
            .cloned()
            .collect();
        let authorized = index.authorized_capabilities(allowed);
        let selection_request = provenance.selection_request();
        let operations = matcher::operations::OperationSet::from_request(&selection_request)?;
        let stage_started = std::time::Instant::now();
        let mut recall = match self.lane {
            DiscoveryLane::Ordinary => {
                let frontier = crate::discovery_store::RecallFrontier::from_state(exposed, visits);
                let ordered = ordered_recall_queries(
                    provenance.current(),
                    operations
                        .operations()
                        .iter()
                        .map(|operation| operation.text()),
                );
                let receipts = self
                    .store
                    .retrieve_many(generation, &ordered, allowed, &frontier)
                    .await?;
                crate::discovery_store::RetrievalReceipt::interleave(generation, receipts, allowed)
                    .map_err(crate::discovery_store::DiscoveryStoreError::from)?
            }
            DiscoveryLane::Supplemental => {
                index
                    .evidence(generation, &references, &catalogs, &bindings, allowed)?
                    .0
            }
        };
        for reference in exposed.iter().rev() {
            let candidate = if let Some(position) = recall
                .candidates
                .iter()
                .position(|candidate| &candidate.reference == reference)
            {
                recall.candidates.remove(position)
            } else {
                index
                    .candidate(reference)
                    .ok_or(DiscoveryServiceError::ExposedCapabilityMissing)?
                    .clone()
            };
            recall.candidates.insert(0, candidate);
        }
        metrics::record(metrics::Stage::Recall, stage_started.elapsed());
        let stage_started = std::time::Instant::now();
        let mut whole_analysis = None;
        let (retrieval, matching, mut search) = if recall.candidates.is_empty() {
            let search = matcher::online::SearchReceipt {
                policy: matcher::whole_intent::POLICY.into(),
                exposed: exposed.iter().cloned().collect(),
                unexplored: authorized
                    .iter()
                    .filter(|reference| !exposed.contains(reference))
                    .cloned()
                    .collect(),
                ..Default::default()
            };
            (
                recall,
                CapabilityMatchReceipt {
                    matches: Vec::new(),
                    budget_retained: Vec::new(),
                },
                search,
            )
        } else {
            if recall.generation != generation
                || recall.candidates.iter().any(|candidate| {
                    !allowed.permits(&candidate.reference)
                        || index
                            .candidate(&candidate.reference)
                            .is_none_or(|canonical| canonical.document != candidate.document)
                })
            {
                return Err(matcher::whole_intent::WholeIntentError::InvalidInput.into());
            }
            let nomination = matcher::semantic_nomination::SemanticNomination::new(
                &self.match_model,
                &selection_request,
                &recall,
                &catalogs,
            )?;
            let nomination_checkpoint = decision_checkpoint::DecisionCheckpoint::new(
                &self.store,
                &self.store.pending_decisions,
            );
            let nomination_dispatched = std::sync::atomic::AtomicBool::new(false);
            let nominated_raw = nomination_checkpoint
                .receive(nomination.clone(), || async {
                    nomination_dispatched.store(true, std::sync::atomic::Ordering::Relaxed);
                    let started = std::time::Instant::now();
                    let result = self
                        .contribution_response(nomination.body(), 1, "semantic-nomination-v1")
                        .await;
                    metrics::record(metrics::Stage::DecisionProvider, started.elapsed());
                    result.map_err(|error| Box::new(error) as matcher::DecisionProviderError)
                })
                .await
                .map_err(|error| matcher::DiscoveryMatcherError::Provider(error))?;
            let (mut nominated, nomination_usage) = nomination.decode(&nominated_raw)?;
            for (index, candidate) in recall.candidates.iter().enumerate() {
                if exposed.contains(&candidate.reference) {
                    nominated.insert(index);
                }
            }
            // A summary can conflate a list, search and keyed read on one entity.
            // Review the recalled read family together; nomination grants no exposure.
            let is_read = |candidate: &crate::discovery_store::RetrievedCapability| {
                matches!(
                    candidate.document.operation.kind,
                    plasm_core::schema::CapabilityKind::Get
                        | plasm_core::schema::CapabilityKind::Query
                        | plasm_core::schema::CapabilityKind::Search
                )
            };
            let read_families: std::collections::BTreeSet<_> = nominated
                .iter()
                .filter_map(|i| {
                    let candidate = &recall.candidates[*i];
                    is_read(candidate).then(|| {
                        (
                            candidate.reference.catalog.clone(),
                            candidate.document.entity.clone(),
                        )
                    })
                })
                .collect();
            for (index, candidate) in recall.candidates.iter().enumerate() {
                if is_read(candidate)
                    && read_families.contains(&(
                        candidate.reference.catalog.clone(),
                        candidate.document.entity.clone(),
                    ))
                {
                    nominated.insert(index);
                }
            }
            nomination_checkpoint
                .persist()
                .await
                .map_err(matcher::DiscoveryMatcherError::Checkpoint)?;
            if nominated.is_empty() {
                let sent = nomination_dispatched.load(std::sync::atomic::Ordering::Relaxed);
                whole_analysis = Some("**Whole-intent discovery:** no candidates were nominated for contract assessment. All recalled candidates remain unassessed; the requested outcomes are unresolved.".into());
                let search = matcher::online::SearchReceipt {
                    policy: matcher::whole_intent::POLICY.into(),
                    exposed: exposed.iter().cloned().collect(),
                    unexplored: authorized
                        .iter()
                        .filter(|r| !exposed.contains(r))
                        .cloned()
                        .collect(),
                    deferred: recall
                        .candidates
                        .iter()
                        .map(|candidate| matcher::online::DeferredCandidate {
                            reference: candidate.reference.clone(),
                            reason: matcher::online::DeferredReason::NotNominated,
                        })
                        .collect(),
                    requests: usize::from(sent),
                    input_bytes: if sent { nomination.body().len() } else { 0 },
                    reported_input_tokens: if sent {
                        nomination_usage.unwrap_or(0)
                    } else {
                        0
                    },
                    missing_usage: usize::from(sent && nomination_usage.is_none()),
                    ..Default::default()
                };
                (
                    recall,
                    CapabilityMatchReceipt {
                        matches: Vec::new(),
                        budget_retained: Vec::new(),
                    },
                    search,
                )
            } else {
                let nomination_recall = RetrievalReceipt {
                    generation: recall.generation.clone(),
                    candidates: recall
                        .candidates
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| nominated.contains(i))
                        .map(|(_, c)| c.clone())
                        .collect(),
                    lexical_count: recall.lexical_count,
                    vector_count: recall.vector_count,
                    lexical_truncated: recall.lexical_truncated,
                    vector_truncated: recall.vector_truncated,
                };
                let packet = matcher::whole_intent::WholeIntentPacket::new_with_limit(
                    &self.match_model,
                    &selection_request,
                    &nomination_recall,
                    &catalogs,
                    allowed,
                    &index,
                    &bindings,
                    deferred_priorities,
                    exposed,
                    matcher::whole_intent::MAX_BODY_BYTES.saturating_sub(nomination.body().len()),
                    false,
                )?;
                let checkpoint = decision_checkpoint::DecisionCheckpoint::new(
                    &self.store,
                    &self.store.pending_decisions,
                );
                let dispatched = std::sync::atomic::AtomicBool::new(false);
                let raw = checkpoint
                    .receive(packet.clone(), || async {
                        dispatched.store(true, std::sync::atomic::Ordering::Relaxed);
                        let started = std::time::Instant::now();
                        let result = self
                            .contribution_response(packet.body(), 1, "whole-intent-selection-v1")
                            .await;
                        metrics::record(metrics::Stage::DecisionProvider, started.elapsed());
                        result.map_err(|error| Box::new(error) as matcher::DecisionProviderError)
                    })
                    .await
                    .map_err(|error| matcher::DiscoveryMatcherError::Provider(error))?;
                let (mut selection, selected, usage) = packet.decode(&raw)?;
                let uncertain = packet.uncertain(&raw)?;
                checkpoint
                    .persist()
                    .await
                    .map_err(matcher::DiscoveryMatcherError::Checkpoint)?;
                let mut coverage_sent = false;
                let mut coverage_usage = None;
                let mut coverage_bytes = 0;
                let mut coverage_supported = false;
                if !selected.is_empty() {
                    let selected_recall = RetrievalReceipt {
                        generation: recall.generation.clone(),
                        candidates: packet
                            .candidates()
                            .iter()
                            .filter(|c| selected.contains(&c.reference))
                            .cloned()
                            .collect(),
                        lexical_count: recall.lexical_count,
                        vector_count: recall.vector_count,
                        lexical_truncated: recall.lexical_truncated,
                        vector_truncated: recall.vector_truncated,
                    };
                    let remaining = matcher::whole_intent::MAX_BODY_BYTES
                        .saturating_sub(nomination.body().len() + packet.body().len());
                    let selected_packet = matcher::whole_intent::WholeIntentPacket::new_with_limit(
                        &self.match_model,
                        &selection_request,
                        &selected_recall,
                        &catalogs,
                        allowed,
                        &index,
                        &bindings,
                        deferred_priorities,
                        exposed,
                        remaining,
                        true,
                    );
                    match selected_packet {
                        Ok(selected_packet) if selected_packet.omitted().is_empty() => {
                            let coverage =
                                matcher::whole_intent::CoveragePacket::new(&selected_packet)?;
                            if coverage.body().len() > remaining {
                                return Err(matcher::whole_intent::WholeIntentError::Budget.into());
                            }
                            let coverage_checkpoint = decision_checkpoint::DecisionCheckpoint::new(
                                &self.store,
                                &self.store.pending_decisions,
                            );
                            let dispatched = std::sync::atomic::AtomicBool::new(false);
                            let raw = coverage_checkpoint
                                .receive(coverage.clone(), || async {
                                    dispatched.store(true, std::sync::atomic::Ordering::Relaxed);
                                    let started = std::time::Instant::now();
                                    let result = self
                                        .contribution_response(
                                            coverage.body(),
                                            1,
                                            "selected-coverage-v1",
                                        )
                                        .await;
                                    metrics::record(
                                        metrics::Stage::DecisionProvider,
                                        started.elapsed(),
                                    );
                                    result.map_err(|error| {
                                        Box::new(error) as matcher::DecisionProviderError
                                    })
                                })
                                .await
                                .map_err(|error| matcher::DiscoveryMatcherError::Provider(error))?;
                            (coverage_supported, coverage_usage) = coverage.decode(&raw)?;
                            coverage_checkpoint
                                .persist()
                                .await
                                .map_err(matcher::DiscoveryMatcherError::Checkpoint)?;
                            coverage_sent = dispatched.load(std::sync::atomic::Ordering::Relaxed);
                            if coverage_sent {
                                coverage_bytes = coverage.body().len();
                            }
                        }
                        Ok(_) | Err(matcher::whole_intent::WholeIntentError::Budget) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                selection.requirements.retain(|r| {
                    matches!(
                        &r.assessment,
                        matcher::whole_intent::Assessment::Supported { .. }
                    )
                });
                if !coverage_supported {
                    selection.requirements.push(matcher::whole_intent::Requirement {
                        requirement: selection_request.clone(),
                        assessment: matcher::whole_intent::Assessment::Gap { gap: "The selected contracts do not establish every requested outcome and constraint.".into() },
                    });
                }
                let mut analysis = String::from("**Whole-intent discovery:** assessments are bounded by the presented authorized evidence.");
                for constraint in &selection.constraints {
                    analysis.push_str("\n- Constraint: ");
                    analysis.push_str(constraint);
                }
                for requirement in &selection.requirements {
                    analysis.push_str("\n- ");
                    analysis.push_str(&requirement.requirement);
                    match &requirement.assessment {
                        matcher::whole_intent::Assessment::Supported { .. } => {
                            analysis.push_str(": supported")
                        }
                        matcher::whole_intent::Assessment::Gap { gap } => {
                            analysis.push_str(": unresolved — ");
                            analysis.push_str(gap);
                        }
                    }
                }
                if !packet.omitted().is_empty() || nominated.len() < recall.candidates.len() {
                    analysis.push_str("\nSome candidates were not assessed within the evidence budget; restate or extend the intent if the unresolved outcomes need further discovery.");
                }
                whole_analysis = Some(analysis);
                let matching = CapabilityMatchReceipt {
                    matches: packet
                        .candidates()
                        .iter()
                        .filter(|candidate| selected.contains(&candidate.reference))
                        .map(|candidate| matcher::CapabilityIntentMatch {
                            capability_id: candidate.id.clone(),
                            choice: matcher::MatchChoice::Relevant,
                        })
                        .collect(),
                    budget_retained: Vec::new(),
                };
                let considered: std::collections::BTreeSet<_> = packet
                    .candidates()
                    .iter()
                    .map(|candidate| candidate.reference.clone())
                    .collect();
                let sent = dispatched.load(std::sync::atomic::Ordering::Relaxed);
                let search = matcher::online::SearchReceipt {
                    policy: matcher::whole_intent::POLICY.into(),
                    exposed: exposed.iter().cloned().collect(),
                    unexplored: authorized
                        .iter()
                        .filter(|reference| {
                            !exposed.contains(reference) && !considered.contains(*reference)
                        })
                        .cloned()
                        .collect(),
                    considered,
                    deferred: uncertain
                        .iter()
                        .cloned()
                        .map(|reference| matcher::online::DeferredCandidate {
                            reference,
                            reason: matcher::online::DeferredReason::OperationUncertain,
                        })
                        .chain(
                            recall
                                .candidates
                                .iter()
                                .enumerate()
                                .filter(|(i, _)| !nominated.contains(i))
                                .map(|(_, candidate)| matcher::online::DeferredCandidate {
                                    reference: candidate.reference.clone(),
                                    reason: matcher::online::DeferredReason::NotNominated,
                                })
                                .chain(packet.omitted().iter().cloned().map(|reference| {
                                    let reason = if packet.was_projected(&reference) {
                                        matcher::online::DeferredReason::InputBytes
                                    } else {
                                        matcher::online::DeferredReason::ProjectionLimit
                                    };
                                    matcher::online::DeferredCandidate { reference, reason }
                                })),
                        )
                        .collect(),
                    requests: usize::from(sent)
                        + usize::from(
                            nomination_dispatched.load(std::sync::atomic::Ordering::Relaxed),
                        )
                        + usize::from(coverage_sent),
                    input_bytes: if sent { packet.body().len() } else { 0 }
                        + if nomination_dispatched.load(std::sync::atomic::Ordering::Relaxed) {
                            nomination.body().len()
                        } else {
                            0
                        }
                        + coverage_bytes,
                    reported_input_tokens: if sent { usage.unwrap_or(0) } else { 0 }
                        + if nomination_dispatched.load(std::sync::atomic::Ordering::Relaxed) {
                            nomination_usage.unwrap_or(0)
                        } else {
                            0
                        }
                        + if coverage_sent {
                            coverage_usage.unwrap_or(0)
                        } else {
                            0
                        },
                    missing_usage: usize::from(sent && usage.is_none())
                        + usize::from(
                            nomination_dispatched.load(std::sync::atomic::Ordering::Relaxed)
                                && nomination_usage.is_none(),
                        )
                        + usize::from(coverage_sent && coverage_usage.is_none()),
                };
                (recall, matching, search)
            }
        };
        metrics::record(
            metrics::Stage::SelectionIncludingProviderAndCheckpoints,
            stage_started.elapsed(),
        );
        for reference in &self.guided_capabilities {
            let candidate = retrieval
                .candidates
                .iter()
                .find(|candidate| &candidate.reference == reference);
            let decision = candidate.and_then(|candidate| {
                matching
                    .matches
                    .iter()
                    .find(|item| item.capability_id == candidate.id)
            });
            tracing::debug!(target: "plasm_agent::workflow_learning", catalog = %reference.catalog, capability = %reference.capability, already_exposed = exposed.contains(reference), retrieved = candidate.is_some(), considered = search.considered.contains(reference), deferred = ?search.deferred.iter().find(|item| &item.reference == reference).map(|item| &item.reason), decision = ?decision.map(|item| &item.choice), "supplemental capability discovery decision");
        }
        let business = matching.selected(&retrieval);
        let closure = if !business.is_empty() || !exposed.is_empty() {
            let references = catalogs.iter().map(|(id, cgs)| (id.clone(), cgs)).collect();
            Some(prerequisite_closure(
                &references,
                &bindings,
                &business,
                &allowed.catalogs,
            )?)
        } else {
            None
        };
        validate_closure_authorization(closure.as_ref(), allowed)?;
        if let Some(closure) = &closure {
            for reference in closure
                .business
                .iter()
                .chain(&closure.input_sources)
                .chain(&closure.prerequisites)
            {
                search.considered.insert(reference.clone());
                search.unexplored.remove(reference);
            }
            search
                .deferred
                .retain(|candidate| !search.considered.contains(&candidate.reference));
        }
        // No additions to an existing surface is not an unmatched workflow.
        let recovery = if business.is_empty()
            && exposed.is_empty()
            && search.deferred.is_empty()
            && whole_analysis.is_none()
        {
            Some(DiscoveryRecovery::from_unmatched(
                &matching,
                &retrieval,
                &catalogs,
                allowed.catalogs.iter(),
            ))
        } else {
            None
        };
        let intent_analysis = whole_analysis.unwrap_or_else(||
            "**Whole-intent discovery:** no authorized candidate evidence was retrieved; the requested outcomes remain unresolved.".to_owned()
        );
        Ok(RoutingReceipt {
            intent_provenance: provenance.clone(),
            authorization: allowed.clone(),
            intent_analysis,
            intent: provenance.current().to_owned(),
            pin_id: String::new(),
            retrieval,
            matching,
            search,
            closure,
            recovery,
        })
    }

    async fn contribution_response(
        &self,
        body: &str,
        search_attempt: usize,
        contract: &str,
    ) -> std::result::Result<String, plasm_runtime::provider_transport::ProviderTransportError>
    {
        plasm_runtime::provider_transport::request_attempt(
            self.transport.as_ref(),
            body,
            search_attempt,
            std::time::Duration::from_secs(30),
            |attempt| {
                if let Some(directory) = &self.rejection_dir {
                    let mut record = serde_json::to_value(attempt)?;
                    record["attempt"] = search_attempt.into();
                    record["contract"] = contract.into();
                    record["request_body"] = body.into();
                    save_rejection(directory, &record)?;
                }
                Ok(())
            },
        )
        .await
    }
}

fn discovery_intent_analysis(
    search: &matcher::online::SearchReceipt,
    uncovered: &[&str],
    has_contextual_fragments: bool,
) -> String {
    let mut analysis = discovery_assessment_summary(search);
    if has_contextual_fragments {
        analysis.push_str("\n\nSome request fragments were assessed as context rather than independent requirements; their coverage remains unverified.");
    }
    if !uncovered.is_empty() {
        analysis.push_str("\n\nThese request fragments have no affirmative independent coverage assessment (up to four shown). Fragments are mechanical text ranges, not interpreted requirements. Earlier declarations remain available; this does not establish that a capability is unavailable:");
        for span in uncovered {
            analysis.push_str("\n- ");
            analysis.push_str(span);
        }
    }
    if !uncovered.is_empty() || has_contextual_fragments {
        analysis.push_str("\n\nExtend the same logical session with the remaining discovery need to assess more candidates.");
    }
    analysis
}

fn discovery_assessment_summary(search: &matcher::online::SearchReceipt) -> String {
    let mut summary = if search.has_unavailable_provider() {
        "**Capability discovery:** a later assessment was unavailable; earlier verified contributions remain available."
    } else if search.has_pending_operation_judgments() {
        "**Capability discovery:** additional candidates remain unassessed. Use declared operations if they satisfy the task, or extend the session to assess more."
    } else if search.has_incomplete_operation_judgments() {
        "**Capability discovery:** some candidate contributions remain uncertain."
    } else {
        "**Capability contribution** (bounded online search; unvisited capabilities remain unexplored):"
    }.to_owned();
    if search
        .deferred
        .iter()
        .any(|item| matches!(item.reason, matcher::online::DeferredReason::InputBytes))
    {
        summary.push_str("\n\nSome candidates did not fit within this search's packet or total-byte budget; their relevance remains unknown. A later search can assess them if their complete evidence fits.");
    }
    summary
}

fn save_rejection(
    directory: &std::path::Path,
    record: &serde_json::Value,
) -> std::result::Result<(), plasm_runtime::provider_transport::ProviderRecordError> {
    use std::io::Write;

    std::fs::create_dir_all(directory)?;
    let path = directory.join(format!("discovery-decision-{}.json", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&serde_json::to_vec_pretty(record)?)?;
    file.sync_all()?;
    Ok(())
}

fn validate_closure_authorization(
    closure: Option<&PrerequisiteClosure>,
    allowed: &DiscoveryAuthorization,
) -> Result<()> {
    if let Some(closure) = closure {
        if closure
            .business
            .iter()
            .chain(&closure.input_sources)
            .chain(&closure.prerequisites)
            .any(|capability| !allowed.permits(capability))
        {
            return Err(DiscoveryServiceError::UnauthorizedClosure);
        }
    }
    Ok(())
}

#[cfg(test)]
mod concurrency_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn partial_discovery_keeps_uncertainty_without_integration_inventory() {
        let packet: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/discovery/partial-routing.json"
        ))
        .unwrap();
        let receipt: RoutingReceipt = serde_json::from_value(packet["routing"].clone()).unwrap();
        let text = discovery_intent_analysis(&receipt.search, &["Find owned records"], true);
        assert!(text.contains("Find owned records"));
        assert!(text.contains("coverage remains unverified"));
        assert!(text.contains("same logical session"));
        assert!(!text.contains("Available integrations"));
    }

    #[test]
    fn assessment_summary_reports_packet_deferral_without_claiming_relevance() {
        let mut search = matcher::online::SearchReceipt::default();
        assert!(!discovery_assessment_summary(&search).contains("total-byte budget"));
        search.deferred.push(matcher::online::DeferredCandidate {
            reference: CapabilityRef {
                catalog: "matrix".into(),
                capability: "candidate".into(),
            },
            reason: matcher::online::DeferredReason::InputBytes,
        });
        let summary = discovery_assessment_summary(&search);
        assert!(summary.contains("did not fit within this search's packet or total-byte budget"));
        assert!(summary.contains("relevance remains unknown"));
        assert!(summary.contains("if their complete evidence fits"));
    }

    #[test]
    fn recall_prioritizes_current_spans_without_dropping_history() {
        let current = "Read the device. Inspect its owner.";
        let ordered = ordered_recall_queries(
            current,
            [
                "Turn 1: Archive the record.",
                "Turn 2: Read the device.",
                "Inspect its owner.",
                "Turn 1: Archive the record.",
                current,
            ],
        );
        assert_eq!(
            ordered,
            vec![
                current,
                "Turn 2: Read the device.",
                "Inspect its owner.",
                "Turn 1: Archive the record."
            ]
        );
    }

    #[test]
    fn nominated_guidance_requires_individual_discovery_judgment() {
        let packet: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/discovery/partial-routing.json"
        ))
        .unwrap();
        let mut routing: RoutingReceipt =
            serde_json::from_value(packet["routing"].clone()).unwrap();
        routing.retrieval.candidates.truncate(1);
        let candidate = &routing.retrieval.candidates[0];
        let mut decision = CapabilityMatchReceipt {
            matches: vec![matcher::CapabilityIntentMatch {
                capability_id: candidate.id.clone(),
                choice: matcher::MatchChoice::Unrelated,
            }],
            budget_retained: Vec::new(),
        };
        assert!(decision.selected(&routing.retrieval).is_empty());
        decision.matches[0].choice = matcher::MatchChoice::Relevant;
        assert_eq!(
            decision.selected(&routing.retrieval),
            vec![candidate.reference.clone()]
        );
    }

    #[test]
    fn retain_retrieved_candidates_does_not_bypass_authorization() {
        use sha2::{Digest, Sha256};
        let packet: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/discovery/partial-routing.json"
        ))
        .unwrap();
        let mut receipt: RoutingReceipt =
            serde_json::from_value(packet["routing"].clone()).unwrap();
        // The prose fixture uses placeholder digests; construct valid admission evidence.
        for candidate in &mut receipt.retrieval.candidates {
            candidate.document.text_hash =
                hex::encode(Sha256::digest(candidate.document.text.as_bytes()));
        }
        let original = serde_json::to_value(&receipt.retrieval).unwrap();
        receipt.retrieval.validate(&receipt.authorization).unwrap();
        let all: Vec<_> = receipt
            .retrieval
            .candidates
            .iter()
            .map(|c| c.reference.clone())
            .collect();
        assert_eq!(all.len(), receipt.retrieval.candidates.len());
        for reference in &all {
            for deny_catalog in [false, true] {
                let mut policy = receipt.authorization.clone();
                if deny_catalog {
                    policy.catalogs.remove(&reference.catalog);
                } else {
                    let retained = all
                        .iter()
                        .filter(|r| r.catalog == reference.catalog && *r != reference)
                        .map(|r| r.capability.clone())
                        .collect();
                    policy
                        .capabilities
                        .insert(reference.catalog.clone(), retained);
                }
                assert!(matches!(
                    receipt.retrieval.validate(&policy),
                    Err(crate::discovery_store::RetrievalValidationError::UnauthorizedCandidate)
                ));
                for lane in [0, 1, 2] {
                    let mut closure = PrerequisiteClosure {
                        business: vec![],
                        input_sources: vec![],
                        prerequisites: vec![],
                        acquisitions: vec![],
                        edges: vec![],
                    };
                    match lane {
                        0 => closure.business.push(reference.clone()),
                        1 => closure.input_sources.push(reference.clone()),
                        _ => closure.prerequisites.push(reference.clone()),
                    }
                    assert!(matches!(
                        validate_closure_authorization(Some(&closure), &policy),
                        Err(DiscoveryServiceError::UnauthorizedClosure)
                    ));
                }
            }
        }
        // Candidate retention does not mutate the authenticated policy or contract evidence.
        assert_eq!(serde_json::to_value(&receipt.retrieval).unwrap(), original);
    }

    #[test]
    fn contribution_receipt_preserves_contracts_and_rejects_support_authority() {
        let packet: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/discovery/partial-routing.json"
        ))
        .unwrap();
        let original = packet["routing"].clone();
        let receipt: RoutingReceipt = serde_json::from_value(original.clone()).unwrap();
        let wire = serde_json::to_value(&receipt).unwrap();
        assert_eq!(wire["retrieval"], original["retrieval"]);
        assert_eq!(wire["closure"], original["closure"]);
        assert_eq!(wire["authorization"], original["authorization"]);
        assert!(wire.get("environment_support").is_none());
        for injected in [
            serde_json::Value::Null,
            serde_json::json!({"choice": "established", "request_hash": "abstract"}),
        ] {
            let mut invalid = wire.clone();
            invalid["environment_support"] = injected;
            assert!(serde_json::from_value::<RoutingReceipt>(invalid).is_err());
        }
        for lane in ["business", "input_sources", "prerequisites"] {
            let mut closure: PrerequisiteClosure =
                serde_json::from_value(wire["closure"].clone()).unwrap();
            let denied = CapabilityRef {
                catalog: "denied".into(),
                capability: "write".into(),
            };
            match lane {
                "business" => closure.business.push(denied),
                "input_sources" => closure.input_sources.push(denied),
                _ => closure.prerequisites.push(denied),
            }
            assert!(
                validate_closure_authorization(Some(&closure), &receipt.authorization).is_err()
            );
        }
    }

    #[test]
    fn unauthorized_declared_prerequisite_blocks_exposure() {
        let read = CapabilityRef {
            catalog: "matrix".into(),
            capability: "read".into(),
        };
        let acquire = CapabilityRef {
            catalog: "matrix".into(),
            capability: "acquire".into(),
        };
        let closure = PrerequisiteClosure {
            business: vec![read],
            input_sources: vec![],
            prerequisites: vec![acquire],
            acquisitions: vec![],
            edges: vec![],
        };
        let mut authorization = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        authorization
            .capabilities
            .insert("matrix".into(), BTreeSet::from(["read".into()]));
        assert!(validate_closure_authorization(Some(&closure), &authorization).is_err());
    }
}
