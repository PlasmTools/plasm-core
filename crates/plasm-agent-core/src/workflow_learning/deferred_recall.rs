//! Nonblocking, session-bound teaching recall for discovery.
use super::{
    catalogue_guidance::CatalogueAssociation,
    model::{LibraryScope, StoredExample},
    WorkflowLearning,
};
use crate::discovery_store::{DiscoveryAuthorization, DiscoveryStore};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

type Recall = (Vec<StoredExample>, Vec<CatalogueAssociation>);

#[derive(Clone, PartialEq, Eq)]
pub struct RecallContext {
    pub scope: LibraryScope,
    pub intent: String,
    pub intent_revision: String,
    pub generation: String,
    pub authorization: DiscoveryAuthorization,
}

/// Same-intent continuation is one revision; changing away and back is not.
pub fn intent_revision(provenance: &crate::intent_provenance::IntentProvenance) -> String {
    let mut turns = Vec::new();
    for turn in provenance.turns() {
        if turns.last().copied() != Some(turn) {
            turns.push(turn);
        }
    }
    plasm_core::catalog_discovery::content_hash(
        &serde_json::to_vec(&turns).expect("intent strings serialize"),
    )
}

enum State {
    Pending,
    Completed(Recall),
    Unavailable,
}
struct Entry {
    context: RecallContext,
    session: Option<String>,
    created: Instant,
    state: State,
    delivered: bool,
    delivered_examples: BTreeSet<Uuid>,
}

#[derive(Default)]
pub struct DeferredRecalls {
    entries: Mutex<BTreeMap<Uuid, Entry>>,
    supplemental: Mutex<BTreeMap<Uuid, SupplementalEntry>>,
}

struct SupplementalEntry {
    context: RecallContext,
    session: Option<String>,
    created: Instant,
    exposed: Vec<plasm_core::prerequisites::CapabilityRef>,
    binding: Option<tokio::sync::watch::Sender<Option<String>>>,
    state: SupplementalState,
}

enum SupplementalState {
    Pending,
    Ready(SupplementalResult),
    Claimed,
    Delivered,
    Unavailable,
}

#[derive(Debug, PartialEq, Eq)]
enum FinishSupplemental {
    Retained,
    Retry,
    Gone,
}

#[derive(Clone, Copy)]
pub enum SupplementalNotice {
    Ready,
    Unavailable,
}

impl SupplementalNotice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum SupplementalError {
    #[error("supplemental catalogue revalidation unavailable")]
    Revalidation,
    #[error("supplemental intent is invalid")]
    Intent,
    #[error(transparent)]
    Discovery(#[from] crate::discovery_service::DiscoveryServiceError),
    #[error("supplemental receipt encoding failed")]
    Encoding,
}

#[derive(Clone)]
pub struct SupplementalResult {
    pub intent: String,
    pub closure: plasm_core::prerequisites::PrerequisiteClosure,
    pub associations: Vec<CatalogueAssociation>,
    pub receipt_json: String,
}

/// A claim rolls back on cancellation so another session turn can deliver it.
pub struct SupplementalClaim {
    owner: Arc<DeferredRecalls>,
    ticket: Uuid,
    result: Option<SupplementalResult>,
}

impl SupplementalClaim {
    pub fn result(&self) -> &SupplementalResult {
        self.result.as_ref().expect("active supplemental claim")
    }

