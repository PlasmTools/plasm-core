//! Bounded online selection. Catalogue facts nominate; contribution judgments retain.
#[cfg(test)]
#[path = "online/additive_tests.rs"]
mod additive_tests;
mod connected;
mod operation_selection;
pub(crate) use operation_selection::select_operations_indexed;
#[cfg(test)]
#[path = "online/decision_diagnostic.rs"]
mod decision_diagnostic;
#[cfg(test)]
#[path = "online/fixed_suite.rs"]
mod fixed_suite;
#[cfg(test)]
mod hybrid_tests;
#[cfg(test)]
#[path = "online/live_gate.rs"]
mod live_gate;
#[cfg(test)]
#[path = "online/progress_suite.rs"]
mod progress_suite;
#[cfg(test)]
mod reformulation_tests;
#[cfg(test)]
mod retry_tests;
#[cfg(test)]
#[path = "online/starvation_suite.rs"]
mod starvation_suite;
#[cfg(test)]
mod trajectory_suite;
use super::{
    catalogue_search::CatalogueSearch,
    search_budget::{BudgetStop, SearchBudget},
    search_graph::{Edge, Relationship},
    witnesses::{Binding, Endpoint, Slot},
    *,
};
use crate::discovery_store::{DiscoveryAuthorization, RetrievalReceipt};
use plasm_core::{prerequisites::DeploymentBindings, CGS};

pub(crate) const SEARCH_POLICY: &str =
    "additive-connected-frontier-current-hybrid-evidence-3-judgments-3-attempts-32000-byte-packet-v46";

#[derive(Clone, Copy)]
pub struct SearchContext<'a> {
    pub model: &'a str,
    pub intent: &'a IntentProvenance,
    pub generation: &'a str,
    pub catalogs: &'a BTreeMap<String, CGS>,
    pub deployments: &'a DeploymentBindings,
    pub allowed: &'a DiscoveryAuthorization,
    /// Available declarations, not acquired values or permanent relevance judgments.
    pub exposed: &'a [CapabilityRef],
    /// Session-local completed judgments; priority only, never an exclusion.
    pub visits: &'a BTreeMap<CapabilityRef, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferredReason {
    Requests,
    InputBytes,
    ReportedInputTokens,
    OperationPending,
    OperationUncertain,
    ProviderUnavailable,
}

