//! Direct, bounded capability discovery: deterministic recall followed by Luna relevance matching.

use crate::discovery_scheduling as scheduling;
pub(crate) mod decision_checkpoint;
mod metrics;
use plasm_core::prerequisites::{
    CapabilityRef, PrerequisiteClosure, PrerequisiteError, prerequisite_closure,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::discovery_matcher::{self as matcher, CapabilityMatchReceipt};
use crate::discovery_store::{
    BacklogKey, DiscoveryAuthorization, DiscoverySessionPin, DiscoveryStore, FrontierKey,
    RetrievalReceipt, SearchAdmission, SearchKey,
};

pub use crate::discovery_recovery::{CatalogAppDescription, DiscoveryRecovery, RECOVERY_GUIDANCE};

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
                matches!(item.reason, matcher::online::DeferredReason::InputBytes)
                    && receipt.authorization.permits(&item.reference)
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
            matcher::online::SEARCH_POLICY,
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
    operation_state: OperationFrontier,
    active_request: Option<String>,
}

/// A session frontier never treats absent extraction as an empty operation set.
/// The prepared state is scoped and revalidated by `OperationMatrix::restore`.
#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(tag = "state", content = "checkpoint", rename_all = "snake_case")]
pub(crate) enum OperationFrontier {
    #[default]
    AwaitingExtraction,
    Prepared(Box<matcher::operations::MatrixCheckpoint>),
}

/// Extraction failures leave the previous revision intact. A changed request
/// requires new semantic proposals; it never inherits prior operation answers.
enum OperationFrontierLoad {
    AwaitingExtraction,
    ChangedRevision,
    Resume(Box<matcher::operations::OperationMatrix>),
}

impl OperationFrontier {
    fn answered_pairs(
        &self,
    ) -> std::collections::BTreeSet<(matcher::operations::OperationId, CapabilityRef)> {
        match self {
            Self::AwaitingExtraction => Default::default(),
            Self::Prepared(checkpoint) => checkpoint.answered_pairs(),
        }
    }

    fn load(
        &self,
        scope: &matcher::operations::MatrixScope,
        intent: &str,
        authorized: &std::collections::BTreeSet<CapabilityRef>,
    ) -> std::result::Result<OperationFrontierLoad, matcher::operations::OperationError> {
        match self {
            Self::AwaitingExtraction => Ok(OperationFrontierLoad::AwaitingExtraction),
            Self::Prepared(checkpoint) => {
                if !checkpoint.scope().same_host_scope(scope) {
                    return Err(matcher::operations::OperationError::ScopeMismatch);
                }
                if checkpoint.scope() != scope {
                    return Ok(OperationFrontierLoad::ChangedRevision);
                }
                Ok(OperationFrontierLoad::Resume(Box::new(
                    matcher::operations::OperationMatrix::restore(
                        scope,
                        intent,
                        authorized,
                        checkpoint.as_ref().clone(),
                    )?,
                )))
            }
        }
    }

    fn commit(
        &mut self,
        scope: &matcher::operations::MatrixScope,
        matrix: &matcher::operations::OperationMatrix,
    ) -> std::result::Result<(), matcher::operations::OperationError> {
        let checkpoint = matrix.checkpoint();
        if checkpoint.scope() != scope {
            return Err(matcher::operations::OperationError::ScopeMismatch);
        }
        *self = Self::Prepared(Box::new(checkpoint));
        Ok(())
    }
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