    pub fn commit(mut self) {
        if let Ok(mut states) = self.owner.supplemental.lock() {
            if let Some(entry) = states.get_mut(&self.ticket) {
                entry.state = SupplementalState::Delivered;
                self.result = None;
            }
        }
    }
}

impl Drop for SupplementalClaim {
    fn drop(&mut self) {
        if let Some(result) = self.result.take() {
            if let Ok(mut states) = self.owner.supplemental.lock() {
                if let Some(entry) = states.get_mut(&self.ticket) {
                    if matches!(entry.state, SupplementalState::Claimed) {
                        entry.state = SupplementalState::Ready(result);
                    }
                }
            }
        }
    }
}

pub enum RecallStatus {
    Pending,
    Completed(Recall),
    Unavailable,
    Missing,
}

/// A result remains associated with the intent for which it was judged.
#[derive(Clone)]
pub struct CompletedRecall {
    pub ticket: Uuid,
    pub intent: String,
    pub examples: Vec<StoredExample>,
    pub associations: Vec<CatalogueAssociation>,
}

/// Recheck publication state before using a completed handoff. Failure is closed.
pub async fn validate_current(
    learning: &WorkflowLearning,
    scope: &LibraryScope,
    value: Recall,
) -> Option<Recall> {
    tokio::time::timeout(Duration::from_millis(200), async {
        let repo = learning.repository().await.ok()?;
        let (examples, guidance) = value;
        let mut current_examples = Vec::new();
        for example in examples {
            match repo.retrieve(scope, example.id).await {
                Ok(current)
                    if serde_json::to_value(&current.candidate).ok()?
                        == serde_json::to_value(&example.candidate).ok()? =>
                {
                    current_examples.push(current)
                }
                Ok(_) | Err(super::repository::RepositoryError::Unavailable) => {}
                Err(_) => return None,
            }
        }
        let mut current_guidance = Vec::new();
        for association in guidance {
            let current = repo.guidance_is_current(scope, &association).await.ok()?;
            tracing::debug!(target: "plasm_agent::workflow_learning", family = %association.family, variant = %association.variant, current, "catalogue guidance publication revalidated");
            if current {
                current_guidance.push(association);
            }
        }
        Some((current_examples, current_guidance))
    })
    .await
    .ok()
    .flatten()
}

impl DeferredRecalls {
    pub fn discard_stale_supplemental(
        &self,
        session: &str,
        context: &RecallContext,
        exposed: &[plasm_core::prerequisites::CapabilityRef],
    ) {
        let exposed: Vec<_> = exposed
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if let Ok(mut states) = self.supplemental.lock() {
            states.retain(|_, entry| {
                if entry.session.as_deref() != Some(session) {
                    return true;
                }
                if &entry.context == context {
                    if matches!(entry.state, SupplementalState::Pending) {
                        entry.exposed = exposed.clone();
                    }
                    return true;
                }
                if matches!(entry.state, SupplementalState::Pending)
                    && entry.context.scope == context.scope
                    && entry.context.generation == context.generation
                    && entry.context.authorization == context.authorization
                {
                    entry.context = context.clone();
                    entry.exposed = exposed.clone();
                    return true;
                }
                false
            });
        }
    }

    fn pending_supplemental_context(
        &self,
        ticket: Uuid,
    ) -> Option<(RecallContext, Vec<plasm_core::prerequisites::CapabilityRef>)> {
        let states = self.supplemental.lock().ok()?;
        let entry = states.get(&ticket)?;
        matches!(entry.state, SupplementalState::Pending)
            .then(|| (entry.context.clone(), entry.exposed.clone()))
    }

    fn finish_supplemental(
        &self,
        ticket: Uuid,
        judged_context: &RecallContext,
        judged_exposed: &[plasm_core::prerequisites::CapabilityRef],
        state: SupplementalState,
    ) -> FinishSupplemental {
        let Ok(mut states) = self.supplemental.lock() else {
            return FinishSupplemental::Gone;
        };
        let Some(entry) = states.get_mut(&ticket) else {
            return FinishSupplemental::Gone;
        };
        if !matches!(entry.state, SupplementalState::Pending) {
            return FinishSupplemental::Gone;
        }
        if &entry.context != judged_context || entry.exposed != judged_exposed {
            return FinishSupplemental::Retry;
        }
        entry.state = state;
        FinishSupplemental::Retained
    }
    pub fn supplemental_ready_tickets(
        &self,
        session: &str,
        intent: &str,
        revision: &str,
        generation: &str,
        authorization: &DiscoveryAuthorization,
    ) -> Vec<(Uuid, SupplementalNotice)> {
        let Ok(states) = self.supplemental.lock() else {
            return Vec::new();
        };
        states
            .iter()
            .filter_map(|(id, entry)| {
                let notice = match entry.state {
                    SupplementalState::Ready(_) => SupplementalNotice::Ready,
                    SupplementalState::Unavailable => SupplementalNotice::Unavailable,
                    _ => return None,
                };
                (entry.created.elapsed() < Duration::from_secs(24 * 3600)
                    && entry.session.as_deref() == Some(session)
                    && entry.context.intent == intent
                    && entry.context.intent_revision == revision
                    && entry.context.generation == generation
                    && &entry.context.authorization == authorization)
                    .then_some((*id, notice))
            })
            .collect()
    }
    pub fn has_delivery(&self, session: &str, include_examples: bool) -> bool {
        let examples_ready = include_examples
            && self.entries.lock().ok().is_some_and(|entries| {
                entries.values().any(|entry|
            entry.session.as_deref() == Some(session) && !entry.delivered
                && matches!(&entry.state, State::Completed((examples, _)) if !examples.is_empty()))
            });
        examples_ready
            || self.supplemental.lock().ok().is_some_and(|states| {
                states.values().any(|entry| {
                    entry.session.as_deref() == Some(session)
                        && matches!(
                            entry.state,
                            SupplementalState::Ready(_) | SupplementalState::Unavailable
                        )
                })
            })
    }

