//! One semantic relevance judgment over authorized candidates and typed evidence.
use crate::decision_codec::DecisionCodec;
mod decisions;
use crate::discovery_store::RetrievalReceipt;
use crate::intent_provenance::IntentProvenance;
pub(crate) use decisions::ENDPOINT as RELEVANCE_ENDPOINT;
use futures_util::{stream::FuturesUnordered, StreamExt};
use plasm_core::{catalog_discovery::content_hash, prerequisites::CapabilityRef};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

type DecisionProviderError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug, Error)]
pub enum DiscoveryMatcherError {
    #[error("relevance model is required")]
    MissingModel,
    #[error("capability relevance requires a typesafe/jev-* decision model")]
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
    #[error("relevance page rejected with one question and indivisible shared evidence ({bytes} bytes); supply a decision model with sufficient context; no evidence was removed and no selection was committed")]
    IndivisibleItem { bytes: usize },
    #[error("relevance request serialization failed")]
    Encode(#[source] crate::decision_codec::DecisionCodecError),
    #[error("decision response is invalid")]
    Response(#[from] crate::decision_codec::DecisionResponseError),
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
}
pub const RELEVANCE_MODEL: &str = "typesafe/jev-1.13";
// Packing target for the subjects judged on a page, excluding shared evidence.
// Shared evidence is bounded by retrieval and never dropped during partitioning.
const TARGET_PAGE_BYTES: usize = 16_000;
const MAX_PAGE_QUESTIONS: usize = 32;
/// Bounds work admitted by one discovery turn, including split pages.
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
const QUESTION_FORMAT: &str = "The current_intent is the current discovery need. Read intent_turns in order for references and constraints; explicit revisions in later turns supersede earlier goals. Judge contribution to the current need, not to every historical goal.";
const RUBRIC: &str = "Judge whether the documented output or effect supplies part of the requested outcome or information needed for it. Judge contribution, not whether this operation can execute now or complete the workflow alone. Keep input, scope and side-effect constraints intact; relevance does not establish a valid or authorized workflow.";
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
    questions: Vec<Question>,
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
    collection: plasm_core::catalog_discovery::CollectionEvidence,
}

pub fn issue_batches(
    model: &str,
    intent: &IntentProvenance,
    retrieval: &RetrievalReceipt,
) -> Result<Vec<IssuedBatch>, DiscoveryMatcherError> {
    if model.trim().is_empty() {
        return Err(DiscoveryMatcherError::MissingModel);
    }
    if !model.starts_with("typesafe/jev-") {
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
            .map(|candidate| Question {
                id: candidate.id.clone(),
                reference: candidate.reference.clone(),
                entity: candidate.document.entity.clone(),
                operation: candidate.document.operation.clone(),
                collection: candidate.document.collection.clone(),
            })
            .collect(),
    );
    let mut batches = Vec::new();
    let mut pending = Vec::new();
    for question in evidence.iter() {
        pending.push(question.clone());
        let subject_bytes = serde_json::to_vec(&pending)
            .map_err(crate::decision_codec::DecisionCodecError::from)
            .map_err(DiscoveryMatcherError::Encode)?
            .len();
        if pending.len() > 1
            && (pending.len() > MAX_PAGE_QUESTIONS || subject_bytes > TARGET_PAGE_BYTES)
        {
            let last = pending.pop().ok_or(DiscoveryMatcherError::EmptyBatch)?;
            batches.push(issue_batch(model, intent, &pending, &evidence)?);
            pending = vec![last];
        }
    }
    if !pending.is_empty() {
        batches.push(issue_batch(model, intent, &pending, &evidence)?);
    }
    Ok(batches)
}

struct RelevanceCodec;
impl DecisionCodec for RelevanceCodec {
    type Request<'a> = decisions::Request<'a>;

    fn encode(
        request: &Self::Request<'_>,
    ) -> Result<String, crate::decision_codec::DecisionCodecError> {
        // Preserve the deliberate page-local map order at the wire boundary.
        // The generic codec's Value roundtrip sorts maps and would erase it.
        Ok(serde_json::to_string(request)?)
    }
}
#[derive(Serialize)]
struct SharedEvidence<'a> {
    intent_turns: Vec<&'a str>,
    current_intent: &'a str,
    intent_interpretation: &'static str,
    rule: &'static str,
    capabilities: OrderedCapabilities<'a>,
}

struct OrderedCapabilities<'a>(Vec<(String, DirectEvidence<'a>)>);