impl From<BudgetStop> for DeferredReason {
    fn from(value: BudgetStop) -> Self {
        match value {
            BudgetStop::Requests => Self::Requests,
            BudgetStop::InputBytes => Self::InputBytes,
            BudgetStop::ReportedInputTokens => Self::ReportedInputTokens,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredCandidate {
    pub reference: CapabilityRef,
    pub reason: DeferredReason,
}

/// Preserve current-request recall order before bounding a deferred backlog.
/// Graph-only nominations follow recalled candidates and confer no relevance.
fn ordered_deferred(
    deferred: BTreeMap<CapabilityRef, DeferredReason>,
    recall: &RetrievalReceipt,
) -> Vec<DeferredCandidate> {
    let ranks: BTreeMap<_, _> = recall
        .candidates
        .iter()
        .enumerate()
        .map(|(rank, candidate)| (&candidate.reference, rank))
        .collect();
    let mut deferred: Vec<_> = deferred
        .into_iter()
        .map(|(reference, reason)| DeferredCandidate { reference, reason })
        .collect();
    deferred.sort_by_key(|candidate| {
        ranks
            .get(&candidate.reference)
            .copied()
            .unwrap_or(usize::MAX)
    });
    deferred
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchReceipt {
    pub policy: String,
    pub exposed: BTreeSet<CapabilityRef>,
    pub considered: BTreeSet<CapabilityRef>,
    pub unexplored: BTreeSet<CapabilityRef>,
    pub deferred: Vec<DeferredCandidate>,
    pub requests: usize,
    pub input_bytes: usize,
    pub reported_input_tokens: u64,
    pub missing_usage: usize,
}

impl Default for SearchReceipt {
    fn default() -> Self {
        Self {
            policy: SEARCH_POLICY.into(),
            exposed: BTreeSet::new(),
            considered: BTreeSet::new(),
            unexplored: BTreeSet::new(),
            deferred: Vec::new(),
            requests: 0,
            input_bytes: 0,
            reported_input_tokens: 0,
            missing_usage: 0,
        }
    }
}

impl SearchReceipt {
    pub fn has_unavailable_provider(&self) -> bool {
        self.deferred
            .iter()
            .any(|candidate| matches!(candidate.reason, DeferredReason::ProviderUnavailable))
    }

    pub fn has_pending_operation_judgments(&self) -> bool {
        self.deferred
            .iter()
            .any(|candidate| matches!(&candidate.reason, DeferredReason::OperationPending))
    }

    pub fn has_incomplete_operation_judgments(&self) -> bool {
        self.deferred.iter().any(|candidate| {
            matches!(
                &candidate.reason,
                DeferredReason::OperationPending
                    | DeferredReason::OperationUncertain
                    | DeferredReason::ProviderUnavailable
            )
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum ProgressState {
    Admitted,
    Settled,
    Failed { diagnostics: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PathProgress {
    target: CapabilityRef,
    edges: Vec<Edge<CapabilityRef>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchProgress {
    state: ProgressState,
    accounting: SearchReceipt,
    judgments: Vec<CapabilityIntentMatch>,
    pending: BTreeSet<CapabilityRef>,
    paths: Vec<PathProgress>,
}

impl std::fmt::Display for SearchProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} attempts admitted, {} input bytes, {} judgments saved, {} pending capabilities; spent admission retained",
            self.accounting.requests, self.accounting.input_bytes, self.judgments.len(), self.pending.len())
    }
}

impl SearchProgress {
    pub fn accounting(&self) -> &SearchReceipt {
        &self.accounting
    }
    pub fn pending(&self) -> &BTreeSet<CapabilityRef> {
        &self.pending
    }
}

/// A response arrived, but validation or persistence failed afterward.
/// Preserve provider accounting across that failure boundary.
#[derive(Debug, thiserror::Error)]
#[error("discovery response processing failed")]
pub(crate) struct ReceivedResponseFailure {
    input_tokens: Option<u64>,
    #[source]
    source: DecisionProviderError,
}

impl ReceivedResponseFailure {
    pub(crate) fn new(raw: &str, source: DecisionProviderError) -> Self {
        Self {
            input_tokens: reported_input_tokens(raw),
            source,
        }
    }
}

fn reported_input_tokens(raw: &str) -> Option<u64> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let usage = value.get("usage")?;
    usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))?
        .as_u64()
}

struct ProgressFrontier<'a> {
    routes: &'a BTreeMap<CapabilityRef, Vec<Edge<CapabilityRef>>>,
    deferred: &'a BTreeMap<CapabilityRef, DeferredReason>,
}

fn progress(
    index: &CatalogueSearch,
    exposed: &[CapabilityRef],
    budget: &SearchBudget<CapabilityRef>,
    judged: &BTreeMap<CapabilityRef, CapabilityIntentMatch>,
    frontier: ProgressFrontier<'_>,
    pending: BTreeSet<CapabilityRef>,
    state: ProgressState,
) -> SearchProgress {
    let ProgressFrontier { routes, deferred } = frontier;
    SearchProgress {
        state,
        accounting: SearchReceipt {
            exposed: exposed.iter().cloned().collect(),
            considered: budget.members().clone(),
            unexplored: index
                .graph
                .authorized()
                .iter()
                .filter(|r| {
                    !exposed.contains(r) && !judged.contains_key(*r) && !routes.contains_key(*r)
                })
                .cloned()
                .collect(),
            deferred: deferred
                .iter()
                .map(|(reference, reason)| DeferredCandidate {
                    reference: reference.clone(),
                    reason: reason.clone(),
                })
                .collect(),
            requests: budget.requests(),
            input_bytes: budget.input_bytes(),
            reported_input_tokens: budget.reported_input_tokens(),
            missing_usage: budget.missing_usage(),
            ..SearchReceipt::default()
        },
        judgments: judged.values().cloned().collect(),
        pending,
        paths: routes
            .iter()
            .map(|(target, edges)| PathProgress {
                target: target.clone(),
                edges: edges.clone(),
            })
            .collect(),
    }
}

fn failure_state(error: &dyn std::error::Error) -> ProgressState {
    let mut diagnostics = vec![error.to_string()];
    let mut cause = error.source();
    while let Some(error) = cause {
        diagnostics.push(error.to_string());
        cause = error.source();
    }
    ProgressState::Failed { diagnostics }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum NominationSlot {
    Input(CapabilityRef, plasm_core::prerequisites::InputSourceTarget),
    Creation(CapabilityRef),
}

fn nomination_slot(
    index: &CatalogueSearch,
    edge: &Edge<CapabilityRef>,
) -> Result<NominationSlot, DiscoveryMatcherError> {
    match edge.relationship {
        Relationship::SuppliesInput => {
            let binding = index.input_bindings.get(&edge.provenance).ok_or_else(|| {
                DiscoveryMatcherError::InvalidWitness {
                    reference: edge.source.clone(),
                    part: "input binding identity",
                }
            })?;
            Ok(NominationSlot::Input(
                binding.consumer.clone(),
                binding.input.clone(),
            ))
        }
        Relationship::MayCreateResource => Ok(NominationSlot::Creation(edge.consumer.clone())),
        Relationship::RequiredAcquisition => Err(DiscoveryMatcherError::InvalidWitness {
            reference: edge.source.clone(),
            part: "required acquisition nominated as optional",
        }),
    }
}

fn required(
    nodes: &BTreeSet<CapabilityRef>,
    catalogs: &BTreeMap<String, CGS>,
    deployments: &DeploymentBindings,
    allowed: &DiscoveryAuthorization,
) -> Result<BTreeSet<Edge<CapabilityRef>>, DiscoveryMatcherError> {
    required_closure(nodes, catalogs, deployments, allowed)?
        .edges
        .into_iter()
        .map(|edge| {
            Ok(Edge {
                source: edge.provider.clone(),
                consumer: edge.consumer.clone(),
                relationship: Relationship::RequiredAcquisition,
                provenance: serde_json::to_string(&edge).map_err(DiscoveryMatcherError::Packing)?,
            })
        })
        .collect()
}

fn required_closure(
    nodes: &BTreeSet<CapabilityRef>,
    catalogs: &BTreeMap<String, CGS>,
    deployments: &DeploymentBindings,
    allowed: &DiscoveryAuthorization,
) -> Result<plasm_core::prerequisites::PrerequisiteClosure, DiscoveryMatcherError> {
    let borrowed = catalogs
        .iter()
        .map(|(key, cgs)| (key.clone(), cgs))
        .collect();
    let closure = plasm_core::prerequisites::prerequisite_closure(
        &borrowed,
        deployments,
        &nodes.iter().cloned().collect::<Vec<_>>(),
        &allowed.catalogs,
    )?;
    for edge in &closure.edges {
        if !allowed.permits(&edge.provider) {
            return Err(DiscoveryMatcherError::UnauthorizedWitness(
                edge.provider.clone(),
            ));
        }
    }
    Ok(closure)
}

pub(super) fn packet_scope(
    index: &CatalogueSearch,
    target: &CapabilityRef,
    route: &[Edge<CapabilityRef>],
    context: SearchContext<'_>,
) -> Result<(witnesses::Scope, Question, BTreeSet<CapabilityRef>), DiscoveryMatcherError> {
    let mut route = route.to_vec();
    // Already available suppliers remain local composition evidence, without
    // becoming separate questions or implying their acquisition has executed.
    route.extend(
        index
            .graph
            .incoming(target)
            .filter(|edge| context.exposed.contains(&edge.source))
            .cloned(),
    );
    route.sort();
    route.dedup();
    let SearchContext {
        generation,
        catalogs,
        deployments,
        allowed,
        ..
    } = context;
    let mut members = BTreeSet::from([target.clone()]);
    for edge in &route {
        members.extend([edge.source.clone(), edge.consumer.clone()]);
    }
    let required = required(&members, catalogs, deployments, allowed)?;
    for edge in &required {
        members.extend([edge.source.clone(), edge.consumer.clone()]);
    }
    let (_, mut evidence) = index.evidence(generation, &members, catalogs, deployments, allowed)?;
    let input_edges = route
        .iter()
        .filter(|edge| edge.relationship == Relationship::SuppliesInput)
        .map(|edge| {
            index.input_bindings.get(&edge.provenance).ok_or_else(|| {
                DiscoveryMatcherError::InvalidWitness {
                    reference: edge.source.clone(),
                    part: "input binding identity",
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    // The host validates the full required closure above. Model evidence keeps
    // the target's direct acquisitions and this path, not every ancestor's
    // prerequisite chain repeated in every question.
    evidence.bindings.retain(|binding| {
        let [from, to] = binding.endpoints();
        match binding {
            Binding::Prerequisite { .. } => required.iter().any(|edge| {
                edge.source == from.capability
                    && edge.consumer == to.capability
                    && (edge.consumer == *target || route.contains(edge))
            }),
            Binding::InputSource { collect, .. } => input_edges.iter().any(|edge| {
                edge.provider == from.capability
                    && edge.consumer == to.capability
                    && edge.collect == *collect
                    && matches!(&from.slot, Slot::Field { field } if *field == edge.output_field)
                    && matches!(&to.slot, Slot::Input(input) if *input == edge.input)
            }),
            _ => false,
        }
    });
    for edge in route
        .iter()
        .filter(|edge| edge.relationship == Relationship::MayCreateResource)
    {
        let entity = index.candidates[&edge.source].document.entity.clone();
        evidence.bindings.push(Binding::MayCreateResource {
            from: Endpoint {
                capability: edge.source.clone(),
                slot: Slot::Entity {
                    entity: entity.clone(),
                },
            },
            to: Endpoint {
                capability: edge.consumer.clone(),
                slot: Slot::Entity { entity },
            },
        });
    }
    let candidate = &index.candidates[target];
    let question = Question {
        id: candidate.id.clone(),
        reference: target.clone(),
        entity: candidate.document.entity.clone(),
        operation: evidence.capabilities[target].operation.clone(),
    };
    let ids = &index.labels;
    let definitions = witnesses::value(&evidence.definitions)?;
    let scope = witnesses::project_path(target, &evidence, ids, &definitions)?;
    Ok((scope, question, members))
}

#[cfg(test)]
fn packet(
    index: &CatalogueSearch,
    target: &CapabilityRef,
    route: &[Edge<CapabilityRef>],
    context: SearchContext<'_>,
) -> Result<(IssuedBatch, BTreeSet<CapabilityRef>), DiscoveryMatcherError> {
    let (scope, question, members) = packet_scope(index, target, route, context)?;
    let issued = issue_scope(
        context.model,
        context.intent,
        &question,
        &std::sync::Arc::new(vec![question.clone()]),
        &index.labels,
        scope,
        false,
    )?;
    Ok((issued.into_issued(), members))
}

/// No fixed task panels or execution effects. Bounded identical transport retries
/// are charged and checkpointed before dispatch. The returned
/// retrieval contains only judged leaves; the search receipt accounts for the rest.
#[cfg(test)]
pub async fn select<F, Fut>(
    context: SearchContext<'_>,
    decide: F,
) -> Result<(RetrievalReceipt, CapabilityMatchReceipt, SearchReceipt), DiscoveryMatcherError>
where
    F: FnMut(IssuedBatch) -> Fut,
    Fut: std::future::Future<Output = Result<String, DecisionProviderError>>,
{
    let SearchContext {
        model,
        catalogs,
        deployments,
        allowed,
        ..
    } = context;
    if !crate::decision_codec::model_matches(RELEVANCE_MODEL, model) {
        return Err(DiscoveryMatcherError::UnsupportedModel);
    }
    let index = CatalogueSearch::new(catalogs, deployments, allowed)?;
    select_indexed(context, &index, decide, |_| async { Ok(()) }).await
}

/// Explicit lexical component fixture. Production must supply hybrid recall.
#[cfg(test)]
pub(crate) async fn select_indexed<F, Fut, O, OFut>(
    context: SearchContext<'_>,
    index: &CatalogueSearch,
    decide: F,
    observe: O,
) -> Result<(RetrievalReceipt, CapabilityMatchReceipt, SearchReceipt), DiscoveryMatcherError>
where
    F: FnMut(IssuedBatch) -> Fut,
    Fut: std::future::Future<Output = Result<String, DecisionProviderError>>,
    O: FnMut(SearchProgress) -> OFut,
    OFut: std::future::Future<Output = Result<(), DecisionProviderError>>,
{
    let recall = RetrievalReceipt {
        generation: context.generation.into(),
        candidates: index
            .ranking
            .rank(context.intent.current())
            .into_iter()
            .map(|r| index.candidates[&r].clone())
            .collect(),
        lexical_count: 0,
        vector_count: 0,
        lexical_truncated: false,
        vector_truncated: false,
    };
    // Component growth fixtures intentionally exceed the production recall bound.
    select_admitted(context, index, &recall, decide, observe).await
}

/// Public matcher boundary requires explicit recall from the pinned generation.
pub async fn select_retrieved<F, Fut>(
    context: SearchContext<'_>,
    recall: &RetrievalReceipt,
    operations: operations::OperationSet,
    decide: F,
) -> Result<(RetrievalReceipt, CapabilityMatchReceipt, SearchReceipt), DiscoveryMatcherError>
where
    F: FnMut(operations::DecisionBatch) -> Fut,
    Fut: std::future::Future<Output = Result<String, DecisionProviderError>>,
{
    let index = CatalogueSearch::new(context.catalogs, context.deployments, context.allowed)?;
    let authorized = index.authorized_capabilities(context.allowed);
    let selection_request = context.intent.selection_request();
    let request = operations::RequestIdentity::from_request(&selection_request);
    let scope = operations::MatrixScope::new(
        "isolated-selection",
        context.generation,
        &authorized,
        request,
    )?;
    let mut matrix = operations::OperationMatrix::new(
        scope,
        &selection_request,
        operations,
        &authorized,
        recall
            .candidates
            .iter()
            .map(|candidate| candidate.reference.clone())
            .collect(),
    )?;
    select_operations_indexed(
        context,
        &index,
        recall,
        &mut matrix,
        &BTreeMap::new(),
        decide,
        |_| async { Ok(()) },
    )
    .await
}

#[cfg(test)]
pub(crate) async fn select_recalled_indexed<F, Fut, O, OFut>(
    context: SearchContext<'_>,
    index: &CatalogueSearch,
    recall: &RetrievalReceipt,
    decide: F,
    observe: O,
) -> Result<(RetrievalReceipt, CapabilityMatchReceipt, SearchReceipt), DiscoveryMatcherError>
where
    F: FnMut(IssuedBatch) -> Fut,
    Fut: std::future::Future<Output = Result<String, DecisionProviderError>>,
    O: FnMut(SearchProgress) -> OFut,
    OFut: std::future::Future<Output = Result<(), DecisionProviderError>>,
{
    validate_recall(context, index, recall)?;
    select_admitted(context, index, recall, decide, observe).await
}

fn validate_recall(
    context: SearchContext<'_>,
    index: &CatalogueSearch,
    recall: &RetrievalReceipt,
) -> Result<(), DiscoveryMatcherError> {
    recall.validate(context.allowed)?;
    if recall.generation != context.generation {
        return Err(DiscoveryMatcherError::RetrievalGenerationMismatch);
    }
    for candidate in &recall.candidates {
        if !index
            .candidates
            .get(&candidate.reference)
            .is_some_and(|local| local.document == candidate.document)
        {
            return Err(DiscoveryMatcherError::InvalidWitness {
                reference: candidate.reference.clone(),
                part: "retrieval document differs from pinned CGS",
            });
        }
    }
    Ok(())
}

/// Plan against the same connected graph as discovery, then schedule only
/// operation pairs for admitted members. Pairs omitted for size remain
/// pending in the matrix and are never interpreted as negative judgments.
fn prepare_operation_wave(
    context: SearchContext<'_>,
    index: &CatalogueSearch,
    recall: &RetrievalReceipt,
    matrix: &mut operations::OperationMatrix,
    deferred_priorities: &BTreeMap<CapabilityRef, usize>,
    packet_limit: usize,
    cache: &mut connected::PacketCache,
) -> Result<(connected::Plan, Option<operations::DecisionBatch>), DiscoveryMatcherError> {
    validate_recall(context, index, recall)?;
    for reference in context.exposed {
        if !context.allowed.permits(reference) || !index.candidates.contains_key(reference) {
            return Err(DiscoveryMatcherError::InvalidWitness {
                reference: reference.clone(),
                part: "exposed capability authorization or catalogue identity",
            });
        }
    }
    let roots = recall
        .candidates
        .iter()
        .map(|candidate| candidate.reference.clone())
        .collect();
    let mut plan =
        connected::plan_recalled_with_deferred(index, context, roots, deferred_priorities, cache)?;
    let authorized = index
        .candidates
        .keys()
        .filter(|reference| context.allowed.permits(reference))
        .cloned()
        .collect();
    let mut admitted: Vec<_> = plan
        .wave
        .iter()
        .map(|(reference, _)| reference.clone())
        .collect();
    matrix.add_candidates(&authorized, &admitted)?;
    let mut witness_cache = operations::GraphWitnessCache::new();
    while !admitted.is_empty() {
        for limit in (1..=request_packing::MAX_QUESTIONS).rev() {
            let pairs = matrix.next_packet_for(&admitted, limit)?;
            if pairs.is_empty() {
                return Ok((plan, None));
            }
            let questions: Vec<_> = pairs
                .into_iter()
                .map(|(operation, capability)| operations::DecisionQuestion {
                    operation,
                    capability,
                })
                .collect();
            match operations::DecisionBatch::from_graph(
                context,
                matrix.operations(),
                matrix.scope(),
                index,
                &plan.wave,
                &questions,
                &mut witness_cache,
            ) {
                Ok(batch) if batch.body().len() <= packet_limit => return Ok((plan, Some(batch))),
                Ok(_) | Err(operations::OperationError::DecisionPacketTooLarge { .. }) => {}
                Err(error) => return Err(error.into()),
            }
            if limit > 1 {
                continue;
            }
            let reference = questions[0].capability.clone();
            plan.deferred
                .insert(reference.clone(), DeferredReason::InputBytes);
            admitted.retain(|candidate| candidate != &reference);
            break;
        }
    }
    Ok((plan, None))
}

#[cfg(test)]
async fn select_admitted<F, Fut, O, OFut>(
    context: SearchContext<'_>,
    index: &CatalogueSearch,
    recall: &RetrievalReceipt,
    mut decide: F,
    mut observe: O,
) -> Result<(RetrievalReceipt, CapabilityMatchReceipt, SearchReceipt), DiscoveryMatcherError>
where
    F: FnMut(IssuedBatch) -> Fut,
    Fut: std::future::Future<Output = Result<String, DecisionProviderError>>,
    O: FnMut(SearchProgress) -> OFut,
    OFut: std::future::Future<Output = Result<(), DecisionProviderError>>,
{
    let SearchContext {
        model,
        intent: _,
        generation,
        catalogs,
        deployments,
        allowed,
        exposed,
        ..
    } = context;
    if !crate::decision_codec::model_matches(RELEVANCE_MODEL, model) {
        return Err(DiscoveryMatcherError::UnsupportedModel);
    }
    for reference in exposed {
        if !allowed.permits(reference) || !index.candidates.contains_key(reference) {
            return Err(DiscoveryMatcherError::InvalidWitness {
                reference: reference.clone(),
                part: "exposed capability authorization or catalogue identity",
            });
        }
    }
    let all_roots = recall
        .candidates
        .iter()
        .map(|candidate| candidate.reference.clone())
        .collect();
    let planned = connected::plan_recalled(index, context, all_roots)?;
    let mut wave = planned.wave;
    let mut routes = BTreeMap::<CapabilityRef, Vec<Edge<CapabilityRef>>>::new();
    let mut judged = BTreeMap::new();
    let mut budget = SearchBudget::default();
    let deferred = planned.deferred;
    let mut prepared = planned.issued;
    while !wave.is_empty() {
        let issued: Vec<_> = prepared.take().into_iter().collect();
        if issued.is_empty() {
            break;
        }
        budget
            .reserve(
                planned.members.clone(),
                &issued.iter().map(IssuedBatch::body).collect::<Vec<_>>(),
            )
            .map_err(|_| DiscoveryMatcherError::InvalidWitness {
                reference: wave[0].0.clone(),
                part: "connected admission changed after selection",
            })?;
        let mut pending: BTreeSet<_> = issued
            .iter()
            .flat_map(|request| request.bindings.values())
            .filter_map(|id| {
                wave.iter()
                    .map(|(reference, _)| reference)
                    .find(|r| &index.candidates[*r].id == id)
                    .cloned()
            })
            .collect();
        observe(progress(
            index,
            exposed,
            &budget,
            &judged,
            ProgressFrontier {
                routes: &routes,
                deferred: &deferred,
            },
            pending.clone(),
            ProgressState::Admitted,
        ))
        .await
        .map_err(DiscoveryMatcherError::Checkpoint)?;
        for request in issued {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            let raw = loop {
                let response = if tokio::time::Instant::now() >= deadline {
                    Err(Box::new(
                        plasm_runtime::provider_transport::ProviderTransportError::DeadlineExpired,
                    ) as DecisionProviderError)
                } else {
                    tokio::time::timeout_at(deadline, decide(request.clone()))
                        .await
                        .unwrap_or_else(|_| {
                            Err(Box::new(
                                plasm_runtime::provider_transport::ProviderTransportError::DeadlineExpired,
                            ) as DecisionProviderError)
                        })
                };
                match response {
                    Ok(raw) => break raw,
                    Err(error) => {
                        budget.record_usage(
                            error
                                .downcast_ref::<ReceivedResponseFailure>()
                                .and_then(|failure| failure.input_tokens),
                        );
                        let delay = error
                            .downcast_ref::<plasm_runtime::provider_transport::ProviderTransportError>()
                            .and_then(|error| error.retry_delay(budget.requests()))
                            .filter(|delay| {
                                budget.requests() < search_budget::MAX_ATTEMPTS
                                    && budget.reported_input_tokens()
                                        <= search_budget::MAX_REPORTED_INPUT_TOKENS
                                    && *delay
                                        < deadline
                                            .saturating_duration_since(tokio::time::Instant::now())
                            });
                        observe(progress(
                            index,
                            exposed,
                            &budget,
                            &judged,
                            ProgressFrontier {
                                routes: &routes,
                                deferred: &deferred,
                            },
                            pending.clone(),
                            if delay.is_some() {
                                ProgressState::Admitted
                            } else {
                                failure_state(error.as_ref())
                            },
                        ))
                        .await
                        .map_err(DiscoveryMatcherError::Checkpoint)?;
                        let Some(delay) = delay else {
                            return Err(DiscoveryMatcherError::Provider(error));
                        };
                        tokio::time::sleep(delay).await;
                        if budget.reserve_retry(request.body()).is_err() {
                            return Err(DiscoveryMatcherError::Provider(error));
                        }
                        observe(progress(
                            index,
                            exposed,
                            &budget,
                            &judged,
                            ProgressFrontier {
                                routes: &routes,
                                deferred: &deferred,
                            },
                            pending.clone(),
                            ProgressState::Admitted,
                        ))
                        .await
                        .map_err(DiscoveryMatcherError::Checkpoint)?;
                    }
                }
            };
            budget.record_usage(reported_input_tokens(&raw));
            let answers = match decode_batch(&request, &raw) {
                Ok(answers) => answers,
                Err(error) => {
                    observe(progress(
                        index,
                        exposed,
                        &budget,
                        &judged,
                        ProgressFrontier {
                            routes: &routes,
                            deferred: &deferred,
                        },
                        pending.clone(),
                        failure_state(&error),
                    ))
                    .await
                    .map_err(DiscoveryMatcherError::Checkpoint)?;
                    return Err(error);
                }
            };
            for answer in answers {
                let answer = answer.matched;
                let (reference, route) = wave
                    .iter()
                    .find(|(r, _)| index.candidates[r].id == answer.capability_id)
                    .ok_or(DiscoveryMatcherError::MissingAnswer)?;
                if answer.choice != MatchChoice::Unrelated {
                    routes.insert(reference.clone(), route.clone());
                }
                pending.remove(reference);
                if judged.insert(reference.clone(), answer).is_some() {
                    return Err(DiscoveryMatcherError::DuplicateAnswer);
                }
            }
        }
        let retained: BTreeSet<_> = routes.keys().cloned().collect();
        let mut dependencies = required(&retained, catalogs, deployments, allowed)?;
        while !dependencies.is_empty() {
            let ready = dependencies
                .iter()
                .find(|edge| routes.contains_key(&edge.consumer))
                .cloned()
                .ok_or_else(|| DiscoveryMatcherError::InvalidWitness {
                    reference: dependencies.first().unwrap().consumer.clone(),
                    part: "required route ancestry",
                })?;
            dependencies.remove(&ready);
            let mut route = routes[&ready.consumer].clone();
            route.push(ready.clone());
            routes.entry(ready.source).or_insert(route);
        }
        observe(progress(
            index,
            exposed,
            &budget,
            &judged,
            ProgressFrontier {
                routes: &routes,
                deferred: &deferred,
            },
            pending,
            ProgressState::Settled,
        ))
        .await
        .map_err(DiscoveryMatcherError::Checkpoint)?;
        wave.clear();
    }
    let references: BTreeSet<_> = judged.keys().cloned().collect();
    // Question identities are revision-local reference hashes. Admission evidence
    // is joined by the fully qualified reference, never by an incidental row ID.
    let recalled: BTreeMap<_, _> = recall
        .candidates
        .iter()
        .map(|c| (&c.reference, c))
        .collect();
    let candidates: Vec<_> = references
        .iter()
        .map(|r| {
            let mut candidate = index.candidates[r].clone();
            candidate.admissions = recalled
                .get(r)
                .map(|c| c.admissions.clone())
                .unwrap_or_else(|| BTreeSet::from(["typed_relationship".into()]));
            candidate
        })
        .collect();
    let retrieval = RetrievalReceipt {
        generation: generation.into(),
        lexical_count: candidates
            .iter()
            .filter(|c| c.admissions.contains("lexical"))
            .count(),
        vector_count: candidates
            .iter()
            .filter(|c| c.admissions.contains("vector"))
            .count(),
        candidates,
        lexical_truncated: recall.lexical_truncated,
        vector_truncated: recall.vector_truncated,
    };
    let matching = CapabilityMatchReceipt {
        matches: judged.into_values().collect(),
        budget_retained: Vec::new(),
    };
    let search = SearchReceipt {
        policy: SEARCH_POLICY.into(),
        exposed: exposed.iter().cloned().collect(),
        considered: budget.members().clone(),
        unexplored: index
            .graph
            .authorized()
            .iter()
            .filter(|reference| {
                !exposed.contains(reference)
                    && !references.contains(*reference)
                    && !routes.contains_key(*reference)
            })
            .cloned()
            .collect(),
        deferred: deferred
            .into_iter()
            .map(|(reference, reason)| DeferredCandidate { reference, reason })
            .collect(),
        requests: budget.requests(),
        input_bytes: budget.input_bytes(),
        reported_input_tokens: budget.reported_input_tokens(),
        missing_usage: budget.missing_usage(),
    };
    Ok((retrieval, matching, search))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    pub(crate) fn fixture() -> BTreeMap<String, CGS> {
        let mut cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/discovery_reference_index"),
        )
        .unwrap();
        cgs.capabilities.get_mut("process").unwrap().description =
            "Produce the requested spectrum.".into();
        cgs.capabilities
            .get_mut("source_read")
            .unwrap()
            .inputs
            .receiver = Some(plasm_core::CapabilityReceiver::None);
        cgs.capabilities.get_mut("prepare").unwrap().kind =
            plasm_core::schema::CapabilityKind::Create;
        cgs.capabilities
            .get_mut("prepare")
            .unwrap()
            .provides
            .clear();
        cgs.capabilities
            .get_mut("prepare")
            .unwrap()
            .output_schema
            .as_mut()
            .unwrap()
            .output_type = plasm_core::schema::OutputType::SideEffect {
            description: "Creates prepared material; read it to obtain its value.".into(),
        };
        let mut read = cgs.capabilities["source_read"].clone();
        read.name = "prepared_read".into();
        read.domain = "Prepared".into();
        read.description = "Read prepared resources.".into();
        read.output_schema.as_mut().unwrap().output_type = plasm_core::schema::OutputType::Entity {
            entity_type: "Prepared".into(),
        };
        cgs.capabilities.insert(read.name.clone(), read);
        BTreeMap::from([("matrix".into(), cgs)])
    }

    pub(super) fn answer(request: &IssuedBatch, choice: &str) -> String {
        let body: serde_json::Value = serde_json::from_str(request.body()).unwrap();
        let answers: BTreeMap<_, _> = body["questions"]
            .as_object()
            .unwrap()
            .keys()
            .map(|key| {
                (
                    key.clone(),
                    json!({"type":"choice", "choice":choice, "confidence":1.0,
                "probabilities":{"relevant":if choice == "relevant" {1.0} else {0.0},
                "uncertain":if choice == "uncertain" {1.0} else {0.0},
                "unrelated":if choice == "unrelated" {1.0} else {0.0}}}),
                )
            })
            .collect();
        json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","answers":answers,"usage":{"input_tokens":100}}).to_string()
    }

    #[tokio::test]
    async fn byte_admission_preserves_relevance_order_instead_of_identifier_order() {
        let mut catalogs = fixture();
        let cgs = catalogs.get_mut("matrix").unwrap();
        let source = cgs.capabilities["source_read"].clone();
        for (name, repetitions) in [("a_secondary", 1), ("z_primary", 8)] {
            let mut cap = source.clone();
            cap.name = name.into();
            cap.description = format!(
                "{} {}",
                "spectrometer ".repeat(repetitions),
                "neutral ".repeat(2100)
            );
            cgs.capabilities.insert(name.into(), cap);
        }
        let (retrieval, _, receipt) = select(
            SearchContext {
                visits: &Default::default(),
                exposed: &[],
                model: RELEVANCE_MODEL,
                intent: &IntentProvenance::from_turns(["spectrometer".into()]).unwrap(),
                generation: "ordering",
                catalogs: &catalogs,
                deployments: &Default::default(),
                allowed: &DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()])),
            },
            |request| async move { Ok(answer(&request, "unrelated")) },
        )
        .await
        .unwrap();
        let names: Vec<_> = retrieval
            .candidates
            .iter()
            .map(|c| c.reference.capability.as_str())
            .collect();
        assert_eq!(names, ["z_primary"]);
        assert!(receipt
            .deferred
            .iter()
            .any(|d| d.reference.capability == "a_secondary"));
        assert!(receipt.input_bytes <= search_budget::MAX_INPUT_BYTES);
    }

    #[tokio::test]
    async fn live_roots_nominate_read_then_creation_without_whole_catalogue_packets() {
        let catalogs = fixture();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let captured = calls.clone();
        let (retrieval, matching, search) = select(
            SearchContext {
                visits: &Default::default(),
                exposed: &[],
                model: RELEVANCE_MODEL,
                intent: &IntentProvenance::from_turns(["spectrum".into()]).unwrap(),
                generation: "revision",
                catalogs: &catalogs,
                deployments: &Default::default(),
                allowed: &DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()])),
            },
            move |request| {
                captured.lock().unwrap().push(request.body().to_owned());
                async move { Ok(answer(&request, "relevant")) }
            },
        )
        .await
        .unwrap();
        let selected: BTreeSet<_> = matching
            .selected(&retrieval)
            .into_iter()
            .map(|r| r.capability)
            .collect();
        assert!(selected.contains("process"));
        assert!(selected.contains("prepared_read"), "{search:?}");
        assert!(
            selected.contains("prepare"),
            "creation nomination was lost: {search:?}"
        );
        assert_eq!(search.requests, 1);
        assert!(search.input_bytes <= 16_000);
        let bodies = calls.lock().unwrap();
        assert_eq!(bodies.len(), search.requests);
        assert_eq!(
            bodies.iter().map(String::len).sum::<usize>(),
            search.input_bytes
        );
        assert!(bodies[0].contains("prepared_read"));
        assert!(
            bodies
                .iter()
                .any(|body| body.contains("may_create_resource")),
            "creation route absent: {search:?}"
        );
        assert!(bodies
            .iter()
            .all(|body| !body.contains("Apply the configured routine")));
    }

    #[tokio::test]
    async fn caller_supplied_receiver_does_not_require_a_discovered_producer() {
        let mut catalogs = fixture();
        catalogs
            .get_mut("matrix")
            .unwrap()
            .capabilities
            .get_mut("prepared_read")
            .unwrap()
            .inputs
            .receiver = Some(plasm_core::CapabilityReceiver::Entity {
            entity: "Prepared".into(),
        });
        let (retrieval, _, search) = select(
            SearchContext {
                visits: &Default::default(),
                exposed: &[],
                model: RELEVANCE_MODEL,
                intent: &IntentProvenance::from_turns(["Read prepared resources.".into()]).unwrap(),
                generation: "caller-receiver",
                catalogs: &catalogs,
                deployments: &Default::default(),
                allowed: &DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()])),
            },
            |request| async move { Ok(answer(&request, "relevant")) },
        )
        .await
        .unwrap();
        assert!(retrieval
            .candidates
            .iter()
            .any(|c| c.reference.capability == "prepared_read"));
        assert!(!search
            .unexplored
            .iter()
            .any(|key| key.capability == "prepared_read"));
    }

    #[tokio::test]
    async fn oversized_complete_contract_is_deferred_without_provider_or_exposure() {
        let mut catalogs = fixture();
        catalogs
            .get_mut("matrix")
            .unwrap()
            .capabilities
            .get_mut("process")
            .unwrap()
            .description = "spectrum ".repeat(20_000);
        let (retrieval, matching, search) = select(
            SearchContext {
                visits: &Default::default(),
                exposed: &[],
                model: RELEVANCE_MODEL,
                intent: &IntentProvenance::from_turns(["spectrum".into()]).unwrap(),
                generation: "revision",
                catalogs: &catalogs,
                deployments: &Default::default(),
                allowed: &DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()])),
            },
            |_| async { panic!("oversized contract must not dispatch") },
        )
        .await
        .unwrap();
        assert!(retrieval.candidates.is_empty());
        assert!(matching.matches.is_empty());
        assert_eq!(search.requests, 0);
        assert!(search.unexplored.iter().any(|r| r.capability == "process"));
        assert!(matches!(
            search.deferred[0].reason,
            DeferredReason::InputBytes
        ));
    }

    #[tokio::test]
    async fn unselected_catalogue_prerequisites_do_not_abort_authorized_search() {
        let mut catalogs = fixture();
        let unrelated = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/prerequisite_matrix"),
        )
        .unwrap();
        catalogs.insert("unrelated".into(), unrelated);
        let (retrieval, _, search) = select(
            SearchContext {
                visits: &Default::default(),
                exposed: &[],
                model: RELEVANCE_MODEL,
                intent: &IntentProvenance::from_turns(["spectrum".into()]).unwrap(),
                generation: "revision",
                catalogs: &catalogs,
                deployments: &Default::default(),
                allowed: &DiscoveryAuthorization::catalogs(BTreeSet::from([
                    "matrix".into(),
                    "unrelated".into(),
                ])),
            },
            |request| async move { Ok(answer(&request, "unrelated")) },
        )
        .await
        .unwrap();
        assert!(!retrieval.candidates.is_empty());
        assert!(retrieval
            .candidates
            .iter()
            .all(|candidate| candidate.reference.catalog == "matrix"));
        assert_eq!(search.requests, 1);
        assert!(search.unexplored.iter().any(|r| r.catalog == "unrelated"));
    }

    #[tokio::test]
    async fn opaque_uncertainty_is_retained_without_expanding_unrelated_peers() {
        let catalogs = fixture();
        let (retrieval, matching, search) = select(
            SearchContext {
                visits: &Default::default(),
                exposed: &[],
                model: RELEVANCE_MODEL,
                intent: &IntentProvenance::from_turns(["configured routine".into()]).unwrap(),
                generation: "revision",
                catalogs: &catalogs,
                deployments: &Default::default(),
                allowed: &DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()])),
            },
            |request| async move { Ok(answer(&request, "uncertain")) },
        )
        .await
        .unwrap();
        assert_eq!(
            matching
                .selected(&retrieval)
                .iter()
                .map(|r| r.capability.as_str())
                .collect::<Vec<_>>(),
            vec!["opaque"]
        );
        assert_eq!(search.requests, 1);
        assert!(search.unexplored.iter().any(|r| r.capability == "process"));
    }

    #[tokio::test]
    async fn connected_supplier_after_required_acquisition_keeps_edge_roles() {
        let mut cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/prerequisite_matrix"),
        )
        .unwrap();
        cgs.capabilities.get_mut("read").unwrap().description = "Observe the spectrum".into();
        let mut supplier = cgs.capabilities["read"].clone();
        supplier.name = "supplier".into();
        supplier.description = "Read account identifiers".into();
        supplier.inputs.selection.0.clear();
        cgs.capabilities.insert(supplier.name.clone(), supplier);
        let catalogs = BTreeMap::from([("matrix".into(), cgs)]);
        let deployments = DeploymentBindings {
            bindings: vec![plasm_core::prerequisites::DeploymentBinding {
                consumer: CapabilityRef {
                    catalog: "matrix".into(),
                    capability: "read".into(),
                },
                requirement: "scoped_access".into(),
                provider_catalog: "matrix".into(),
                provider: "value_source".into(),
            }],
        };
        let (retrieval, matching, search) = select(
            SearchContext {
                visits: &Default::default(),
                exposed: &[],
                model: RELEVANCE_MODEL,
                intent: &IntentProvenance::from_turns(["spectrum".into()]).unwrap(),
                generation: "required-then-optional",
                catalogs: &catalogs,
                deployments: &deployments,
                allowed: &DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()])),
            },
            |request| async move { Ok(answer(&request, "relevant")) },
        )
        .await
        .unwrap();
        assert!(matching
            .selected(&retrieval)
            .iter()
            .any(|key| key.capability == "supplier"));
        assert!(search
            .considered
            .iter()
            .any(|key| key.capability == "acquire"));
        assert_eq!(search.requests, 1);
        assert!(search.input_bytes <= 16_000);
    }

    #[tokio::test]
    async fn required_acquisition_is_charged_and_authorized_before_dispatch() {
        let cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/prerequisite_matrix"),
        )
        .unwrap();
        let catalogs = BTreeMap::from([("matrix".into(), cgs)]);
        let read = CapabilityRef {
            catalog: "matrix".into(),
            capability: "read".into(),
        };
        let acquire = CapabilityRef {
            catalog: "matrix".into(),
            capability: "acquire".into(),
        };
        let deployments = DeploymentBindings {
            bindings: vec![plasm_core::prerequisites::DeploymentBinding {
                consumer: read.clone(),
                requirement: "scoped_access".into(),
                provider_catalog: "matrix".into(),
                provider: "value_source".into(),
            }],
        };
        let intent = IntentProvenance::from_turns(["Read".into()]).unwrap();
        for permit_acquire in [false, true] {
            let mut capabilities = BTreeSet::from(["read".into()]);
            if permit_acquire {
                capabilities.insert("acquire".into());
            }
            let allowed = DiscoveryAuthorization {
                catalogs: BTreeSet::from(["matrix".into()]),
                capabilities: BTreeMap::from([("matrix".into(), capabilities)]),
            };
            let result = select(
                SearchContext {
                    visits: &Default::default(),
                    exposed: &[],
                    model: RELEVANCE_MODEL,
                    intent: &intent,
                    generation: "revision",
                    catalogs: &catalogs,
                    deployments: &deployments,
                    allowed: &allowed,
                },
                |request| async move {
                    assert!(permit_acquire, "unauthorized acquisition reached provider");
                    Ok(answer(&request, "relevant"))
                },
            )
            .await;
            if permit_acquire {
                let (retrieval, matching, search) = result.unwrap();
                assert_eq!(matching.selected(&retrieval), vec![read.clone()]);
                assert!(search.considered.contains(&acquire));
                assert!(!search.unexplored.contains(&acquire));
            } else {
                assert!(matches!(
                    result,
                    Err(DiscoveryMatcherError::UnauthorizedWitness(_))
                ));
            }
        }
    }

    #[tokio::test]
    async fn checkpoint_precedes_dispatch_and_preserves_failed_request_budget() {
        let catalogs = fixture();
        let deployments = Default::default();
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let intent = IntentProvenance::from_turns(["spectrum".into()]).unwrap();
        let context = SearchContext {
            visits: &Default::default(),
            exposed: &[],
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "revision",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
        };
        let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
        let checkpoints = Arc::new(Mutex::new(Vec::<SearchProgress>::new()));
        let before_dispatch = checkpoints.clone();
        let saved = checkpoints.clone();
        let error = select_indexed(
            context,
            &index,
            move |_| {
                let snapshots = before_dispatch.lock().unwrap();
                let latest = snapshots.last().expect("admission must precede dispatch");
                assert!(matches!(latest.state, ProgressState::Admitted));
                assert_eq!(latest.accounting.requests, 1);
                assert!(latest.accounting.input_bytes > 0);
                assert!(!latest.pending.is_empty());
                async {
                    Err(Box::new(std::io::Error::other("abstract failure"))
                        as DecisionProviderError)
                }
            },
            move |snapshot| {
                saved.lock().unwrap().push(snapshot);
                async { Ok(()) }
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, DiscoveryMatcherError::Provider(_)));
        let snapshots = checkpoints.lock().unwrap();
        assert_eq!(snapshots.len(), 2);
        let failed = snapshots.last().unwrap();
        assert!(matches!(failed.state, ProgressState::Failed { .. }));
        assert_eq!(failed.accounting.requests, 1);
        assert_eq!(failed.accounting.missing_usage, 1);
        assert_eq!(
            failed.accounting.input_bytes,
            snapshots[0].accounting.input_bytes
        );
        assert!(failed.judgments.is_empty());
        let encoded = serde_json::to_string(failed).unwrap();
        let restored: SearchProgress = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored.accounting.requests, 1);
        assert_eq!(restored.pending, failed.pending);
    }

    #[tokio::test]
    async fn checkpoint_failure_prevents_provider_dispatch() {
        let catalogs = fixture();
        let deployments = Default::default();
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let intent = IntentProvenance::from_turns(["spectrum".into()]).unwrap();
        let context = SearchContext {
            visits: &Default::default(),
            exposed: &[],
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "revision",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
        };
        let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
        let error = select_indexed(
            context,
            &index,
            |_| async { panic!("unpersisted admission must not dispatch") },
            |_| async {
                Err(Box::new(std::io::Error::other("abstract storage failure"))
                    as DecisionProviderError)
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, DiscoveryMatcherError::Checkpoint(_)));
    }

    #[tokio::test]
    async fn provider_failure_is_not_retried() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let error = select(
            SearchContext {
                visits: &Default::default(),
                exposed: &[],
                model: RELEVANCE_MODEL,
                intent: &IntentProvenance::from_turns(["spectrum".into()]).unwrap(),
                generation: "revision",
                catalogs: &fixture(),
                deployments: &Default::default(),
                allowed: &DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()])),
            },
            move |_| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async {
                    Err(Box::new(std::io::Error::other("abstract provider fault"))
                        as DecisionProviderError)
                }
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, DiscoveryMatcherError::Provider(_)));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