    pub fn take_supplemental_failure(&self, session: &str, context: &RecallContext) -> bool {
        let Ok(mut states) = self.supplemental.lock() else {
            return false;
        };
        let Some(entry) = states.values_mut().find(|entry| {
            entry.session.as_deref() == Some(session)
                && &entry.context == context
                && matches!(entry.state, SupplementalState::Unavailable)
        }) else {
            return false;
        };
        entry.state = SupplementalState::Delivered;
        true
    }
    pub fn claim_supplemental(
        self: &Arc<Self>,
        session: &str,
        context: &RecallContext,
    ) -> Option<SupplementalClaim> {
        let mut states = self.supplemental.lock().ok()?;
        states.retain(|_, entry| entry.created.elapsed() < Duration::from_secs(24 * 3600));
        for entry in states.values().filter(|entry| {
            entry.session.as_deref() == Some(session)
                && matches!(entry.state, SupplementalState::Ready(_))
        }) {
            tracing::debug!(target: "plasm_agent::workflow_learning",
                scope_matches = entry.context.scope == context.scope,
                intent_matches = entry.context.intent == context.intent,
                revision_matches = entry.context.intent_revision == context.intent_revision,
                generation_matches = entry.context.generation == context.generation,
                authorization_matches = entry.context.authorization == context.authorization,
                "checking supplemental discovery session fence");
        }
        let ticket = states
            .iter()
            .filter(|(_, entry)| {
                entry.session.as_deref() == Some(session)
                    && &entry.context == context
                    && matches!(entry.state, SupplementalState::Ready(_))
            })
            .map(|(id, entry)| (*id, entry.created))
            .max_by_key(|(_, created)| *created)?
            .0;
        let SupplementalState::Ready(result) = std::mem::replace(
            &mut states.get_mut(&ticket)?.state,
            SupplementalState::Claimed,
        ) else {
            return None;
        };
        Some(SupplementalClaim {
            owner: Arc::clone(self),
            ticket,
            result: Some(result),
        })
    }

    pub fn supplemental_ready(&self, session: &str, context: &RecallContext) -> bool {
        let Ok(states) = self.supplemental.lock() else {
            return false;
        };
        states.values().any(|entry| {
            entry.created.elapsed() < Duration::from_secs(24 * 3600)
                && entry.session.as_deref() == Some(session)
                && &entry.context == context
                && matches!(entry.state, SupplementalState::Ready(_))
        })
    }
    /// Return prior-intent associations as candidates, independently of whether
    /// their example cards have already been delivered. They are not admission
    /// decisions for the current intent.
    pub fn prior_associations_for_session(
        &self,
        session: &str,
        context: &RecallContext,
    ) -> Vec<CatalogueAssociation> {
        let Ok(mut entries) = self.entries.lock() else {
            return Vec::new();
        };
        entries.retain(|_, entry| entry.created.elapsed() < Duration::from_secs(300));
        let mut matching: Vec<_> = entries
            .values()
            .filter(|entry| {
                entry.session.as_deref() == Some(session)
                    && entry.context.scope == context.scope
                    && entry.context.generation == context.generation
                    && entry.context.authorization == context.authorization
                    && entry.context.intent != context.intent
            })
            .collect();
        matching.sort_by_key(|entry| std::cmp::Reverse(entry.created));
        let mut seen = BTreeSet::new();
        matching
            .into_iter()
            .filter_map(|entry| match &entry.state {
                State::Completed((_, associations)) => Some(associations.as_slice()),
                _ => None,
            })
            .flatten()
            .filter(|association| {
                seen.insert((association.family.clone(), association.variant.clone()))
            })
            .take(16)
            .cloned()
            .collect()
    }

    /// Find completed teaching for this exact session and authority context.
    /// The caller must revalidate current publication state before disclosure.
    pub fn completed_for_session(
        &self,
        session: &str,
        scope: &LibraryScope,
        generation: &str,
        authorization: &DiscoveryAuthorization,
    ) -> Vec<CompletedRecall> {
        let Ok(mut entries) = self.entries.lock() else {
            return Vec::new();
        };
        entries.retain(|_, item| item.created.elapsed() < Duration::from_secs(300));
        entries
            .iter()
            .filter_map(|(ticket, entry)| {
                if entry.session.as_deref() != Some(session)
                    || entry.delivered
                    || &entry.context.scope != scope
                    || entry.context.generation != generation
                    || &entry.context.authorization != authorization
                {
                    return None;
                }
                match &entry.state {
                    State::Completed((examples, associations)) => Some(CompletedRecall {
                        ticket: *ticket,
                        intent: entry.context.intent.clone(),
                        examples: examples.clone(),
                        associations: associations.clone(),
                    }),
                    _ => None,
                }
            })
            .collect()
    }