impl Serialize for OrderedCapabilities<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, evidence) in &self.0 {
            map.serialize_entry(key, evidence)?;
        }
        map.end()
    }
}
#[derive(Serialize)]
struct DirectEvidence<'a> {
    capability: DirectCard<'a>,
    collection_meaning: &'a str,
}
#[derive(Serialize)]
struct DirectCard<'a> {
    reference: &'a CapabilityRef,
    entity: &'a str,
    operation: &'a plasm_core::catalog_discovery::OperationEvidence,
}

fn issue_batch(
    model: &str,
    intent: &IntentProvenance,
    questions: &[Question],
    evidence: &std::sync::Arc<Vec<Question>>,
) -> Result<IssuedBatch, DiscoveryMatcherError> {
    if questions.is_empty() {
        return Err(DiscoveryMatcherError::EmptyBatch);
    }
    let mut wire_questions = BTreeMap::new();
    let mut bindings = BTreeMap::new();
    let subjects: BTreeSet<_> = questions.iter().map(|q| &q.id).collect();
    let mut capabilities = Vec::new();
    for (i, q) in evidence.iter().enumerate() {
        let card = DirectEvidence {
            capability: DirectCard {
                reference: &q.reference,
                entity: &q.entity,
                operation: &q.operation,
            },
            collection_meaning: &q.collection.meaning,
        };
        let mut canonical = serde_json::to_value(&card)
            .map_err(crate::decision_codec::DecisionCodecError::from)
            .map_err(DiscoveryMatcherError::Encode)?;
        canonical.sort_all_objects();
        let bytes = serde_json::to_vec(&canonical)
            .map_err(crate::decision_codec::DecisionCodecError::from)
            .map_err(DiscoveryMatcherError::Encode)?;
        capabilities.push((
            !subjects.contains(&q.id),
            content_hash(&bytes),
            format!("c{i}"),
            card,
        ));
    }
    capabilities.sort_by(|a, b| (&a.0, &a.1, &a.2).cmp(&(&b.0, &b.1, &b.2)));
    let capabilities = OrderedCapabilities(
        capabilities
            .into_iter()
            .map(|(_, _, key, card)| (key, card))
            .collect(),
    );
    for (i, q) in questions.iter().enumerate() {
        let key = format!("q{i}");
        let index = evidence
            .iter()
            .position(|card| card.id == q.id)
            .ok_or(DiscoveryMatcherError::MissingAnswer)?;
        bindings.insert(key.clone(), q.id.clone());
        wire_questions.insert(key, decisions::Question {
            kind: "choice",
            instructions: format!("Using the shared intent and rule, classify the contribution of `capabilities.c{index}`. Other entries in `capabilities` are available composition evidence, not an execution plan."),
            criteria: CRITERIA.into_iter().collect(),
        });
    }
    let state = SharedEvidence {
        intent_turns: intent.turns().collect(),
        current_intent: intent.current(),
        intent_interpretation: QUESTION_FORMAT,
        rule: RUBRIC,
        capabilities,
    };
    let body = RelevanceCodec::encode(&decisions::Request {
        model,
        state: &state,
        questions: wire_questions,
    })
    .map_err(DiscoveryMatcherError::Encode)?;
    Ok(IssuedBatch {
        cache_key: content_hash(format!("jev-shared-contribution-v5\\n{body}").as_bytes()),
        body,
        model: model.into(),
        bindings,
        questions: questions.to_vec(),
        evidence: evidence.clone(),
    })
}

/// Split complete semantic questions. No descriptions are truncated or sliced.
pub fn repartition(
    batch: &IssuedBatch,
    intent: &IntentProvenance,
) -> Result<[IssuedBatch; 2], DiscoveryMatcherError> {
    if batch.questions.len() <= 1 {
        return Err(DiscoveryMatcherError::IndivisibleItem {
            bytes: batch.body.len(),
        });
    }
    let (left, right) = batch.questions.split_at(batch.questions.len() / 2);
    Ok([
        issue_batch(&batch.model, intent, left, &batch.evidence)?,
        issue_batch(&batch.model, intent, right, &batch.evidence)?,
    ])
}

/// Owns selection progress: only successfully decoded leaves can complete a selection.
/// A rejected page is replaced atomically by two complete, smaller units of work.
/// With N candidates and P initial pages, binary splitting admits at most N
/// successful leaves and 2*N-P page attempts. An indivisible rejection fails.
pub struct SelectionPages {
    pending: std::collections::VecDeque<IssuedBatch>,
    accepted: Vec<IssuedBatch>,
    answers: Vec<SelectionAnswer>,
}