    fn record(
        &mut self,
        key: &str,
        receipt: &RoutingReceipt,
        answered: &std::collections::BTreeSet<CapabilityRef>,
    ) {
        self.completed.entry(key.to_owned()).or_insert_with(|| {
            let mut visited: std::collections::BTreeSet<_> = receipt
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
                .collect();
            visited.extend(
                answered
                    .iter()
                    .filter(|reference| receipt.authorization.permits(reference))
                    .cloned(),
            );
            visited
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
            matcher::online::SEARCH_POLICY,
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
            matcher::online::SEARCH_POLICY,
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
    #[error(transparent)]
    OperationExtraction(#[from] matcher::operations::ExtractionError),
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
    session: &'a str,
    generation: &'a str,
    provenance: &'a crate::intent_provenance::IntentProvenance,
    allowed: &'a DiscoveryAuthorization,
    exposed: &'a [CapabilityRef],
    visits: &'a std::collections::BTreeMap<CapabilityRef, usize>,
    deferred_priorities: &'a std::collections::BTreeMap<CapabilityRef, usize>,
    operation_state: OperationFrontier,
}

pub struct DiscoveryService {
    store: DiscoveryStore,
    transport: std::sync::Arc<dyn plasm_runtime::provider_transport::ProviderTransport>,
    match_model: String,
    operation_model: String,
    operation_extractor: std::sync::Arc<dyn matcher::operations::OperationExtractor>,
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
                    matcher::RELEVANCE_ENDPOINT,
                    "no-provider-calls-permitted",
                ),
            ),
            match_model: matcher::RELEVANCE_MODEL.into(),
            operation_model: "openai/gpt-6-luna".into(),
            operation_extractor: std::sync::Arc::new(
                matcher::operations::ProviderOperationExtractor::new(
                    "openai/gpt-6-luna",
                    std::sync::Arc::new(
                        plasm_runtime::provider_transport::ReqwestProviderTransport::new(
                            reqwest::Client::builder()
                                .proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap())
                                .build()
                                .unwrap(),
                            "https://openrouter.ai/api/v1/chat/completions",
                            "no-provider-calls-permitted",
                        ),
                    ),
                ),
            ),
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
        let operation_model = std::env::var("PLASM_DISCOVERY_OPERATION_MODEL")
            .unwrap_or_else(|_| "openai/gpt-6-luna".into());
        let operation_extractor =
            std::sync::Arc::new(matcher::operations::ProviderOperationExtractor::new(
                operation_model.clone(),
                std::sync::Arc::new(scheduling::ScheduledProviderTransport::new(
                    std::sync::Arc::new(
                        plasm_runtime::provider_transport::ReqwestProviderTransport::new(
                            reqwest::Client::builder()
                                .timeout(std::time::Duration::from_secs(45))
                                .build()?,
                            "https://openrouter.ai/api/v1/chat/completions",
                            api_key.clone(),
                        ),
                    ),
                    scheduling::Workload::OperationExtraction,
                )),
            ));
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
                        matcher::RELEVANCE_ENDPOINT,
                        api_key,
                    ),
                ),
                scheduling::Workload::DiscoveryDecision,
            )),
            match_model: std::env::var("PLASM_DISCOVERY_CAPABILITY_MATCH_MODEL")
                .unwrap_or_else(|_| matcher::RELEVANCE_MODEL.into()),
            operation_model,
            operation_extractor,
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
        let policy_models = format!("{}:{}", self.match_model, self.operation_model);
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
                |visits, operation_state, effective_key| {
                    let generation = &generation;
                    let session = &pin_id;
                    async move {
                        self.route_uncached(RouteWork {
                            key: &effective_key,
                            session,
                            generation,
                            provenance: request.intent_provenance,
                            allowed: request.allowed,
                            exposed: request.exposed,
                            visits: &visits,
                            deferred_priorities: &deferred_priorities,
                            operation_state,
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
        let policy_models = format!("{}:{}", self.match_model, self.operation_model);
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
                session: &key,
                generation,
                provenance,
                allowed,
                exposed,
                visits: &Default::default(),
                deferred_priorities: &Default::default(),
                operation_state: OperationFrontier::default(),
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
        let policy_models = format!("{}:{}", self.match_model, self.operation_model);
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
            |visits, operation_state, effective_key| async move {
                self.route_uncached(RouteWork {
                    key: &effective_key,
                    session,
                    generation,
                    provenance,
                    allowed,
                    exposed,
                    visits: &visits,
                    deferred_priorities: &Default::default(),
                    operation_state,
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
        F: FnOnce(
            std::collections::BTreeMap<CapabilityRef, usize>,
            OperationFrontier,
            String,
        ) -> Fut,
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
                let prior_pairs = frontier.operation_state.answered_pairs();
                let historical_replay = frontier.active_request.as_deref() != Some(key)
                    && frontier.completed.contains_key(key);
                let effective_key = if frontier.active_request.as_deref() == Some(key)
                    && matches!(&frontier.operation_state, OperationFrontier::Prepared(_))
                {
                    plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&(
                        "operation-continuation-v1",
                        key,
                        &frontier.operation_state,
                    ))?)
                } else {
                    key.to_owned()
                };
                let receipt = self
                    .route_owned(&effective_key, exposed, || {
                        work(
                            visits,
                            frontier.operation_state.clone(),
                            effective_key.clone(),
                        )
                    })
                    .await?;
                if !historical_replay && !frontier.completed.contains_key(&effective_key) {
                    if let Some(saved) = self
                        .store
                        .searches()
                        .operations(SearchKey(&effective_key))
                        .await?
                    {
                        frontier.operation_state = saved;
                    }
                }
                if !historical_replay {
                    frontier.active_request = Some(key.to_owned());
                }
                let answered = frontier
                    .operation_state
                    .answered_pairs()
                    .difference(&prior_pairs)
                    .map(|(_, capability)| capability.clone())
                    .collect();
                frontier.record(&effective_key, &receipt, &answered);
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
            session,
            generation,
            provenance,
            allowed,
            exposed,
            visits,
            deferred_priorities,
            mut operation_state,
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
        let extraction =
            matcher::operations::ExtractionRequest::new(&self.operation_model, &selection_request)?;
        let scope = matcher::operations::MatrixScope::new(
            session,
            generation,
            &authorized,
            extraction.identity(),
        )?;
        let mut matrix = match operation_state.load(&scope, &selection_request, &authorized)? {
            OperationFrontierLoad::Resume(matrix) => *matrix,
            OperationFrontierLoad::AwaitingExtraction | OperationFrontierLoad::ChangedRevision => {
                let started = std::time::Instant::now();
                let operations = self.operation_extractor.extract(&selection_request).await;
                metrics::record_extraction(started.elapsed(), operations.is_ok());
                matcher::operations::OperationMatrix::new(
                    scope.clone(),
                    &selection_request,
                    operations?,
                    &authorized,
                    Vec::new(),
                )?
            }
        };
        matrix.prioritize_current_request(provenance.current());
        let stage_started = std::time::Instant::now();
        let recall = match self.lane {
            DiscoveryLane::Ordinary => {
                let frontier = crate::discovery_store::RecallFrontier::from_state(exposed, visits);
                let ordered = ordered_recall_queries(
                    provenance.current(),
                    matrix
                        .operations()
                        .operations()
                        .iter()
                        .map(|operation| operation.text()),
                );
                let receipts = self
                    .store
                    .retrieve_many(generation, &ordered, allowed, &frontier)
                    .await?;
                let mut ranks = std::collections::BTreeMap::new();
                for operation in matrix.operations().operations() {
                    let index = ordered
                        .iter()
                        .position(|query| *query == operation.text())
                        .expect("each operation has a retrieval query");
                    for (rank, candidate) in receipts[index].candidates.iter().enumerate() {
                        ranks.insert((operation.id().clone(), candidate.reference.clone()), rank);
                    }
                }
                matrix.set_recall_ranks(&authorized, ranks)?;
                crate::discovery_store::RetrievalReceipt::interleave(generation, receipts, allowed)
                    .map_err(crate::discovery_store::DiscoveryStoreError::from)?
            }
            DiscoveryLane::Supplemental => {
                index
                    .evidence(generation, &references, &catalogs, &bindings, allowed)?
                    .0
            }
        };
        metrics::record(metrics::Stage::Recall, stage_started.elapsed());
        let stage_started = std::time::Instant::now();
        let (retrieval, matching, search) = if recall.candidates.is_empty() {
            let search = matcher::online::SearchReceipt {
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
            let checkpoint = decision_checkpoint::DecisionCheckpoint::new(
                &self.store,
                &self.store.pending_decisions,
            );
            let mut attempt = 0;
            let result = matcher::online::select_operations_indexed(
                matcher::online::SearchContext {
                    visits,
                    model: &self.match_model,
                    intent: provenance,
                    generation,
                    catalogs: &catalogs,
                    deployments: &bindings,
                    allowed,
                    exposed,
                },
                &index,
                &recall,
                &mut matrix,
                deferred_priorities,
                |issued| {
                    attempt += 1;
                    let attempt = attempt;
                    let checkpoint = &checkpoint;
                    async move {
                        checkpoint
                            .receive(issued.clone(), || async {
                                let provider_started = std::time::Instant::now();
                                let result =
                                    self.contribution_response(issued.body(), attempt).await;
                                metrics::record(
                                    metrics::Stage::DecisionProvider,
                                    provider_started.elapsed(),
                                );
                                result.map_err(|error| {
                                    Box::new(error) as matcher::DecisionProviderError
                                })
                            })
                            .await
                    }
                },
                |progress| {
                    let checkpoint = &checkpoint;
                    async move {
                        checkpoint.persist().await?;
                        self.store
                            .searches()
                            .checkpoint(SearchKey(key), &progress)
                            .await?;
                        Ok(())
                    }
                },
            )
            .await?;
            operation_state.commit(&scope, &matrix)?;
            self.store
                .searches()
                .checkpoint_operations(SearchKey(key), &operation_state)
                .await?;
            result
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
        // No additions to an existing surface is not an unmatched workflow.
        let recovery = if business.is_empty() && exposed.is_empty() && search.deferred.is_empty() {
            Some(DiscoveryRecovery::from_unmatched(
                &matching,
                &retrieval,
                &catalogs,
                allowed.catalogs.iter(),
            ))
        } else {
            None
        };
        let mut intent_analysis = if search.has_unavailable_provider() {
            "**Capability discovery:** a later assessment was unavailable; earlier verified contributions remain available."
        } else if search.has_pending_operation_judgments() {
            "**Capability discovery:** additional candidates remain unassessed. Use declared operations if they satisfy the task, or extend the session to assess more."
        } else if search.has_incomplete_operation_judgments() {
            "**Capability discovery:** some candidate contributions remain uncertain."
        } else {
            "**Capability contribution** (bounded online search; unvisited capabilities remain unexplored):"
        }.to_owned();
        let uncovered: Vec<_> = matrix.uncovered_operations().take(4).collect();
        if !uncovered.is_empty() {
            intent_analysis.push_str("\n\nThis discovery pass established no candidate contribution for these extracted request spans (up to four shown). Earlier declarations remain available; this does not establish that a capability is unavailable:");
            for span in uncovered {
                intent_analysis.push_str("\n- ");
                intent_analysis.push_str(span);
            }
            let partial_recovery = DiscoveryRecovery::from_unmatched(
                &matching,
                &retrieval,
                &catalogs,
                allowed.catalogs.iter(),
            );
            intent_analysis.push_str("\n\n");
            intent_analysis.push_str(&partial_recovery.render_partial_catalogs());
        }
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
    ) -> std::result::Result<String, plasm_runtime::provider_transport::ProviderTransportError>
    {
        plasm_runtime::provider_transport::request_attempt(
            self.transport.as_ref(),
            body,
            search_attempt,
            plasm_runtime::provider_transport::ProviderRetryPolicy::discovery_attempt()
                .attempt_timeout,
            |attempt| {
                if let Some(directory) = &self.rejection_dir {
                    let mut record = serde_json::to_value(attempt)?;
                    record["attempt"] = search_attempt.into();
                    record["contract"] = "decisions-shared-contribution-v6".into();
                    record["request_body"] = body.into();
                    save_rejection(directory, &record)?;
                }
                Ok(())
            },
        )
        .await
    }
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