    /// Claim a validated result once, including across concurrent tool calls.
    pub fn claim_delivery(&self, ticket: Uuid, examples: &[Uuid]) -> BTreeSet<Uuid> {
        let Ok(mut entries) = self.entries.lock() else {
            return BTreeSet::new();
        };
        let Some(entry) = entries.get(&ticket) else {
            return BTreeSet::new();
        };
        if entry.delivered || !matches!(entry.state, State::Completed(_)) {
            return BTreeSet::new();
        }
        let session = entry.session.clone();
        let context = entry.context.clone();
        let seen: BTreeSet<Uuid> = entries
            .values()
            .filter(|other| {
                other.session == session
                    && other.context.scope == context.scope
                    && other.context.generation == context.generation
                    && other.context.authorization == context.authorization
            })
            .flat_map(|other| other.delivered_examples.iter().copied())
            .collect();
        let fresh: BTreeSet<Uuid> = examples
            .iter()
            .copied()
            .filter(|id| !seen.contains(id))
            .collect();
        let entry = entries
            .get_mut(&ticket)
            .expect("ticket remains present while locked");
        entry.delivered = true;
        entry.delivered_examples = fresh.clone();
        fresh
    }
    /// Start before ordinary routing; the request never waits for the result.
    pub fn start(
        self: &Arc<Self>,
        learning: Arc<WorkflowLearning>,
        context: RecallContext,
        store: DiscoveryStore,
        prior: Vec<CatalogueAssociation>,
        exposed: Vec<plasm_core::prerequisites::CapabilityRef>,
    ) -> Option<Uuid> {
        if !matches!(
            learning.mode,
            super::LearningMode::Serving | super::LearningMode::ShadowWithRecall
        ) {
            return None;
        }
        let task_context = context.clone();
        let supplemental_learning = Arc::clone(&learning);
        let supplemental_store = store.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let queued_at = Instant::now();
        let ticket = self.schedule(context.clone(), async move {
            static POOL: std::sync::OnceLock<crate::live_plan_run_worker::LivePlanRunPool> =
                std::sync::OnceLock::new();
            let pool = POOL.get_or_init(crate::live_plan_run_worker::LivePlanRunPool::new);
            let result = pool
                .run_local(move || async move {
                    if queued_at.elapsed() >= Duration::from_secs(300) {
                        return None;
                    }
                    Some(
                        learning
                            .recall_with_guidance(
                                &task_context.scope,
                                &task_context.intent,
                                &task_context.authorization,
                                &store,
                                &task_context.generation,
                            )
                            .await,
                    )
                })
                .await;
            match result {
                Ok(Some(Ok(value))) => {
                    tracing::debug!(target: "plasm_agent::workflow_learning", examples = value.0.len(), guidance = value.1.len(), elapsed_ms = queued_at.elapsed().as_millis(), "deferred teaching recall completed");
                    let _ = sender.send(value.1.clone());
                    State::Completed(value)
                }
                Ok(Some(Err(error))) => {
                    tracing::warn!(target: "plasm_agent::workflow_learning", %error, elapsed_ms = queued_at.elapsed().as_millis(), "deferred teaching recall failed");
                    State::Unavailable
                }
                Ok(None) => {
                    tracing::warn!(target: "plasm_agent::workflow_learning", "deferred teaching recall expired in the queue");
                    State::Unavailable
                }
                Err(error) => {
                    tracing::warn!(target: "plasm_agent::workflow_learning", %error, "deferred teaching recall worker failed");
                    State::Unavailable
                }
            }
        })?;
        let (binding_tx, mut binding_rx) = tokio::sync::watch::channel(None::<String>);
        {
            let mut states = self.supplemental.lock().ok()?;
            states.retain(|_, entry| {
                entry.created.elapsed() < Duration::from_secs(24 * 3600)
                    && !matches!(entry.state, SupplementalState::Delivered)
            });
            if states.len() >= 256 {
                tracing::warn!(target: "plasm_agent::workflow_learning", "supplemental discovery queue is full");
                return Some(ticket);
            }
            states.insert(
                ticket,
                SupplementalEntry {
                    context,
                    session: None,
                    created: Instant::now(),
                    exposed: exposed
                        .into_iter()
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                    binding: Some(binding_tx),
                    state: SupplementalState::Pending,
                },
            );
        }
        let owner = Arc::clone(self);
        tokio::spawn(async move {
            let mut associations = receiver.await.unwrap_or_default();
            associations.extend(prior);
            let mut seen = BTreeSet::new();
            associations.retain(|association| {
                seen.insert((association.family.clone(), association.variant.clone()))
            });
            associations.truncate(16);
            if associations.is_empty() {
                if let Ok(mut states) = owner.supplemental.lock() {
                    if let Some(entry) = states.get_mut(&ticket) {
                        entry.state = SupplementalState::Delivered;
                    }
                }
                return;
            }
            let session = tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let bound = binding_rx.borrow_and_update().clone();
                    if let Some(session) = bound {
                        return Some(session);
                    }
                    if binding_rx.changed().await.is_err() {
                        return None;
                    }
                }
            })
            .await
            .ok()
            .flatten();
            let Some(session) = session else {
                tracing::warn!(target: "plasm_agent::workflow_learning", "supplemental discovery was not bound to a session");
                if let Ok(mut states) = owner.supplemental.lock() {
                    if let Some(entry) = states.get_mut(&ticket) {
                        if matches!(entry.state, SupplementalState::Pending) {
                            entry.state = SupplementalState::Unavailable;
                        }
                    }
                }
                return;
            };
            static POOL: std::sync::OnceLock<crate::live_plan_run_worker::LivePlanRunPool> =
                std::sync::OnceLock::new();
            let pool = POOL.get_or_init(crate::live_plan_run_worker::LivePlanRunPool::new);
            // A session may refine its request while recall or matching runs.
            // Judge the latest request, and never publish a judgement for an
            // older revision. Bound retries so rapid changes cannot spawn an
            // unbounded chain of model calls.
            for attempt in 0..2 {
                let Some((supplemental_context, exposed)) =
                    owner.pending_supplemental_context(ticket)
                else {
                    return;
                };
                let judged_context = supplemental_context.clone();
                let judged_exposed = exposed.clone();
                let supplemental_learning = Arc::clone(&supplemental_learning);
                let supplemental_store = supplemental_store.clone();
                let associations = associations.clone();
                let session = session.clone();
                let job = pool.run_local(move || async move {
                    let (_, current) = validate_current(
                        &supplemental_learning,
                        &supplemental_context.scope,
                        (Vec::new(), associations),
                    )
                    .await
                    .ok_or(SupplementalError::Revalidation)?;
                    let capabilities = current
                        .iter()
                        .flat_map(|association| {
                            association
                                .requirements()
                                .required_capabilities()
                                .iter()
                                .cloned()
                        })
                        .collect::<BTreeSet<_>>();
                    if capabilities.is_empty() {
                        return Ok::<Option<SupplementalResult>, SupplementalError>(None);
                    }
                    let provenance = crate::intent_provenance::IntentProvenance::from_turns([
                        supplemental_context.intent.clone(),
                    ])
                    .map_err(|_| SupplementalError::Intent)?;
                    let service =
                        crate::discovery_service::DiscoveryService::from_env(supplemental_store)?
                            .supplemental(capabilities);
                    let receipt = service
                        .route_supplemental_turn(
                            &session,
                            ticket,
                            &supplemental_context.generation,
                            &provenance,
                            &supplemental_context.authorization,
                            &exposed,
                        )
                        .await?;
                    let Some(closure) = receipt.closure.clone() else {
                        return Ok(None);
                    };
                    if closure.business.is_empty() {
                        return Ok(None);
                    }
                    let receipt_json =
                        serde_json::to_string(&receipt).map_err(|_| SupplementalError::Encoding)?;
                    Ok(Some(SupplementalResult {
                        intent: supplemental_context.intent,
                        closure,
                        associations: current,
                        receipt_json,
                    }))
                });
                let result = tokio::time::timeout(Duration::from_secs(30), job).await;
                let state = match result {
                    Ok(Ok(Ok(Some(result)))) => SupplementalState::Ready(result),
                    Ok(Ok(Ok(None))) => SupplementalState::Delivered,
                    Ok(Ok(Err(error))) => {
                        tracing::warn!(target: "plasm_agent::workflow_learning", %error, "supplemental discovery failed");
                        SupplementalState::Unavailable
                    }
                    _ => SupplementalState::Unavailable,
                };
                let outcome = match &state {
                    SupplementalState::Ready(_) => "ready",
                    SupplementalState::Delivered => "empty",
                    _ => "unavailable",
                };
                let finish =
                    owner.finish_supplemental(ticket, &judged_context, &judged_exposed, state);
                tracing::debug!(target: "plasm_agent::workflow_learning", ticket = %ticket, outcome, ?finish, attempt, "supplemental discovery completed");
                if finish != FinishSupplemental::Retry {
                    return;
                }
            }
            if let Ok(mut states) = owner.supplemental.lock() {
                if let Some(entry) = states.get_mut(&ticket) {
                    if matches!(entry.state, SupplementalState::Pending) {
                        entry.state = SupplementalState::Unavailable;
                    }
                }
            }
        });
        Some(ticket)
    }

    fn schedule(
        self: &Arc<Self>,
        context: RecallContext,
        work: impl std::future::Future<Output = State> + Send + 'static,
    ) -> Option<Uuid> {
        let id = Uuid::new_v4();
        {
            let mut entries = self.entries.lock().ok()?;
            entries.retain(|_, item| item.created.elapsed() < Duration::from_secs(300));
            if entries.len() >= 256 {
                return None;
            }
            entries.insert(
                id,
                Entry {
                    context,
                    session: None,
                    created: Instant::now(),
                    state: State::Pending,
                    delivered: false,
                    delivered_examples: BTreeSet::new(),
                },
            );
        }
        let owner = Arc::clone(self);
        tokio::spawn(async move {
            let state = work.await;
            if let Ok(mut entries) = owner.entries.lock() {
                if let Some(entry) = entries.get_mut(&id) {
                    entry.state = state;
                }
            }
        });
        Some(id)
    }

    pub fn bind(&self, id: Uuid, session: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            if let Some(entry) = entries.get_mut(&id) {
                entry.session = Some(session.to_owned());
            }
        }
        if let Ok(mut states) = self.supplemental.lock() {
            if let Some(entry) = states.get_mut(&id) {
                entry.session = Some(session.to_owned());
                if let Some(binding) = &entry.binding {
                    binding.send_replace(Some(session.to_owned()));
                }
            }
        }
    }

    /// Exact context matching prevents stale or cross-scope teaching disclosure.
    pub fn status(&self, session: &str, context: &RecallContext) -> RecallStatus {
        let Ok(mut entries) = self.entries.lock() else {
            return RecallStatus::Unavailable;
        };
        entries.retain(|_, item| item.created.elapsed() < Duration::from_secs(300));
        match entries
            .values()
            .filter(|entry| entry.session.as_deref() == Some(session) && &entry.context == context)
            .max_by_key(|entry| entry.created)
            .map(|entry| &entry.state)
        {
            Some(State::Pending) => RecallStatus::Pending,
            Some(State::Completed(value)) => RecallStatus::Completed(value.clone()),
            Some(State::Unavailable) => RecallStatus::Unavailable,
            None => RecallStatus::Missing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn supplemental_pending_rejudges_latest_intent_and_never_crosses_authority() {
        let owner = DeferredRecalls::default();
        let first = context();
        let ticket = Uuid::new_v4();
        owner.supplemental.lock().unwrap().insert(
            ticket,
            SupplementalEntry {
                context: first.clone(),
                session: Some("session".into()),
                created: Instant::now(),
                exposed: vec![],
                binding: None,
                state: SupplementalState::Pending,
            },
        );
        let another_session = Uuid::new_v4();
        owner.supplemental.lock().unwrap().insert(
            another_session,
            SupplementalEntry {
                context: first.clone(),
                session: Some("another-session".into()),
                created: Instant::now(),
                exposed: vec![],
                binding: None,
                state: SupplementalState::Pending,
            },
        );
        let mut next = first.clone();
        next.intent = "a refined request".into();
        next.intent_revision = "next revision".into();
        let exposed = vec![plasm_core::prerequisites::CapabilityRef {
            catalog: "fixture".into(),
            capability: "read".into(),
        }];
        owner.discard_stale_supplemental("session", &next, &exposed);
        assert!(owner
            .pending_supplemental_context(another_session)
            .is_some_and(|(value, refs)| value == first && refs.is_empty()));
        assert!(owner
            .pending_supplemental_context(ticket)
            .is_some_and(|(value, refs)| value == next && refs == exposed));
        assert_eq!(
            owner.finish_supplemental(ticket, &first, &[], SupplementalState::Delivered),
            FinishSupplemental::Retry
        );
        let mut newer_exposure = exposed.clone();
        newer_exposure.push(plasm_core::prerequisites::CapabilityRef {
            catalog: "fixture".into(),
            capability: "write".into(),
        });
        owner.discard_stale_supplemental("session", &next, &newer_exposure);
        assert_eq!(
            owner.finish_supplemental(ticket, &next, &exposed, SupplementalState::Delivered),
            FinishSupplemental::Retry
        );
        assert_eq!(
            owner.finish_supplemental(ticket, &next, &newer_exposure, SupplementalState::Delivered),
            FinishSupplemental::Retained
        );

        let other = Uuid::new_v4();
        owner.supplemental.lock().unwrap().insert(
            other,
            SupplementalEntry {
                context: first.clone(),
                session: Some("session".into()),
                created: Instant::now(),
                exposed: vec![],
                binding: None,
                state: SupplementalState::Pending,
            },
        );
        let mut denied = first.clone();
        denied.authorization = DiscoveryAuthorization::catalogs(BTreeSet::new());
        owner.discard_stale_supplemental("session", &denied, &[]);
        assert!(owner.pending_supplemental_context(other).is_none());
        assert_eq!(
            owner.finish_supplemental(other, &first, &[], SupplementalState::Delivered),
            FinishSupplemental::Gone
        );
    }

    #[test]
    fn supplemental_claim_is_scoped_retryable_and_delivered_once() {
        let owner = Arc::new(DeferredRecalls::default());
        let context = context();
        let ticket = Uuid::new_v4();
        let result = SupplementalResult {
            intent: context.intent.clone(),
            closure: plasm_core::prerequisites::PrerequisiteClosure {
                acquisitions: vec![],
                business: vec![],
                input_sources: vec![],
                prerequisites: vec![],
                edges: vec![],
            },
            associations: vec![],
            receipt_json: "{}".into(),
        };
        owner.supplemental.lock().unwrap().insert(
            ticket,
            SupplementalEntry {
                context: context.clone(),
                session: Some("session".into()),
                created: Instant::now() - Duration::from_secs(301),
                exposed: vec![],
                binding: None,
                state: SupplementalState::Ready(result),
            },
        );
        let mut changed = context.clone();
        changed.intent = "another intent".into();
        assert!(owner.claim_supplemental("session", &changed).is_none());
        assert!(owner.claim_supplemental("other", &context).is_none());
        let mut changed_revision = context.clone();
        changed_revision.intent_revision = "later revision".into();
        assert!(owner
            .claim_supplemental("session", &changed_revision)
            .is_none());
        let mut other_project = context.clone();
        other_project.scope.project = "other".into();
        assert!(owner
            .claim_supplemental("session", &other_project)
            .is_none());
        let mut other_catalogue = context.clone();
        other_catalogue.generation = "other".into();
        assert!(owner
            .claim_supplemental("session", &other_catalogue)
            .is_none());
        let mut unauthorized = context.clone();
        unauthorized.authorization = DiscoveryAuthorization::catalogs(BTreeSet::new());
        assert!(owner.claim_supplemental("session", &unauthorized).is_none());
        let claim = owner.claim_supplemental("session", &context).unwrap();
        assert!(owner.claim_supplemental("session", &context).is_none());
        drop(claim);
        owner
            .claim_supplemental("session", &context)
            .unwrap()
            .commit();
        assert!(owner.claim_supplemental("session", &context).is_none());
    }

    #[test]
    fn supplemental_revision_ignores_repeated_turns_but_not_changed_back_intent() {
        let chain = |turns: &[&str]| {
            crate::intent_provenance::IntentProvenance::from_turns(
                turns.iter().map(|s| (*s).to_owned()),
            )
            .unwrap()
        };
        assert_eq!(
            intent_revision(&chain(&["a"])),
            intent_revision(&chain(&["a", "a"]))
        );
        assert_ne!(
            intent_revision(&chain(&["a"])),
            intent_revision(&chain(&["a", "b", "a"]))
        );
    }

    fn context() -> RecallContext {
        RecallContext {
            scope: LibraryScope {
                tenant: "tenant".into(),
                project: "project".into(),
            },
            intent: "inspect an item".into(),
            intent_revision: "revision".into(),
            generation: "generation".into(),
            authorization: DiscoveryAuthorization::catalogs(BTreeSet::from(["catalog".into()])),
        }
    }

    fn association() -> CatalogueAssociation {
        CatalogueAssociation {
            family: "fixture".into(),
            variant: "fixture".into(),
            intent: super::super::index::NormalizedIntent {
                purpose: "old task".into(),
                facets: vec![],
                aliases: vec![],
            },
            embedding: vec![],
            embedding_profile: "fixture".into(),
            requirements: serde_json::from_value(serde_json::json!({
                "capability_contracts": [], "dependencies": [],
                "required_capabilities": [{"catalog":"catalog", "capability":"read"}],
                "catalog_fingerprints": {}, "source_sha256":"fixture", "compiled_sha256":"fixture"
            }))
            .unwrap(),
            evidence: BTreeMap::new(),
            confirmed: true,
        }
    }

    #[test]
    fn prior_guidance_survives_example_delivery_and_requires_same_authority() {
        let recalls = DeferredRecalls::default();
        let original = context();
        let ticket = Uuid::new_v4();
        recalls.entries.lock().unwrap().insert(
            ticket,
            Entry {
                context: original.clone(),
                session: Some("session".into()),
                created: Instant::now(),
                state: State::Completed((vec![], vec![association()])),
                delivered: true,
                delivered_examples: BTreeSet::new(),
            },
        );
        let mut changed = original.clone();
        changed.intent = "a changed request".into();
        assert_eq!(
            recalls
                .prior_associations_for_session("session", &changed)
                .len(),
            1
        );
        assert!(recalls
            .prior_associations_for_session("session", &original)
            .is_empty());
        assert!(recalls
            .prior_associations_for_session("other", &changed)
            .is_empty());
        let mut denied = changed.clone();
        denied.authorization.catalogs.clear();
        assert!(recalls
            .prior_associations_for_session("session", &denied)
            .is_empty());
        let mut other_project = changed.clone();
        other_project.scope.project = "other".into();
        assert!(recalls
            .prior_associations_for_session("session", &other_project)
            .is_empty());
        let mut drift = changed;
        drift.generation = "other".into();
        assert!(recalls
            .prior_associations_for_session("session", &drift)
            .is_empty());
    }

    #[test]
    fn continuation_requires_matching_session_scope_generation_and_authorization() {
        let recalls = DeferredRecalls::default();
        let id = Uuid::new_v4();
        recalls.entries.lock().unwrap().insert(
            id,
            Entry {
                context: context(),
                session: None,
                created: Instant::now(),
                state: State::Completed((vec![], vec![])),
                delivered: false,
                delivered_examples: BTreeSet::new(),
            },
        );
        assert!(matches!(
            recalls.status("session", &context()),
            RecallStatus::Missing
        ));
        recalls.bind(id, "session");
        assert!(matches!(
            recalls.status("session", &context()),
            RecallStatus::Completed(_)
        ));
        assert!(matches!(
            recalls.status("other", &context()),
            RecallStatus::Missing
        ));
        let mut altered = context();
        altered.scope.project = "other".into();
        assert!(matches!(
            recalls.status("session", &altered),
            RecallStatus::Missing
        ));
        altered = context();
        altered.generation = "new".into();
        assert!(matches!(
            recalls.status("session", &altered),
            RecallStatus::Missing
        ));
        altered = context();
        altered.intent = "different request".into();
        assert!(matches!(
            recalls.status("session", &altered),
            RecallStatus::Missing
        ));
        altered = context();
        altered.authorization.catalogs.clear();
        assert!(matches!(
            recalls.status("session", &altered),
            RecallStatus::Missing
        ));
    }

    #[test]
    fn completed_handoff_is_bound_to_session_scope_generation_and_authority() {
        let recalls = DeferredRecalls::default();
        let ctx = context();
        let id = Uuid::new_v4();
        recalls.entries.lock().unwrap().insert(
            id,
            Entry {
                context: ctx.clone(),
                session: Some("session".into()),
                created: Instant::now(),
                state: State::Completed((vec![], vec![])),
                delivered: false,
                delivered_examples: BTreeSet::new(),
            },
        );
        let ready = recalls.completed_for_session(
            "session",
            &ctx.scope,
            &ctx.generation,
            &ctx.authorization,
        );
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].intent, ctx.intent);
        assert!(recalls
            .completed_for_session("other", &ctx.scope, &ctx.generation, &ctx.authorization)
            .is_empty());
        assert!(recalls
            .completed_for_session("session", &ctx.scope, "different", &ctx.authorization)
            .is_empty());
        let mut other = ctx.scope.clone();
        other.project = "other".into();
        assert!(recalls
            .completed_for_session("session", &other, &ctx.generation, &ctx.authorization)
            .is_empty());
        let mut denied = ctx.authorization.clone();
        denied.catalogs.clear();
        assert!(recalls
            .completed_for_session("session", &ctx.scope, &ctx.generation, &denied)
            .is_empty());
        let example_id = Uuid::new_v4();
        assert!(recalls
            .claim_delivery(id, &[example_id])
            .contains(&example_id));
        assert!(recalls.claim_delivery(id, &[example_id]).is_empty());
        let second = Uuid::new_v4();
        let mut second_context = ctx.clone();
        second_context.intent = "another intent".into();
        recalls.entries.lock().unwrap().insert(
            second,
            Entry {
                context: second_context,
                session: Some("session".into()),
                created: Instant::now(),
                state: State::Completed((vec![], vec![])),
                delivered: false,
                delivered_examples: BTreeSet::new(),
            },
        );
        assert!(recalls.claim_delivery(second, &[example_id]).is_empty());
        assert!(recalls
            .completed_for_session("session", &ctx.scope, &ctx.generation, &ctx.authorization)
            .is_empty());
    }

    #[tokio::test]
    async fn slow_recall_does_not_block_routing_and_remains_available_later() {
        let recalls = Arc::new(DeferredRecalls::default());
        let (release, wait) = tokio::sync::oneshot::channel();
        let id = recalls
            .schedule(context(), async move {
                let _ = wait.await;
                State::Completed((vec![], vec![]))
            })
            .unwrap();
        recalls.bind(id, "session");
        assert!(matches!(
            recalls.status("session", &context()),
            RecallStatus::Pending
        ));
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if matches!(
                    recalls.status("session", &context()),
                    RecallStatus::Completed(_)
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