/// Execute the same exhaustive paging protocol for live and cached decisions.
/// Only a context rejection can subdivide work; all other errors abort selection.
pub async fn judge_candidates<F, Fut>(
    model: &str,
    intent: &IntentProvenance,
    retrieval: &RetrievalReceipt,
    mut decide: F,
) -> Result<CapabilityMatchReceipt, DiscoveryMatcherError>
where
    F: FnMut(IssuedBatch) -> Fut,
    Fut: std::future::Future<Output = Result<String, DecisionProviderError>>,
{
    let mut pending: std::collections::VecDeque<_> =
        issue_batches(model, intent, retrieval)?.into();
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
                    .downcast_ref::<crate::decision_transport::DecisionTransportError>()
                    .is_some_and(|error| {
                        matches!(
                            error,
                            crate::decision_transport::DecisionTransportError::ContextLimit
                        )
                    }) =>
            {
                let [left, right] = repartition(&issued, intent)?;
                pending.push_front(right);
                pending.push_front(left);
            }
            Err(error) => return Err(DiscoveryMatcherError::Provider(error)),
        }
    }
    finish_selection(answers, retrieval, &accepted)
}
impl SelectionPages {
    pub fn new(batches: Vec<IssuedBatch>) -> Self {
        Self {
            pending: batches.into(),
            accepted: Vec::new(),
            answers: Vec::new(),
        }
    }
    /// Total leaf pages, including completed ones. Repartitioning must account
    /// for both halves before a host admits more provider work.
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
    pub fn split_current(
        &mut self,
        intent: &IntentProvenance,
    ) -> Result<(), DiscoveryMatcherError> {
        let batch = self
            .pending
            .front()
            .ok_or(DiscoveryMatcherError::NoPendingPage)?;
        let [left, right] = repartition(batch, intent)?;
        self.pending.pop_front();
        self.pending.push_front(right);
        self.pending.push_front(left);
        Ok(())
    }
    pub fn finish(
        self,
        retrieval: &RetrievalReceipt,
    ) -> Result<CapabilityMatchReceipt, DiscoveryMatcherError> {
        if !self.pending.is_empty() {
            return Err(DiscoveryMatcherError::UnprocessedPages);
        }
        finish_selection(self.answers, retrieval, &self.accepted)
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
pub fn finish_selection(
    answers: Vec<SelectionAnswer>,
    retrieval: &RetrievalReceipt,
    issued: &[IssuedBatch],
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
    let selected_ids: BTreeSet<_> = selected.keys().collect();
    let candidate_ids: BTreeSet<_> = retrieval.candidates.iter().map(|c| &c.id).collect();
    if selected_ids != candidate_ids {
        return Err(DiscoveryMatcherError::CandidateCoverageMismatch);
    }
    Ok(CapabilityMatchReceipt {
        matches: selected.into_values().collect(),
    })
}

#[cfg(test)]
mod tests {
    mod prefix;
    mod scheduling;
    use super::*;
    use proptest::prelude::*;
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
        json!({"model":RELEVANCE_MODEL,"provider":"TypeSafe","answers":answers}).to_string()
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
                if batch.questions.len() > 1 {
                    prop_assert!(serde_json::to_vec(&batch.questions).unwrap().len() <= TARGET_PAGE_BYTES);
                }
            }
        }
    }
    proptest! {
        #[test]
        fn provider_paging_preserves_all_questions_and_selection(
            mask in prop::collection::vec(0u8..3, 2..129),
            provider_questions in 1usize..5,
        ) {
            let (intent, mut retrieval) = fixture();
            let original = retrieval.candidates[0].clone();
            retrieval.candidates = mask.iter().enumerate().map(|(i, _)| {
                let mut c = original.clone(); c.id = format!("candidate{i}");
                c.reference.capability = format!("read{i}"); c
            }).collect();
            // This simulated provider rejects pages below our packing target too.
            let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
            let mut pages = SelectionPages::new(batches);
            let mut seen = BTreeSet::new();
            let mut requests = 0;
            while let Some(batch) = pages.current() {
                requests += 1;
                prop_assert!(requests <= 2 * mask.len());
                if batch.questions.len() > provider_questions {
                    let before = pages.page_count();
                    pages.split_current(&intent).unwrap();
                    prop_assert_eq!(pages.page_count(), before + 1);
                    continue;
                }
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

        let batch = issue_batches(RELEVANCE_MODEL, &intent, &retrieval)
            .unwrap()
            .remove(0);
        assert!(matches!(
            repartition(&batch, &intent),
            Err(DiscoveryMatcherError::IndivisibleItem { .. })
        ));

        let mut pages = SelectionPages::new(Vec::new());
        assert!(matches!(
            pages.accept("{}"),
            Err(DiscoveryMatcherError::NoPendingPage)
        ));
        assert!(matches!(
            pages.split_current(&intent),
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
            let result: Result<String, DecisionProviderError> = if batch.questions.len() > 1 {
                Err(Box::new(
                    crate::decision_transport::DecisionTransportError::ContextLimit,
                ))
            } else {
                assert!(accepted.insert(batch.bindings.values().next().unwrap().clone()));
                Ok(response(&batch, &[MatchChoice::Relevant]))
            };
            std::future::ready(result)
        })
        .await
        .unwrap();
        assert_eq!(accepted.len(), 128);
        assert!(calls <= 255);
        assert_eq!(receipt.selected(&retrieval).len(), 128);
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
        retrieval.candidates.push(template);
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
            prop_assert_eq!(&packet["questions"]["q0"]["instructions"]["collection_meaning"], &json!(meaning));
            prop_assert_eq!(&packet["questions"]["q0"]["instructions"]["capability"]["operation"]["contract"], &json!(contract));
            prop_assert!(packet["questions"]["q0"]["instructions"]["capability"].get("projection").is_none());
        }
    }

    proptest! {
        #[test]
        fn direct_provenance_preserves_every_turn(turns in prop::collection::vec("[a-zA-Z ]{1,80}", 1..32)) {
            let turns: Vec<_> = turns.into_iter().map(|text| format!("Need {text}")).collect();
            let provenance = IntentProvenance::from_turns(turns.clone()).unwrap();
            let (_, retrieval) = fixture();
            let batches = issue_batches(RELEVANCE_MODEL, &provenance, &retrieval).unwrap();
            let content = question_content(&batches[0]);
            prop_assert_eq!(&content["context"]["intent_turns"], &json!(turns));
            prop_assert_eq!(&content["context"]["current_intent"], &json!(provenance.current()));
        }
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
                assert_eq!(
                    evidence["collection_meaning"],
                    json!(target.document.collection.meaning)
                );
                assert_eq!(content["context"]["current_intent"], "First");
                assert_eq!(
                    content["context"]["intent_turns"],
                    json!(["First", "First", "Revised", "First"])
                );
                assert_eq!(content["context"]["rule"], RUBRIC);
                assert_eq!(content["context"]["intent_interpretation"], QUESTION_FORMAT);
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
            r#"{{"model":"{RELEVANCE_MODEL}","provider":"TypeSafe","answers":{{"q0":{answer},"q0":{answer}}}}}"#
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
    async fn decisions_http_boundary_repartitions_and_preserves_uncertain_exposure() {
        use std::sync::{Arc, Mutex};
        let (intent, mut retrieval) = fixture();
        let batch = issue_batches(RELEVANCE_MODEL, &intent, &retrieval)
            .unwrap()
            .remove(0);
        let raw = response(&batch, &[MatchChoice::Uncertain]);
        let mut other = retrieval.candidates[0].clone();
        other.id = "second".into();
        other.reference.capability = "second".into();
        retrieval.candidates.push(other);
        let expected_body = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap()[0]
            .body
            .clone();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let router = axum::Router::new().route(
            "/api/alpha/decisions",
            axum::routing::post(move |headers: axum::http::HeaderMap, body: String| {
                let seen = captured.clone();
                let raw = raw.clone();
                async move {
                    assert_eq!(headers["authorization"], "Bearer test-only");
                    assert_eq!(headers["content-type"], "application/json");
                    let request: serde_json::Value = serde_json::from_str(&body).unwrap();
                    let content = &request;
                    seen.lock().unwrap().push(body);
                    if content["questions"].as_object().unwrap().len() > 1 {
                        (axum::http::StatusCode::BAD_REQUEST,
                            json!({"error":{"code":400,"message":"context rejected","metadata":{"error_type":"context_length_exceeded"}}}).to_string())
                    } else {
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
                crate::decision_transport::request_decision(
                    client,
                    endpoint,
                    "test-only",
                    issued.body(),
                    crate::decision_transport::DecisionRetryPolicy::default(),
                    |_| Ok(()),
                )
                .await
                .map_err(|error| Box::new(error) as DecisionProviderError)
            }
        })
        .await
        .unwrap();
        server.abort();
        assert_eq!(seen.lock().unwrap().len(), 3);
        assert_eq!(seen.lock().unwrap()[0], expected_body);
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
        let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
        assert!(batches[0]
            .body
            .contains(&retrieval.candidates[0].document.operation.contract));
        assert!(repartition(&batches[0], &intent)
            .unwrap_err()
            .to_string()
            .contains("indivisible"));
    }
}
