//! One semantic relevance judgment over authorized candidates and typed evidence.
use crate::decision_codec::DecisionCodec;
pub(crate) mod catalogue_search;
mod decisions;
pub mod evidence;
pub mod online;
mod request_packing;
mod search_budget;
mod search_graph;
#[cfg(test)]
mod search_rank;
mod witnesses;
use crate::discovery_store::RetrievalReceipt;
use crate::intent_provenance::IntentProvenance;
pub(crate) use decisions::ENDPOINT as RELEVANCE_ENDPOINT;
use evidence::CatalogEvidence;
use futures_util::{stream::FuturesUnordered, StreamExt};
use plasm_core::{catalog_discovery::content_hash, prerequisites::CapabilityRef};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub(crate) type DecisionProviderError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug, Error)]
pub enum DiscoveryMatcherError {
    #[error("discovery recall is invalid")]
    Retrieval(#[from] crate::discovery_store::RetrievalValidationError),
    #[error("discovery recall belongs to a different generation")]
    RetrievalGenerationMismatch,
    #[error("connected discovery questions must have identical model and intent context")]
    InvalidConnectedContext,
    #[error("discovery catalogue context is invalid")]
    CatalogueContext(#[source] Box<plasm_core::catalog_discovery::CatalogDiscoveryError>),
    #[error("pinned catalogue evidence is missing for {0:?}")]
    MissingCatalogueEvidence(CapabilityRef),
    #[error("catalogue {catalog} is missing referenced definition {name}")]
    MissingEvidenceDefinition { catalog: String, name: String },
    #[error("discovery binding projection failed")]
    Witness(#[from] plasm_core::prerequisites::PrerequisiteError),
    #[error("discovery witness references unauthorized capability {0}")]
    UnauthorizedWitness(CapabilityRef),
    #[error("discovery witness for {reference} has invalid {part}; no evidence was removed")]
    InvalidWitness {
        reference: CapabilityRef,
        part: &'static str,
    },
    #[error("discovery scope for {reference} requires {capabilities}/{capability_limit} capabilities and {bytes}/{byte_limit} bytes; selection was not started; narrow the discovery need or catalogue bindings")]
    ScopeLimit {
        reference: CapabilityRef,
        capabilities: usize,
        capability_limit: usize,
        bytes: usize,
        byte_limit: usize,
    },
    #[error("relevance model is required")]
    MissingModel,
    #[error("capability relevance requires openai/gpt-6-luna-decisions or a dated revision")]
    UnsupportedModel,
    #[error("judgment candidate bound exceeded")]
    CandidateBoundExceeded,
    #[error("duplicate discovery candidate id")]
    DuplicateCandidate,
    #[error("duplicate discovery capability reference")]
    DuplicateCapabilityReference,
    #[error("relevance batch is empty")]
    EmptyBatch,
    #[error("issued relevance page has no pending page")]
    NoPendingPage,
    #[error(
        "provider rejected a bounded discovery packet ({bytes} bytes); no selection was committed"
    )]
    IndivisibleItem { bytes: usize },
    #[error("relevance request serialization failed")]
    Encode(#[source] crate::decision_codec::DecisionCodecError),
    #[error("issued discovery request could not be packed")]
    Packing(#[source] serde_json::Error),
    #[error("decision response is invalid")]
    Response(#[from] crate::decision_codec::ProviderResponseError),
    #[error("validated relevance answer is missing its issued question")]
    MissingAnswer,
    #[error("an accepted candidate has multiple relevance judgments")]
    MultipleJudgments,
    #[error("relevance page has unprocessed questions")]
    UnprocessedPages,
    #[error("duplicate issued relevance question")]
    DuplicateIssuedQuestion,
    #[error("duplicate relevance answer")]
    DuplicateAnswer,
    #[error("relevance answers differ from issued questions")]
    AnswerSetMismatch,
    #[error("relevance receipt does not cover the candidate universe")]
    CandidateCoverageMismatch,
    #[error("provider decision failed")]
    Provider(#[source] DecisionProviderError),
    #[error("discovery progress persistence failed")]
    Checkpoint(#[source] DecisionProviderError),
}

impl From<plasm_core::catalog_discovery::CatalogDiscoveryError> for DiscoveryMatcherError {
    fn from(error: plasm_core::catalog_discovery::CatalogDiscoveryError) -> Self {
        Self::CatalogueContext(Box::new(error))
    }
}

pub const RELEVANCE_MODEL: &str = "openai/gpt-6-luna-decisions";
// Local requests include the complete target and direct compact witnesses.
#[cfg(test)]
const TARGET_PAGE_BYTES: usize = witnesses::MAX_BYTES;
#[cfg(test)]
const MAX_PAGE_QUESTIONS: usize = 1;
/// Bounds work admitted by one discovery turn.
const ROUTE_CONCURRENCY: usize = 4;
const CRITERIA: [(&str, &str); 3] = [
    (
        "relevant",
        "The documented output, effect or information contributes to the requested outcome, potentially through other operations.",
    ),
    (
        "unrelated",
        "The documented output, effect or information serves a different outcome or the reverse transformation.",
    ),
    (
        "uncertain",
        "The documented output, effect or information is insufficiently specified to establish contribution.",
    ),
];
const RUBRIC: &str = "Could the documented information, output or effect help achieve the requested outcome, directly or through available capabilities?";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MatchChoice {
    Unrelated,
    Uncertain,
    Relevant,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityIntentMatch {
    pub capability_id: String,
    pub choice: MatchChoice,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityMatchReceipt {
    pub matches: Vec<CapabilityIntentMatch>,
    /// Host budget outcomes, not provider judgments or probabilities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub budget_retained: Vec<ScopeBudgetRetention>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeBudgetRetention {
    pub capability_id: String,
    pub reference: CapabilityRef,
    pub capabilities: usize,
    pub capability_limit: usize,
    pub bytes: usize,
    pub byte_limit: usize,
}

/// All scopes are validated before dispatch. Only fitting requests can reach a
/// provider; valid oversized candidates are retained with host-owned evidence.
pub struct PreparedSelection {
    batches: Vec<IssuedBatch>,
    budget_retained: Vec<ScopeBudgetRetention>,
}
impl PreparedSelection {
    pub fn batches(&self) -> &[IssuedBatch] {
        &self.batches
    }
    pub fn budget_retained(&self) -> &[ScopeBudgetRetention] {
        &self.budget_retained
    }
}
impl CapabilityMatchReceipt {
    /// Exposure preserves unresolved relevance. A judgment of uncertainty is not
    /// evidence for excluding a capability; it remains uncertain in this receipt.
    /// Retrieval authorization and execution admission remain separate boundaries.
    pub fn selected(&self, retrieval: &RetrievalReceipt) -> Vec<CapabilityRef> {
        let ids: BTreeSet<_> = self
            .matches
            .iter()
            .filter(|m| matches!(m.choice, MatchChoice::Relevant | MatchChoice::Uncertain))
            .map(|m| &m.capability_id)
            .collect();
        retrieval
            .candidates
            .iter()
            .filter(|c| ids.contains(&c.id))
            .map(|c| c.reference.clone())
            .collect()
    }
}
#[derive(Debug, Clone)]
pub struct IssuedBatch {
    body: String,
    cache_key: String,
    model: String,
    bindings: BTreeMap<String, String>,
    #[cfg(test)]
    questions: Vec<Question>,
    #[cfg(test)]
    evidence: std::sync::Arc<Vec<Question>>,
}
impl IssuedBatch {
    pub fn body(&self) -> &str {
        &self.body
    }
    pub fn cache_key(&self) -> &str {
        &self.cache_key
    }
}
/// An issued question owns its exact semantic document and local result identity.
/// Graph witnesses remain in the routing receipt, not the relevance wire contract.
#[derive(Debug, Clone, Serialize)]
struct Question {
    id: String,
    reference: CapabilityRef,
    entity: String,
    operation: plasm_core::catalog_discovery::OperationEvidence,
}

#[cfg(test)]
pub(crate) fn issue_batches(
    model: &str,
    intent: &IntentProvenance,
    retrieval: &RetrievalReceipt,
    catalog_evidence: &CatalogEvidence,
) -> Result<Vec<IssuedBatch>, DiscoveryMatcherError> {
    let prepared = prepare_local_selection(model, intent, retrieval, catalog_evidence)?;
    // A strict request-only projection must not silently lose local outcomes.
    // Execution paths consume PreparedSelection instead.
    if let Some(budget) = prepared.budget_retained.first() {
        return Err(DiscoveryMatcherError::ScopeLimit {
            reference: budget.reference.clone(),
            capabilities: budget.capabilities,
            capability_limit: budget.capability_limit,
            bytes: budget.bytes,
            byte_limit: budget.byte_limit,
        });
    }
    Ok(prepared.batches)
}

pub fn prepare_selection(
    model: &str,
    intent: &IntentProvenance,
    retrieval: &RetrievalReceipt,
    catalog_evidence: &CatalogEvidence,
) -> Result<PreparedSelection, DiscoveryMatcherError> {
    let mut prepared = prepare_local_selection(model, intent, retrieval, catalog_evidence)?;
    prepared.batches = request_packing::pack(prepared.batches)?;
    Ok(prepared)
}

fn prepare_local_selection(
    model: &str,
    intent: &IntentProvenance,
    retrieval: &RetrievalReceipt,
    catalog_evidence: &CatalogEvidence,
) -> Result<PreparedSelection, DiscoveryMatcherError> {
    if model.trim().is_empty() {
        return Err(DiscoveryMatcherError::MissingModel);
    }
    if !crate::decision_codec::model_matches(RELEVANCE_MODEL, model) {
        return Err(DiscoveryMatcherError::UnsupportedModel);
    }
    if retrieval.candidates.len() > crate::discovery_store::CANDIDATE_LIMIT {
        return Err(DiscoveryMatcherError::CandidateBoundExceeded);
    }
    let mut candidates: Vec<_> = retrieval.candidates.iter().collect();
    candidates.sort_by(|a, b| a.reference.cmp(&b.reference));
    if candidates
        .iter()
        .map(|c| &c.id)
        .collect::<BTreeSet<_>>()
        .len()
        != candidates.len()
    {
        return Err(DiscoveryMatcherError::DuplicateCandidate);
    }
    if candidates
        .iter()
        .map(|c| &c.reference)
        .collect::<BTreeSet<_>>()
        .len()
        != candidates.len()
    {
        return Err(DiscoveryMatcherError::DuplicateCapabilityReference);
    }
    let evidence: std::sync::Arc<Vec<Question>> = std::sync::Arc::new(
        candidates
            .iter()
            .map(|candidate| {
                let typed = catalog_evidence
                    .capabilities
                    .get(&candidate.reference)
                    .ok_or_else(|| {
                        DiscoveryMatcherError::MissingCatalogueEvidence(candidate.reference.clone())
                    })?;
                Ok(Question {
                    id: candidate.id.clone(),
                    reference: candidate.reference.clone(),
                    entity: candidate.document.entity.clone(),
                    operation: typed.operation.clone(),
                })
            })
            .collect::<Result<_, DiscoveryMatcherError>>()?,
    );
    // Validate every local packet before the caller can dispatch any provider work.
    let ids: BTreeMap<_, _> = catalog_evidence
        .capabilities
        .keys()
        .enumerate()
        .map(|(i, reference)| (reference.clone(), format!("c{i}")))
        .collect();
    let definitions = witnesses::value(&catalog_evidence.definitions)?;
    let mut prepared = PreparedSelection {
        batches: Vec::new(),
        budget_retained: Vec::new(),
    };
    for question in evidence.iter() {
        match issue_local_batch(
            model,
            intent,
            question,
            &evidence,
            catalog_evidence,
            &ids,
            &definitions,
        ) {
            Ok(batch) => prepared.batches.push(batch),
            Err(DiscoveryMatcherError::ScopeLimit {
                reference,
                capabilities,
                capability_limit,
                bytes,
                byte_limit,
            }) => {
                prepared.budget_retained.push(ScopeBudgetRetention {
                    capability_id: question.id.clone(),
                    reference,
                    capabilities,
                    capability_limit,
                    bytes,
                    byte_limit,
                });
            }
            Err(error) => return Err(error),
        }
    }
    Ok(prepared)
}

struct RelevanceCodec;
impl DecisionCodec for RelevanceCodec {
    type Request<'a> = decisions::Request<'a>;

    fn encode(
        request: &Self::Request<'_>,
    ) -> Result<String, crate::decision_codec::DecisionCodecError> {
        // Preserve the fixed evidence map order at the wire boundary.
        // The generic codec's Value roundtrip sorts maps and would erase it.
        Ok(serde_json::to_string(request)?)
    }
}
#[derive(Serialize)]
struct SharedEvidence<'a> {
    rule: &'static str,
    capabilities: witnesses::OrderedCards,
    discovery_need: &'a str,
    outcome: &'a str,
    types: witnesses::ProjectedDefinitions,
    type_refs: serde_json::Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    bindings: Vec<serde_json::Value>,
}

fn issue_local_batch(
    model: &str,
    intent: &IntentProvenance,
    question: &Question,
    _evidence: &std::sync::Arc<Vec<Question>>,
    catalog: &CatalogEvidence,
    ids: &BTreeMap<CapabilityRef, String>,
    definitions: &serde_json::Value,
) -> Result<IssuedBatch, DiscoveryMatcherError> {
    let scope = witnesses::project(&question.reference, catalog, ids, definitions)?;
    issue_projected_batch(model, intent, question, _evidence, ids, scope)
}

fn issue_projected_batch(
    model: &str,
    intent: &IntentProvenance,
    question: &Question,
    _evidence: &std::sync::Arc<Vec<Question>>,
    ids: &BTreeMap<CapabilityRef, String>,
    scope: witnesses::Scope,
) -> Result<IssuedBatch, DiscoveryMatcherError> {
    issue_scope(model, intent, question, _evidence, ids, scope, true)
}

fn local_question_instructions(id: &str) -> String {
    format!("Using the shared intent and rule, classify the contribution of `capabilities.{id}`. Other entries in `capabilities` are available composition evidence, not an execution plan.")
}

fn issue_scope(
    model: &str,
    intent: &IntentProvenance,
    question: &Question,
    _evidence: &std::sync::Arc<Vec<Question>>,
    ids: &BTreeMap<CapabilityRef, String>,
    scope: witnesses::Scope,
    admit_local: bool,
) -> Result<IssuedBatch, DiscoveryMatcherError> {
    let id = ids
        .get(&question.reference)
        .ok_or(DiscoveryMatcherError::MissingAnswer)?;
    let key = format!("q{}", &id[1..]);
    let state = SharedEvidence {
        rule: RUBRIC,
        capabilities: scope.cards,
        discovery_need: intent.current(),
        outcome: intent.current(),
        types: scope.types,
        type_refs: scope.references,
        bindings: scope.bindings,
    };
    let body = RelevanceCodec::encode(&decisions::Request {
        model,
        state: &state,
        questions: BTreeMap::from([(
            key.clone(),
            decisions::Question {
                kind: "choice",
                instructions: local_question_instructions(id),
                criteria: CRITERIA.into_iter().collect(),
            },
        )]),
    })
    .map_err(DiscoveryMatcherError::Encode)?;
    if admit_local
        && (scope.count > witnesses::MAX_CAPABILITIES || body.len() > witnesses::MAX_BYTES)
    {
        return Err(DiscoveryMatcherError::ScopeLimit {
            reference: question.reference.clone(),
            capabilities: scope.count,
            capability_limit: witnesses::MAX_CAPABILITIES,
            bytes: body.len(),
            byte_limit: witnesses::MAX_BYTES,
        });
    }
    Ok(IssuedBatch {
        cache_key: content_hash(format!("decisions-compact-witnesses-v10\\n{body}").as_bytes()),
        body,
        model: model.into(),
        bindings: BTreeMap::from([(key, question.id.clone())]),
        #[cfg(test)]
        questions: vec![question.clone()],
        #[cfg(test)]
        evidence: _evidence.clone(),
    })
}

/// Pending local questions and accepted judgments; no partial selection.
pub struct SelectionPages {
    pending: std::collections::VecDeque<IssuedBatch>,
    accepted: Vec<IssuedBatch>,
    answers: Vec<SelectionAnswer>,
    budget_retained: Vec<ScopeBudgetRetention>,
}

/// Execute complete local questions for live and cached decisions.
/// Any failed question aborts selection; evidence is never subdivided.
pub async fn judge_candidates<F, Fut>(
    model: &str,
    intent: &IntentProvenance,
    retrieval: &RetrievalReceipt,
    catalog_evidence: &CatalogEvidence,
    mut decide: F,
) -> Result<CapabilityMatchReceipt, DiscoveryMatcherError>
where
    F: FnMut(IssuedBatch) -> Fut,
    Fut: std::future::Future<Output = Result<String, DecisionProviderError>>,
{
    let prepared = prepare_selection(model, intent, retrieval, catalog_evidence)?;
    let mut pending: std::collections::VecDeque<_> = prepared.batches.into();
    let mut running = FuturesUnordered::new();
    let mut accepted = Vec::new();
    let mut answers = Vec::new();
    loop {
        while running.len() < ROUTE_CONCURRENCY {
            let Some(issued) = pending.pop_front() else {
                break;
            };
            let response = decide(issued.clone());
            running.push(async move { (issued, response.await) });
        }
        let Some((issued, result)) = running.next().await else {
            break;
        };
        match result {
            Ok(raw) => {
                answers.extend(decode_batch(&issued, &raw)?);
                accepted.push(issued);
            }
            Err(error)
                if error
                    .downcast_ref::<plasm_runtime::provider_transport::ProviderTransportError>()
                    .is_some_and(|error| {
                        matches!(
                            error,
                            plasm_runtime::provider_transport::ProviderTransportError::ContextLimit
                        )
                    }) =>
            {
                return Err(DiscoveryMatcherError::IndivisibleItem {
                    bytes: issued.body.len(),
                });
            }
            Err(error) => return Err(DiscoveryMatcherError::Provider(error)),
        }
    }
    finish_prepared_selection(answers, retrieval, &accepted, prepared.budget_retained)
}
impl SelectionPages {
    #[cfg(test)]
    pub fn new(batches: Vec<IssuedBatch>) -> Self {
        Self::from_prepared(PreparedSelection {
            batches,
            budget_retained: Vec::new(),
        })
    }
    pub fn from_prepared(prepared: PreparedSelection) -> Self {
        Self {
            pending: prepared.batches.into(),
            accepted: Vec::new(),
            answers: Vec::new(),
            budget_retained: prepared.budget_retained,
        }
    }
    /// Total local questions, including completed ones.
    pub fn page_count(&self) -> usize {
        self.accepted.len() + self.pending.len()
    }
    pub fn current(&self) -> Option<&IssuedBatch> {
        self.pending.front()
    }
    pub fn accept(&mut self, raw: &str) -> Result<(), DiscoveryMatcherError> {
        let issued = self
            .pending
            .front()
            .ok_or(DiscoveryMatcherError::NoPendingPage)?;
        let answers = decode_batch(issued, raw)?;
        self.answers.extend(answers);
        self.accepted
            .push(self.pending.pop_front().expect("validated pending page"));
        Ok(())
    }
    pub fn finish(
        self,
        retrieval: &RetrievalReceipt,
    ) -> Result<CapabilityMatchReceipt, DiscoveryMatcherError> {
        if !self.pending.is_empty() {
            return Err(DiscoveryMatcherError::UnprocessedPages);
        }
        finish_prepared_selection(
            self.answers,
            retrieval,
            &self.accepted,
            self.budget_retained,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct QuestionId(String);
#[derive(Debug, Clone)]
pub struct SelectionAnswer {
    question: QuestionId,
    matched: CapabilityIntentMatch,
}
fn question_id(batch: &IssuedBatch, key: &str) -> QuestionId {
    QuestionId(format!("{}/{key}", batch.cache_key))
}
pub fn decode_batch(
    issued: &IssuedBatch,
    raw: &str,
) -> Result<Vec<SelectionAnswer>, DiscoveryMatcherError> {
    let mut answers = decisions::decode(&issued.model, issued.bindings.keys(), raw)?;
    let mut matches = Vec::new();
    for (question, capability_id) in &issued.bindings {
        let answer = answers
            .remove(question)
            .ok_or(DiscoveryMatcherError::MissingAnswer)?;
        matches.push(SelectionAnswer {
            question: question_id(issued, question),
            matched: CapabilityIntentMatch {
                capability_id: capability_id.clone(),
                choice: answer,
            },
        });
    }
    Ok(matches)
}

/// Every candidate has exactly one complete question. Never fold conflicting
/// judgments or combine questions from distinct intent revisions.
#[cfg(test)]
fn finish_selection(
    answers: Vec<SelectionAnswer>,
    retrieval: &RetrievalReceipt,
    issued: &[IssuedBatch],
) -> Result<CapabilityMatchReceipt, DiscoveryMatcherError> {
    finish_prepared_selection(answers, retrieval, issued, Vec::new())
}
fn finish_prepared_selection(
    answers: Vec<SelectionAnswer>,
    retrieval: &RetrievalReceipt,
    issued: &[IssuedBatch],
    budget_retained: Vec<ScopeBudgetRetention>,
) -> Result<CapabilityMatchReceipt, DiscoveryMatcherError> {
    let expected: BTreeSet<_> = issued
        .iter()
        .flat_map(|batch| batch.bindings.keys().map(|key| question_id(batch, key)))
        .collect();
    if expected.len() != issued.iter().map(|b| b.bindings.len()).sum::<usize>() {
        return Err(DiscoveryMatcherError::DuplicateIssuedQuestion);
    }
    let mut actual = BTreeSet::new();
    let mut selected = BTreeMap::<String, CapabilityIntentMatch>::new();
    let mut answers = answers;
    answers.sort_by(|a, b| a.question.cmp(&b.question));
    for fragment in answers {
        if !actual.insert(fragment.question) {
            return Err(DiscoveryMatcherError::DuplicateAnswer);
        }
        let answer = fragment.matched;
        if selected
            .insert(answer.capability_id.clone(), answer)
            .is_some()
        {
            return Err(DiscoveryMatcherError::MultipleJudgments);
        }
    }
    if actual != expected {
        return Err(DiscoveryMatcherError::AnswerSetMismatch);
    }
    for budget in &budget_retained {
        if !retrieval
            .candidates
            .iter()
            .any(|c| c.id == budget.capability_id && c.reference == budget.reference)
        {
            return Err(DiscoveryMatcherError::CandidateCoverageMismatch);
        }
        if selected
            .insert(
                budget.capability_id.clone(),
                CapabilityIntentMatch {
                    capability_id: budget.capability_id.clone(),
                    choice: MatchChoice::Uncertain,
                },
            )
            .is_some()
        {
            return Err(DiscoveryMatcherError::MultipleJudgments);
        }
    }
    let selected_ids: BTreeSet<_> = selected.keys().collect();
    let candidate_ids: BTreeSet<_> = retrieval.candidates.iter().map(|c| &c.id).collect();
    if selected_ids != candidate_ids {
        return Err(DiscoveryMatcherError::CandidateCoverageMismatch);
    }
    Ok(CapabilityMatchReceipt {
        matches: selected.into_values().collect(),
        budget_retained,
    })
}

#[cfg(test)]
mod tests {
    mod bounded_scopes;
    mod budget_retention;
    mod compact;
    mod intent_scope;
    mod prefix;
    mod reference_index;
    mod scheduling;
    mod scoped_sharing;
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn catalogue_failure_preserves_concrete_source() {
        use plasm_core::catalog_discovery::CatalogDiscoveryError;
        use std::error::Error;

        let error = DiscoveryMatcherError::from(CatalogDiscoveryError::CatalogRevisionMismatch);
        assert!(matches!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<Box<CatalogDiscoveryError>>())
                .map(Box::as_ref),
            Some(CatalogDiscoveryError::CatalogRevisionMismatch)
        ));
    }

    fn issue_batches(
        model: &str,
        intent: &IntentProvenance,
        retrieval: &RetrievalReceipt,
    ) -> Result<Vec<IssuedBatch>, DiscoveryMatcherError> {
        super::issue_batches(
            model,
            intent,
            retrieval,
            &CatalogEvidence::protocol_fixture(retrieval),
        )
    }
    async fn judge_candidates<F, Fut>(
        model: &str,
        intent: &IntentProvenance,
        retrieval: &RetrievalReceipt,
        decide: F,
    ) -> Result<CapabilityMatchReceipt, DiscoveryMatcherError>
    where
        F: FnMut(IssuedBatch) -> Fut,
        Fut: std::future::Future<Output = Result<String, DecisionProviderError>>,
    {
        super::judge_candidates(
            model,
            intent,
            retrieval,
            &CatalogEvidence::protocol_fixture(retrieval),
            decide,
        )
        .await
    }
    fn fixture() -> (IntentProvenance, RetrievalReceipt) {
        let packet: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/discovery/partial-routing.json"
        ))
        .unwrap();
        (
            serde_json::from_value(packet["routing"]["intent_provenance"].clone()).unwrap(),
            serde_json::from_value(packet["routing"]["retrieval"].clone()).unwrap(),
        )
    }
    #[test]
    fn luna_decisions_cutover_binds_model_and_provider() {
        let (intent, retrieval) = fixture();
        assert_eq!(RELEVANCE_MODEL, "openai/gpt-6-luna-decisions");
        for model in [
            RELEVANCE_MODEL.to_owned(),
            format!("{RELEVANCE_MODEL}-20261006"),
        ] {
            let batch = issue_batches(&model, &intent, &retrieval)
                .unwrap()
                .remove(0);
            let request: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
            assert_eq!(request["model"], model);
            let mut raw: serde_json::Value =
                serde_json::from_str(&response(&batch, &[MatchChoice::Uncertain])).unwrap();
            raw["model"] = json!(model);
            let answers = decode_batch(&batch, &raw.to_string()).unwrap();
            let receipt =
                finish_selection(answers, &retrieval, std::slice::from_ref(&batch)).unwrap();
            assert_eq!(
                receipt.selected(&retrieval).len(),
                retrieval.candidates.len()
            );
            raw["provider"] = json!("TypeSafe");
            assert!(decode_batch(&batch, &raw.to_string()).is_err());
        }
        for unsupported in [
            "typesafe/jev-1.13",
            "openai/gpt-6-luna",
            "openai/gpt-6-luna-decisions-other",
        ] {
            assert!(matches!(
                issue_batches(unsupported, &intent, &retrieval),
                Err(DiscoveryMatcherError::UnsupportedModel)
            ));
        }
    }
    fn response(batch: &IssuedBatch, choices: &[MatchChoice]) -> String {
        let answers: BTreeMap<_, _> = batch
            .bindings
            .keys()
            .zip(choices)
            .map(|(key, choice)| {
                (
                    key,
                    json!({"type":"choice","choice":choice,"confidence":1.0,
                "probabilities":{"relevant":if *choice == MatchChoice::Relevant {1.0}else{0.0},
                "uncertain":if *choice == MatchChoice::Uncertain {1.0}else{0.0},
                "unrelated":if *choice == MatchChoice::Unrelated {1.0}else{0.0}}}),
                )
            })
            .collect();
        json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","answers":answers}).to_string()
    }
    // Resolve references for semantic assertions; dedicated wire tests verify compactness.
    fn question_content(batch: &IssuedBatch) -> serde_json::Value {
        let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
        let questions: BTreeMap<_, _> = batch
            .bindings
            .iter()
            .map(|(key, id)| {
                let index = batch.evidence.iter().position(|q| &q.id == id).unwrap();
                (
                    key,
                    json!({"instructions":body["state"]["capabilities"][format!("c{index}")]}),
                )
            })
            .collect();
        let mut context = body["state"].clone();
        context["criteria"] = json!(CRITERIA.into_iter().collect::<BTreeMap<_, _>>());
        json!({"questions":questions,"context":context})
    }

    #[test]
    fn artifact_composition_evidence_survives_pages_and_ordering() {
        let contracts = [
            "Export a text file containing sample identifiers and wavelengths. Returns a path, not contents. Creates a file and increments a counter; requires a credential.",
            "Read text contents at a supplied file path. Requires a credential; does not modify files.",
            "Erase a supplied file; returns no contents.",
            "Read unrelated furniture records.",
        ];
        for size in [8, 64, 128] {
            let (intent, mut retrieval) = fixture();
            let original = retrieval.candidates[0].clone();
            retrieval.candidates = (0..size)
                .map(|i| {
                    let mut c = original.clone();
                    c.id = format!("artifact{i}");
                    c.reference.capability = format!("operation{i}");
                    c.document.operation.contract = contracts[i % contracts.len()].into();
                    c
                })
                .collect();
            let forward = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
            retrieval.candidates.reverse();
            let reverse = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
            assert_eq!(
                forward.iter().map(|b| b.body()).collect::<Vec<_>>(),
                reverse.iter().map(|b| b.body()).collect::<Vec<_>>()
            );
            assert_eq!(forward.len(), size);
            let mut counts = BTreeMap::new();
            for page in forward {
                let body: serde_json::Value = serde_json::from_str(page.body()).unwrap();
                let evidence = body["state"]["capabilities"].as_object().unwrap();
                assert_eq!(evidence.len(), 1);
                let contract = evidence.values().next().unwrap()["capability"]["operation"]
                    ["contract"]
                    .as_str()
                    .unwrap();
                *counts.entry(contract.to_owned()).or_insert(0) += 1;
            }
            for contract in contracts {
                assert_eq!(counts[contract], size / contracts.len());
            }
        }
    }

    #[test]
    fn conditional_contribution_preserves_contracts_and_unknown_readiness() {
        for size in [8, 64, 128] {
            let (intent, mut retrieval) = fixture();
            let original = retrieval.candidates[0].clone();
            let contracts = [
                "Enable the reader for a named store. Requires a supplied credential; also changes its access state.",
                "Read records from a store whose reader is enabled.",
                "Disable the reader for a named store.",
                "Read unrelated maintenance records.",
            ];
            retrieval.candidates = (0..size)
                .map(|i| {
                    let mut candidate = original.clone();
                    candidate.id = format!("candidate{i}");
                    candidate.reference.capability = format!("operation{i}");
                    candidate.document.operation.contract = contracts[i % contracts.len()].into();
                    candidate
                })
                .collect();
            let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
            assert_eq!(batches.len(), size);
            for batch in &batches {
                let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
                assert_eq!(body["state"]["discovery_need"], intent.current());
                assert_eq!(body["state"]["rule"], RUBRIC);
                assert!(body["state"].get("intent_interpretation").is_none());
                let evidence = body["state"]["capabilities"].as_object().unwrap();
                assert_eq!(evidence.len(), 1);
                for card in evidence.values() {
                    let capability = &card["capability"];
                    let name = capability["reference"]["capability"].as_str().unwrap();
                    let i: usize = name.strip_prefix("operation").unwrap().parse().unwrap();
                    assert_eq!(
                        capability["operation"]["contract"],
                        contracts[i % contracts.len()]
                    );
                }
            }
            // The host does not override a classifier verdict or infer readiness.
            for choice in [
                MatchChoice::Relevant,
                MatchChoice::Uncertain,
                MatchChoice::Unrelated,
            ] {
                let mut pages = SelectionPages::new(batches.clone());
                while let Some(batch) = pages.current() {
                    let raw = response(batch, &vec![choice.clone(); batch.bindings.len()]);
                    pages.accept(&raw).unwrap();
                }
                let receipt = pages.finish(&retrieval).unwrap();
                assert_eq!(
                    receipt.selected(&retrieval).len(),
                    if choice == MatchChoice::Unrelated {
                        0
                    } else {
                        size
                    }
                );
            }
        }
    }

    #[test]
    fn uncertainty_preserves_exposure_without_rewriting_judgments() {
        let (intent, mut retrieval) = fixture();
        let original = retrieval.candidates[0].clone();
        retrieval.candidates = ["inspect", "modify", "excluded"]
            .into_iter()
            .map(|name| {
                let mut candidate = original.clone();
                candidate.id = name.into();
                candidate.reference.capability = name.into();
                candidate
            })
            .collect();
        retrieval.candidates[1].document.operation.kind =
            plasm_core::schema::CapabilityKind::Action;
        let issued = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
        let mut answers = Vec::new();
        for batch in &issued {
            let choices: Vec<_> = batch
                .bindings
                .values()
                .map(|id| {
                    if id == "excluded" {
                        MatchChoice::Unrelated
                    } else {
                        MatchChoice::Uncertain
                    }
                })
                .collect();
            answers.extend(decode_batch(batch, &response(batch, &choices)).unwrap());
        }
        let receipt = finish_selection(answers, &retrieval, &issued).unwrap();
        assert_eq!(
            receipt.selected(&retrieval),
            retrieval.candidates[..2]
                .iter()
                .map(|candidate| candidate.reference.clone())
                .collect::<Vec<_>>()
        );
        assert!(receipt
            .matches
            .iter()
            .filter(|m| m.capability_id != "excluded")
            .all(|m| m.choice == MatchChoice::Uncertain));
    }
    proptest! {
        #[test]
        fn relevance_roundtrip_preserves_exact_selection(mask in prop::collection::vec(0u8..3,1..16)) {
            let (intent,mut retrieval)=fixture();let original=retrieval.candidates[0].clone();
            retrieval.candidates=mask.iter().enumerate().map(|(i,_)|{let mut c=original.clone();c.id=format!("candidate{i}");c.reference.capability=format!("read{i}");c}).collect();
            let wire:RetrievalReceipt=serde_json::from_str(&serde_json::to_string(&retrieval).unwrap()).unwrap();
            let batches=issue_batches(RELEVANCE_MODEL,&intent,&wire).unwrap();
            let mut answers=Vec::new();
            for batch in &batches {
                let choices:Vec<_>=batch.bindings.values().map(|id| match mask[id.trim_start_matches("candidate").parse::<usize>().unwrap()] {0=>MatchChoice::Unrelated,1=>MatchChoice::Uncertain,_=>MatchChoice::Relevant}).collect();
                answers.extend(decode_batch(batch,&response(batch,&choices)).unwrap());
            }
            answers.reverse();
            let receipt=finish_selection(answers,&wire,&batches).unwrap();
            let decoded:CapabilityMatchReceipt=serde_json::from_str(&serde_json::to_string(&receipt).unwrap()).unwrap();
            prop_assert_eq!(&receipt,&decoded);
            prop_assert_eq!(receipt.selected(&wire).len(),mask.iter().filter(|&&m|m!=0).count());
            for batch in batches {
                prop_assert!(batch.questions.len() <= MAX_PAGE_QUESTIONS);
                prop_assert_eq!(batch.evidence.len(), wire.candidates.len());
                prop_assert!(batch.body().len() <= TARGET_PAGE_BYTES);
            }
        }
    }
    proptest! {
        #[test]
        fn provider_paging_preserves_all_questions_and_selection(
            mask in prop::collection::vec(0u8..3, 2..129),
        ) {
            let (intent, mut retrieval) = fixture();
            let original = retrieval.candidates[0].clone();
            retrieval.candidates = mask.iter().enumerate().map(|(i, _)| {
                let mut c = original.clone(); c.id = format!("candidate{i}");
                c.reference.capability = format!("read{i}"); c
            }).collect();
            // Completion order does not alter the candidate judgment set.
            let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
            let mut pages = SelectionPages::new(batches);
            let mut seen = BTreeSet::new();
            let mut requests = 0;
            while let Some(batch) = pages.current() {
                requests += 1;
                prop_assert!(requests <= 2 * mask.len());
                let choices: Vec<_> = batch.bindings.values().map(|id| {
                    assert!(seen.insert(id.clone()));
                    match mask[id.trim_start_matches("candidate").parse::<usize>().unwrap()] {
                        0 => MatchChoice::Unrelated, 1 => MatchChoice::Uncertain, _ => MatchChoice::Relevant
                    }
                }).collect();
                let raw = response(batch, &choices);
                let before = pages.page_count();
                pages.accept(&raw).unwrap();
                prop_assert_eq!(pages.page_count(), before);
            }
            let result = pages.finish(&retrieval).unwrap();
            prop_assert_eq!(seen.len(), mask.len());
            let expected: Vec<_> = retrieval.candidates.iter().enumerate()
                .filter(|(i,_)| mask[*i] != 0).map(|(_,c)| c.reference.clone()).collect();
            let wire: CapabilityMatchReceipt = serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
            prop_assert_eq!(wire.selected(&retrieval), expected);
        }
    }

    #[test]
    fn incomplete_or_malformed_pages_cannot_commit_selection() {
        let (intent, retrieval) = fixture();
        let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
        let mut pages = SelectionPages::new(batches.clone());
        assert!(matches!(
            pages.accept("{}"),
            Err(DiscoveryMatcherError::Response(_))
        ));
        assert_eq!(pages.current().unwrap().cache_key, batches[0].cache_key);
        assert!(matches!(
            pages.finish(&retrieval),
            Err(DiscoveryMatcherError::UnprocessedPages)
        ));
    }

    #[test]
    fn matcher_rejects_invalid_inputs_with_semantic_variants() {
        let (intent, retrieval) = fixture();
        assert!(matches!(
            issue_batches("  ", &intent, &retrieval),
            Err(DiscoveryMatcherError::MissingModel)
        ));

        let mut pages = SelectionPages::new(Vec::new());
        assert!(matches!(
            pages.accept("{}"),
            Err(DiscoveryMatcherError::NoPendingPage)
        ));
    }

    #[tokio::test]
    async fn live_paging_protocol_covers_128_across_rejections_and_aborts_partial_work() {
        let (intent, mut retrieval) = fixture();
        let template = retrieval.candidates[0].clone();
        retrieval.candidates = (0..128)
            .map(|i| {
                let mut c = template.clone();
                c.id = format!("candidate{i}");
                c.reference.capability = format!("read{i}");
                c
            })
            .collect();
        let mut calls = 0;
        let mut accepted = BTreeSet::new();
        let receipt = judge_candidates(RELEVANCE_MODEL, &intent, &retrieval, |batch| {
            calls += 1;
            let result: Result<String, DecisionProviderError> = if batch.questions.len() > 4 {
                Err(Box::new(
                    plasm_runtime::provider_transport::ProviderTransportError::ContextLimit,
                ))
            } else {
                for candidate in batch.bindings.values() {
                    assert!(accepted.insert(candidate.clone()));
                }
                Ok(response(
                    &batch,
                    &vec![MatchChoice::Relevant; batch.bindings.len()],
                ))
            };
            std::future::ready(result)
        })
        .await
        .unwrap();
        assert_eq!(accepted.len(), 128);
        assert!(calls <= 128);
        assert_eq!(receipt.selected(&retrieval).len(), 128);
        // Exercise failure after admitting multiple pages even with larger packing.
        let (_, mut retrieval) = scheduling::many_cards(8);
        let mut calls = 0;
        assert!(
            judge_candidates(RELEVANCE_MODEL, &intent, &retrieval, |batch| {
                calls += 1;
                std::future::ready(if calls == 2 {
                    Err(std::io::Error::other("provider unavailable").into())
                } else {
                    Ok(response(
                        &batch,
                        &vec![MatchChoice::Relevant; batch.questions.len()],
                    ))
                })
            })
            .await
            .is_err()
        );
        assert!(calls <= ROUTE_CONCURRENCY + 1);
        retrieval.candidates.resize(129, template);
        assert!(issue_batches(RELEVANCE_MODEL, &intent, &retrieval).is_err());
    }

    proptest! {
        #[test]
        fn judgment_preserves_semantics_across_codec(
            meaning in "[a-zA-Z ]{1,150}",
            contract in "[a-zA-Z ]{1,150}",
        ) {
            let (intent, mut retrieval) = fixture();
            retrieval.candidates[0].document.collection.meaning = meaning.clone();
            retrieval.candidates[0].document.operation.contract = contract.clone();
            let before = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
            let wire: RetrievalReceipt = serde_json::from_slice(&serde_json::to_vec(&retrieval).unwrap()).unwrap();
            let after = issue_batches(RELEVANCE_MODEL, &intent, &wire).unwrap();
            prop_assert_eq!(&before[0].body, &after[0].body);
            prop_assert_eq!(&before[0].cache_key, &after[0].cache_key);
            let packet = question_content(&after[0]);
            prop_assert!(packet["questions"]["q0"]["instructions"].get("collection_meaning").is_none());
            prop_assert_eq!(&packet["questions"]["q0"]["instructions"]["capability"]["operation"]["contract"], &json!(contract));
            prop_assert!(packet["questions"]["q0"]["instructions"]["capability"].get("projection").is_none());
        }
    }

    proptest! {
        #[test]
        fn direct_judgment_uses_only_current_request(turns in prop::collection::vec("[a-zA-Z ]{1,80}", 1..32)) {
            let turns: Vec<_> = turns.into_iter().map(|text| format!("Need {text}")).collect();
            let provenance = IntentProvenance::from_turns(turns.clone()).unwrap();
            let (_, retrieval) = fixture();
            let batches = issue_batches(RELEVANCE_MODEL, &provenance, &retrieval).unwrap();
            let content = question_content(&batches[0]);
            prop_assert!(content["context"].get("intent_turns").is_none());
            let current = IntentProvenance::from_turns([provenance.current().to_owned()]).unwrap();
            let direct = issue_batches(RELEVANCE_MODEL, &current, &retrieval).unwrap();
            prop_assert_eq!(batches[0].body(), direct[0].body());
            prop_assert_eq!(provenance.turns().collect::<Vec<_>>(), turns.iter().map(String::as_str).collect::<Vec<_>>());
            prop_assert_eq!(&content["context"]["discovery_need"], &json!(provenance.current()));
            prop_assert_eq!(&content["context"]["outcome"], &json!(provenance.current()));
        }
    }

    #[test]
    fn current_request_packets_ignore_ancestry_but_preserve_conditions_and_full_evidence() {
        let (_, retrieval) = fixture();
        let current = "Inspect λ specimens; preserve originals. If validation fails, report missing inputs. Do not execute changes.";
        let short = IntentProvenance::from_turns([current.into()]).unwrap();
        let long = IntentProvenance::from_turns(
            (0..40)
                .map(|n| format!("Earlier search {n}: {}", "old request ".repeat(100)))
                .chain([current.into()]),
        )
        .unwrap();
        let build = |intent: &IntentProvenance| {
            let batches = issue_batches(RELEVANCE_MODEL, intent, &retrieval).unwrap();
            let prepared: Vec<_> = batches
                .iter()
                .map(|b| request_packing::PreparedLocal::new(b).unwrap())
                .collect();
            let predicted =
                request_packing::connected_size(&prepared.iter().collect::<Vec<_>>()).unwrap();
            let packet = request_packing::connected(batches).unwrap().unwrap();
            assert_eq!(predicted, packet.body().len());
            packet
        };
        let a = build(&short);
        let b = build(&long);
        assert_eq!(a.body(), b.body());
        let body: serde_json::Value = serde_json::from_str(b.body()).unwrap();
        assert_eq!(body["state"]["outcome"], current);
        assert!(body["state"].get("intent_history").is_none());
        assert!(body["state"].get("intent_turns").is_none());
        assert_eq!(long.turns().count(), 41);
        assert_ne!(
            serde_json::to_value(short).unwrap(),
            serde_json::to_value(long).unwrap()
        );
    }

    #[test]
    fn direct_questions_preserve_contract_limits_and_revised_intent() {
        let (_, mut retrieval) = fixture();
        let intent =
            IntentProvenance::from_turns(["First", "First", "Revised", "First"].map(str::to_owned))
                .unwrap();
        retrieval.candidates[0].document.operation.contract =
            "Affects every configured member; cannot select a subset. Requires a credential. Erases the selected log.".into();
        let mut other = retrieval.candidates[0].clone();
        other.id = "second".into();
        other.reference.capability = "second".into();
        retrieval.candidates.push(other);
        for batch in issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap() {
            let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
            assert_eq!(body["model"], RELEVANCE_MODEL);
            assert!(body.get("reasoning").is_none());
            assert!(body.get("max_tokens").is_none());
            assert!(body.get("temperature").is_none());
            assert!(body.get("messages").is_none());
            let content = question_content(&batch);
            assert!(content.get("state").is_none());
            for (key, question) in content["questions"].as_object().unwrap() {
                let target = retrieval
                    .candidates
                    .iter()
                    .find(|c| c.id == batch.bindings[key])
                    .unwrap();
                let evidence = &question["instructions"];
                assert_eq!(evidence["capability"]["reference"], json!(target.reference));
                assert_eq!(
                    evidence["capability"]["operation"],
                    json!(target.document.operation)
                );
                assert!(evidence.get("collection_meaning").is_none());
                assert_eq!(content["context"]["discovery_need"], "First");
                assert_eq!(content["context"]["outcome"], "First");
                assert!(content["context"].get("intent_turns").is_none());
                assert_eq!(content["context"]["rule"], RUBRIC);
                assert!(content["context"].get("intent_interpretation").is_none());
                for (label, meaning) in CRITERIA {
                    assert_eq!(content["context"]["criteria"][label], meaning);
                }
            }
        }
    }

    #[test]
    fn strict_choices_reject_missing_foreign_duplicate_and_extra_fields() {
        let (intent, retrieval) = fixture();
        let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
        let valid: serde_json::Value =
            serde_json::from_str(&response(&batches[0], &[MatchChoice::Relevant])).unwrap();
        let mut invalids = vec![];
        let mut missing = valid.clone();
        missing["answers"] = json!({});
        invalids.push(missing);
        let mut foreign = valid.clone();
        foreign["answers"]["foreign"] = valid["answers"]["q0"].clone();
        invalids.push(foreign);
        let mut extra = valid.clone();
        extra["answers"]["q0"]["unexpected"] = json!(true);
        invalids.push(extra);
        for invalid in invalids {
            assert!(decode_batch(&batches[0], &invalid.to_string()).is_err());
        }
        let answer = valid["answers"]["q0"].to_string();
        let duplicate = format!(
            r#"{{"model":"{RELEVANCE_MODEL}","provider":"OpenAI","answers":{{"q0":{answer},"q0":{answer}}}}}"#
        );
        assert!(decode_batch(&batches[0], &duplicate).is_err());
        assert!(decode_batch(&batches[0], "not json").is_err());
        assert!(finish_selection(vec![], &retrieval, &batches).is_err());
        let mut answers = decode_batch(
            &batches[0],
            &response(&batches[0], &[MatchChoice::Relevant]),
        )
        .unwrap();
        answers.push(answers[0].clone());
        assert!(finish_selection(answers, &retrieval, &batches).is_err());
    }

    #[test]
    fn native_envelope_and_choices_fail_closed() {
        let (intent, retrieval) = fixture();
        let batch = issue_batches(RELEVANCE_MODEL, &intent, &retrieval)
            .unwrap()
            .remove(0);
        let raw: serde_json::Value =
            serde_json::from_str(&response(&batch, &[MatchChoice::Relevant])).unwrap();
        for (pointer, value) in [
            ("/model", json!("different/model")),
            ("/provider", json!("different/provider")),
            ("/answers/q0/type", json!("score")),
            ("/answers/q0/choice", json!("unsupported")),
            ("/answers/q0/confidence", json!(2.0)),
            ("/answers/q0/probabilities/relevant", json!(-1.0)),
            ("/answers", json!({})),
        ] {
            let mut invalid = raw.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            assert!(
                decode_batch(&batch, &invalid.to_string()).is_err(),
                "{pointer}"
            );
        }
        let mut dated = raw;
        dated["model"] = json!(format!("{RELEVANCE_MODEL}-20261021"));
        let answer = decode_batch(&batch, &dated.to_string()).unwrap();
        let receipt = serde_json::to_value(&answer[0].matched).unwrap();
        assert!(receipt.get("confidence").is_none());
        assert!(receipt.get("probabilities").is_none());
    }

    #[tokio::test]
    async fn decisions_http_boundary_preserves_local_questions_and_uncertain_exposure() {
        use std::sync::{Arc, Mutex};
        let (intent, mut retrieval) = fixture();
        let intent = intent
            .derived("Cancel the original outcome; inspect existing definitions only.".into())
            .unwrap()
            .derived("Find readable definitions; do not register anything.".into())
            .unwrap();
        let mut other = retrieval.candidates[0].clone();
        other.id = "second".into();
        other.reference.capability = "second".into();
        retrieval.candidates.push(other);
        let expected_body =
            request_packing::pack(issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap())
                .unwrap()[0]
                .body
                .clone();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let router = axum::Router::new().route(
            "/api/alpha/decisions",
            axum::routing::post(move |headers: axum::http::HeaderMap, body: String| {
                let seen = captured.clone();
                async move {
                    assert_eq!(headers["authorization"], "Bearer test-only");
                    assert_eq!(headers["content-type"], "application/json");
                    let request: serde_json::Value = serde_json::from_str(&body).unwrap();
                    let content = &request;
                    seen.lock().unwrap().push(body);
                    if content["questions"].as_object().unwrap().len() > 4 {
                        (axum::http::StatusCode::BAD_REQUEST,
                            json!({"error":{"code":400,"message":"context rejected","metadata":{"error_type":"context_length_exceeded"}}}).to_string())
                    } else {
                        let answers: BTreeMap<_, _> = content["questions"].as_object().unwrap().keys()
                            .map(|key| (key, json!({"type":"choice","choice":"uncertain","confidence":1.0,
                                "probabilities":{"relevant":0.0,"uncertain":1.0,"unrelated":0.0}}))).collect();
                        let raw = json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","answers":answers}).to_string();
                        (axum::http::StatusCode::OK, raw)
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!(
            "http://{}/api/alpha/decisions",
            listener.local_addr().unwrap()
        );
        assert!(RELEVANCE_ENDPOINT.ends_with("/api/alpha/decisions"));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = reqwest::Client::new();
        let receipt = judge_candidates(RELEVANCE_MODEL, &intent, &retrieval, |issued| {
            let client = &client;
            let endpoint = &endpoint;
            async move {
                plasm_runtime::provider_transport::request_provider(
                    &plasm_runtime::provider_transport::ReqwestProviderTransport::new(
                        client.clone(),
                        endpoint,
                        "test-only",
                    ),
                    issued.body(),
                    plasm_runtime::provider_transport::ProviderRetryPolicy::default(),
                    |_| Ok(()),
                )
                .await
                .map_err(|error| Box::new(error) as DecisionProviderError)
            }
        })
        .await
        .unwrap();
        server.abort();
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(seen.lock().unwrap()[0], expected_body);
        for body in seen.lock().unwrap().iter() {
            let request: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(request["state"]["outcome"], intent.current());
            assert_eq!(request["state"]["discovery_need"], intent.current());
            assert!(request["state"].get("intent_turns").is_none());
            assert!(request["state"].get("current_intent").is_none());
        }
        assert_eq!(receipt.matches[0].choice, MatchChoice::Uncertain);
        assert_eq!(
            receipt.selected(&retrieval),
            retrieval
                .candidates
                .iter()
                .map(|c| c.reference.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn distinct_intent_answers_cannot_substitute_or_merge() {
        let (intent, retrieval) = fixture();
        let first = issue_batches(RELEVANCE_MODEL, &intent, &retrieval)
            .unwrap()
            .remove(0);
        let second = issue_batches(
            RELEVANCE_MODEL,
            &intent.derived("Inspect the owner as well".into()).unwrap(),
            &retrieval,
        )
        .unwrap()
        .remove(0);
        let left = decode_batch(&first, &response(&first, &[MatchChoice::Relevant]))
            .unwrap()
            .remove(0);
        let right = decode_batch(&second, &response(&second, &[MatchChoice::Unrelated]))
            .unwrap()
            .remove(0);
        let issued = vec![first, second];
        assert!(finish_selection(vec![left.clone(), left.clone()], &retrieval, &issued).is_err());
        assert!(finish_selection(vec![left, right], &retrieval, &issued).is_err());
    }

    #[test]
    fn revised_intent_changes_judgment_key_without_reactivating_earlier_matches() {
        let (intent, retrieval) = fixture();
        let first = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
        let revised = intent
            .derived("Change of plan: do not publish; inspect only".into())
            .unwrap();
        let next = issue_batches(RELEVANCE_MODEL, &revised, &retrieval).unwrap();
        assert_ne!(first[0].cache_key, next[0].cache_key);
        let answers =
            decode_batch(&next[0], &response(&next[0], &[MatchChoice::Unrelated])).unwrap();
        assert!(finish_selection(answers, &retrieval, &next)
            .unwrap()
            .selected(&retrieval)
            .is_empty());
        let body: serde_json::Value = serde_json::from_str(&next[0].body).unwrap();
        assert!(body["state"].get("affirmative_effect_slots").is_none());
        assert!(body["state"].get("coverage").is_none());
    }
    #[test]
    fn indivisible_large_card_is_not_silently_truncated() {
        let (intent, mut retrieval) = fixture();
        retrieval.candidates[0].document.operation.contract =
            "distinct capability meaning ".repeat(30_000);
        assert!(matches!(
            issue_batches(RELEVANCE_MODEL, &intent, &retrieval),
            Err(DiscoveryMatcherError::ScopeLimit { .. })
        ));
    }
}
