//! Direct, bounded capability discovery: deterministic recall followed by Luna relevance matching.

use crate::discovery_scheduling as scheduling;
use plasm_core::prerequisites::{
    prerequisite_closure, CapabilityRef, PrerequisiteClosure, PrerequisiteError,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::discovery_matcher::{self as matcher, CapabilityMatchReceipt};
use crate::discovery_store::{
    DiscoveryAuthorization, DiscoverySessionPin, DiscoveryStore, RetrievalReceipt,
};

pub use crate::discovery_recovery::{CatalogAppDescription, DiscoveryRecovery, RECOVERY_GUIDANCE};

type Result<T> = std::result::Result<T, DiscoveryServiceError>;

/// Completed searches advance priority once within a self-contained request.
/// No negative relevance decisions are applied to exposure or permissions.
#[derive(Default, Serialize, Deserialize)]
struct SearchFrontier {
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
            matcher::online::SEARCH_POLICY,
            pin,
            generation,
            allowed,
            model,
            current,
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
    frontier_key("diagnostic-pin", "frozen", allowed, "fixture-model", current).unwrap()
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

pub struct DiscoveryService {
    store: DiscoveryStore,
    transport: std::sync::Arc<dyn plasm_runtime::provider_transport::ProviderTransport>,
    match_model: String,
    rejection_dir: Option<std::path::PathBuf>,
    guided_capabilities: std::collections::BTreeSet<CapabilityRef>,
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
            rejection_dir: None,
            guided_capabilities: Default::default(),
        }
    }
    pub fn from_env(store: DiscoveryStore) -> Result<Self> {
        let api_key =
            std::env::var("OPENROUTER_API_KEY").map_err(DiscoveryServiceError::MissingApiKey)?;
        if api_key.trim().is_empty() {
            return Err(DiscoveryServiceError::EmptyApiKey);
        }
        Ok(Self {
            guided_capabilities: Default::default(),
            rejection_dir: std::env::var_os("PLASM_DISCOVERY_REJECTION_DIR")
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from),
            store,
            transport: std::sync::Arc::new(scheduling::ScheduledProviderTransport(
                std::sync::Arc::new(
                    plasm_runtime::provider_transport::ReqwestProviderTransport::new(
                        reqwest::Client::builder()
                            .timeout(std::time::Duration::from_secs(300))
                            .build()?,
                        matcher::RELEVANCE_ENDPOINT,
                        api_key,
                    ),
                ),
            )),
            match_model: std::env::var("PLASM_DISCOVERY_CAPABILITY_MATCH_MODEL")
                .unwrap_or_else(|_| matcher::RELEVANCE_MODEL.into()),
        })
    }

    /// Nominate reviewed catalogue dependencies for ordinary authorized admission.
    pub fn with_catalogue_guidance(
        mut self,
        capabilities: impl IntoIterator<Item = CapabilityRef>,
    ) -> Self {
        self.guided_capabilities = capabilities.into_iter().take(32).collect();
        self
    }

    /// Shared new/extend state machine; hosts supply authenticated scope and expiry.
    pub async fn route_turn(&self, request: RouteTurn<'_>) -> Result<RoutingReceipt> {
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
        let frontier_key = frontier_key(
            &pin_id,
            &generation,
            request.allowed,
            &self.match_model,
            request.intent_provenance.current(),
        )?;
        let search = search_key(
            &generation,
            request.intent_provenance,
            request.allowed,
            &self.match_model,
            request.exposed,
        )?;
        let key = if self.guided_capabilities.is_empty() {
            plasm_core::catalog_discovery::content_hash(
                format!("{frontier_key}:{search}").as_bytes(),
            )
        } else {
            plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&(
                &frontier_key,
                &search,
                &self.guided_capabilities,
            ))?)
        };
        let mut receipt = self
            .route_frontier(&frontier_key, &key, request.exposed, |visits| {
                let key = &key;
                let generation = &generation;
                async move {
                    self.route_uncached(
                        key,
                        generation,
                        request.intent_provenance,
                        request.allowed,
                        request.exposed,
                        &visits,
                    )
                    .await
                }
            })
            .await?;
        receipt.pin_id = pin_id;
        self.store
            .commit_discovery_progress(
                &receipt.pin_id,
                request.intent_provenance,
                request.logical_session.is_some(),
            )
            .await?;
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
        let base = search_key(generation, provenance, allowed, &self.match_model, exposed)?;
        let key = if self.guided_capabilities.is_empty() {
            base
        } else {
            plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&(
                base,
                &self.guided_capabilities,
            ))?)
        };
        self.route_cached(&key, exposed, || async {
            self.route_uncached(
                &key,
                generation,
                provenance,
                allowed,
                exposed,
                &Default::default(),
            )
            .await
        })
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
        F: FnOnce(std::collections::BTreeMap<CapabilityRef, usize>) -> Fut,
        Fut: std::future::Future<Output = Result<RoutingReceipt>>,
    {
        // One request-frontier owner, not nested database leases. A duplicate completion
        // can repair a failed frontier write without repeating provider work.
        let _same_request = self.store.pending_decisions.claim(frontier_key).await;
        self.store
            .with_selector_search(frontier_key, || async {
                let mut frontier: SearchFrontier =
                    match self.store.cached_selector_envelope(frontier_key).await? {
                        Some(saved) => serde_json::from_str(&saved)?,
                        None => SearchFrontier::default(),
                    };
                let visits = frontier.visits();
                let receipt = self.route_owned(key, exposed, || work(visits)).await?;
                frontier.record(key, &receipt);
                self.store
                    .store_search_progress(frontier_key, &serde_json::to_string(&frontier)?)
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
        let _same_search = self.store.pending_decisions.claim(key).await;
        self.store
            .with_selector_search(key, || async { self.route_owned(key, exposed, work).await })
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
        if let Some(saved) = self.store.cached_selector_envelope(key).await? {
            let receipt: RoutingReceipt = serde_json::from_str(&saved)?;
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
            .store_selector_envelope(key, &serde_json::to_string(&receipt)?)
            .await?;
        Ok(receipt)
    }

    async fn route_uncached(
        &self,
        key: &str,
        generation: &str,
        provenance: &crate::intent_provenance::IntentProvenance,
        allowed: &DiscoveryAuthorization,
        exposed: &[CapabilityRef],
        visits: &std::collections::BTreeMap<CapabilityRef, usize>,
    ) -> Result<RoutingReceipt> {
        if !exposed.iter().all(|r| allowed.permits(r)) {
            return Err(DiscoveryServiceError::ExposedCapabilityUnauthorized);
        }
        let (catalogs, bindings) = self
            .store
            .load_discovery_generation(generation, allowed)
            .await?;
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
        // Validate the pinned catalogue and authorization before durable admission.
        // Once admitted, interruption must not silently repeat provider work.
        if !self
            .store
            .reserve_selector_search(&format!("{key}:started"))
            .await?
        {
            if let Some(saved) = self
                .store
                .cached_selector_envelope(&format!("{key}:progress"))
                .await?
            {
                return Err(DiscoveryServiceError::SearchInterrupted(Box::new(
                    serde_json::from_str(&saved)?,
                )));
            }
            return Err(DiscoveryServiceError::SearchAlreadyStarted);
        }
        let progress_key = format!("{key}:progress");
        let mut recall = self
            .store
            .retrieve(
                generation,
                provenance.current(),
                allowed,
                &crate::discovery_store::RecallFrontier::from_state(exposed, visits),
            )
            .await?;
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
        if !self.guided_capabilities.is_empty() {
            let (examples, _) =
                index.evidence(generation, &references, &catalogs, &bindings, allowed)?;
            let mut nominated = examples.candidates;
            for candidate in &mut nominated {
                candidate.admissions.insert("workflow_guidance".into());
            }
            for candidate in recall.candidates {
                if !nominated.iter().any(|c| c.reference == candidate.reference) {
                    nominated.push(candidate);
                }
            }
            nominated.truncate(crate::discovery_store::CANDIDATE_LIMIT);
            recall.candidates = nominated;
        }
        let mut attempt = 0;
        let (retrieval, matching, search) = matcher::online::select_recalled_indexed(
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
            |issued| {
                attempt += 1;
                let attempt = attempt;
                async move {
                    // Serialize identical requests within this service, then recheck the
                    // durable cache. Failures are never cached; cancellation releases ownership.
                    let _same_request =
                        self.store.pending_decisions.claim(issued.cache_key()).await;
                    if let Some(raw) = self
                        .store
                        .cached_selector_envelope(issued.cache_key())
                        .await?
                    {
                        return Ok(raw);
                    }
                    let raw = self.contribution_response(issued.body(), attempt).await?;
                    matcher::decode_batch(&issued, &raw).map_err(|error| {
                        matcher::online::ReceivedResponseFailure::new(&raw, Box::new(error))
                    })?;
                    self.store
                        .store_selector_envelope(issued.cache_key(), &raw)
                        .await
                        .map_err(|error| {
                            matcher::online::ReceivedResponseFailure::new(&raw, Box::new(error))
                        })?;
                    Ok(raw)
                }
            },
            |progress| {
                let key = &progress_key;
                async move {
                    self.store
                        .store_search_progress(key, &serde_json::to_string(&progress)?)
                        .await?;
                    Ok(())
                }
            },
        )
        .await?;
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
        let recovery = if business.is_empty() && exposed.is_empty() {
            Some(DiscoveryRecovery::from_unmatched(
                &matching,
                &retrieval,
                &catalogs,
                allowed.catalogs.iter(),
            ))
        } else {
            None
        };
        Ok(RoutingReceipt {
            intent_provenance: provenance.clone(),
            authorization: allowed.clone(),
            intent_analysis:
                "**Capability contribution** (bounded online search; unvisited capabilities remain unexplored):"
                    .to_owned(),
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
