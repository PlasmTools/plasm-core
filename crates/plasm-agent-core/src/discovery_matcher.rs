//! One semantic relevance judgment over authorized candidates and typed evidence.
use crate::decision_codec::DecisionCodec;
use crate::discovery_store::RetrievalReceipt;
use crate::intent_provenance::IntentProvenance;
use anyhow::{ensure, Context, Result};
use plasm_core::{catalog_discovery::content_hash, prerequisites::CapabilityRef};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
pub const JEV_MODEL: &str = "typesafe/jev-1.13";
// A wire-size packing target for economical requests, NOT a provider context guarantee.
// Provider rejections drive further typed partitioning, including below this target.
const TARGET_PAGE_BYTES: usize = 16_000;
const ANSWERS: [&str; 3] = ["relevant", "unrelated", "uncertain"];
const MAX_ROUNDED_PROBABILITY_DRIFT: f64 = 0.011;
const CRITERIA: [(&str, &str); 3] = [
    (
        "relevant",
        "The documented operation or information can contribute to the current need.",
    ),
    (
        "unrelated",
        "The capability concerns different work or conflicts with the current need.",
    ),
    (
        "uncertain",
        "The supplied evidence does not establish relevance or unrelatedness.",
    ),
];
const QUESTION_FORMAT: &str = "Each question names a capability key. Assess its operation and collection meaning under state.rule. Resolve collection_ref in state.collections. Use state.criteria for choice meanings.";

const RUBRIC: &str = "Is this capability relevant to the current discovery need? Relevant means its documented operation or information can contribute to that need, including information used to choose the target collection. Judge relevance, not necessity, workflow completeness, or permission to execute. Respect documented collection meanings and explicit exclusions. Current intent governs; provenance supplies context and explicit revisions replace earlier goals. Choose relevant, unrelated, or uncertain from the supplied evidence.";
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
    pub probabilities: BTreeMap<String, f64>,
    pub confidence: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityMatchReceipt {
    pub matches: Vec<CapabilityIntentMatch>,
}
impl CapabilityMatchReceipt {
    pub fn selected(&self, retrieval: &RetrievalReceipt) -> Vec<CapabilityRef> {
        let ids: BTreeSet<_> = self
            .matches
            .iter()
            .filter(|m| m.choice == MatchChoice::Relevant)
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
}
impl IssuedBatch {
    pub fn body(&self) -> &str {
        &self.body
    }
    pub fn cache_key(&self) -> &str {
        &self.cache_key
    }
}
#[derive(Debug, Deserialize)]
struct DecisionsResponse {
    model: String,
    provider: String,
    #[serde(deserialize_with = "crate::decision_codec::unique_map")]
    answers: BTreeMap<String, DecisionAnswer>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionAnswer {
    #[serde(rename = "type")]
    kind: String,
    choice: String,
    #[serde(deserialize_with = "crate::decision_codec::unique_map")]
    probabilities: BTreeMap<String, f64>,
    confidence: f64,
}
/// An issued question owns its exact semantic document and local result identity.
/// Graph witnesses remain in the routing receipt, not the relevance wire contract.
#[derive(Debug, Clone)]
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
) -> Result<Vec<IssuedBatch>> {
    ensure!(!model.trim().is_empty(), "Jev model required");
    ensure!(
        retrieval.candidates.len() <= crate::discovery_store::CANDIDATE_LIMIT,
        "judgment candidate bound exceeded"
    );
    let mut candidates: Vec<_> = retrieval.candidates.iter().collect();
    candidates.sort_by(|a, b| a.reference.cmp(&b.reference));
    ensure!(
        candidates
            .iter()
            .map(|c| &c.id)
            .collect::<BTreeSet<_>>()
            .len()
            == candidates.len(),
        "duplicate discovery candidate"
    );
    ensure!(
        candidates
            .iter()
            .map(|c| &c.reference)
            .collect::<BTreeSet<_>>()
            .len()
            == candidates.len(),
        "duplicate discovery capability reference"
    );
    let mut batches = Vec::new();
    let mut pending = Vec::new();
    for candidate in candidates {
        pending.push(Question {
            id: candidate.id.clone(),
            reference: candidate.reference.clone(),
            entity: candidate.document.entity.clone(),
            operation: candidate.document.operation.clone(),
            collection: candidate.document.collection.clone(),
        });
        if pending.len() > 1 && issue_batch(model, intent, &pending)?.body.len() > TARGET_PAGE_BYTES
        {
            let last = pending.pop().context("empty relevance batch")?;
            batches.push(issue_batch(model, intent, &pending)?);
            pending = vec![last];
        }
    }
    if !pending.is_empty() {
        batches.push(issue_batch(model, intent, &pending)?);
    }
    Ok(batches)
}

struct JevCodec;
#[derive(Serialize)]
struct JevRequest<'a> {
    model: &'a str,
    state: JevState<'a>,
    questions: BTreeMap<String, JevQuestion>,
}
#[derive(Serialize)]
struct JevState<'a> {
    rule: &'static str,
    question_format: &'static str,
    criteria: BTreeMap<&'static str, &'static str>,
    intent_provenance: &'a IntentProvenance,
    capabilities: BTreeMap<String, JevCard<'a>>,
    collections: BTreeMap<String, &'a str>,
}
#[derive(Serialize)]
struct JevCard<'a> {
    reference: &'a CapabilityRef,
    entity: &'a str,
    operation: &'a plasm_core::catalog_discovery::OperationEvidence,
    collection_ref: String,
}
#[derive(Serialize)]
struct JevQuestion {
    #[serde(rename = "type")]
    kind: ChoiceQuestion,
    instructions: String,
    criteria: BTreeMap<&'static str, &'static str>,
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum ChoiceQuestion {
    Choice,
}
impl DecisionCodec for JevCodec {
    type Request<'a> = JevRequest<'a>;
    type Context = IssuedBatch;
    type Output = Vec<SelectionAnswer>;
    fn decode(context: &IssuedBatch, raw: &str) -> Result<Self::Output> {
        decode_jev(context, raw)
    }
}

fn issue_batch(
    model: &str,
    intent: &IntentProvenance,
    questions: &[Question],
) -> Result<IssuedBatch> {
    ensure!(!questions.is_empty(), "empty relevance page");
    let mut cards = BTreeMap::new();
    let mut collections = BTreeMap::new();
    let mut collection_aliases = BTreeMap::new();
    let mut wire_questions = BTreeMap::new();
    let mut bindings = BTreeMap::new();
    for (i, q) in questions.iter().enumerate() {
        let c = q;
        let alias = format!("c{i}");
        let collection = collection_aliases
            .entry(c.collection.meaning.as_str())
            .or_insert_with(|| {
                let alias = format!("s{}", collections.len());
                collections.insert(alias.clone(), c.collection.meaning.as_str());
                alias
            });
        cards.insert(
            alias.clone(),
            JevCard {
                reference: &c.reference,
                entity: &c.entity,
                operation: &c.operation,
                collection_ref: collection.clone(),
            },
        );
        let key = format!("q{i}");
        bindings.insert(key.clone(), c.id.clone());
        wire_questions.insert(key, JevQuestion {
            kind: ChoiceQuestion::Choice,
            instructions: format!("Is capability {alias} relevant to the current discovery need? Judge its documented operation and collection meaning under state.rule. Choose relevant, unrelated, or uncertain."),
            criteria: ANSWERS.into_iter().map(|label| (label,label)).collect(),
        });
    }
    let body = JevCodec::encode(&JevRequest {
        model,
        state: JevState {
            rule: RUBRIC,
            question_format: QUESTION_FORMAT,
            criteria: CRITERIA.into_iter().collect(),
            intent_provenance: intent,
            capabilities: cards,
            collections,
        },
        questions: wire_questions,
    })?;
    Ok(IssuedBatch {
        cache_key: content_hash(format!("jev-relevance-v5\n{body}").as_bytes()),
        body,
        model: model.into(),
        bindings,
        questions: questions.to_vec(),
    })
}

/// Split complete semantic questions. No descriptions are truncated or sliced.
pub fn repartition(batch: &IssuedBatch, intent: &IntentProvenance) -> Result<[IssuedBatch; 2]> {
    ensure!(batch.questions.len() > 1,
        "Jev rejected indivisible relevance item ({} bytes); reduce its catalog description or supply a model with sufficient context; no selection was committed", batch.body.len());
    let (left, right) = batch.questions.split_at(batch.questions.len() / 2);
    Ok([
        issue_batch(&batch.model, intent, left)?,
        issue_batch(&batch.model, intent, right)?,
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
) -> Result<CapabilityMatchReceipt>
where
    F: FnMut(IssuedBatch) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let mut pages = SelectionPages::new(issue_batches(model, intent, retrieval)?);
    while let Some(issued) = pages.current().cloned() {
        match decide(issued).await {
            Ok(raw) => pages.accept(&raw)?,
            Err(error) if error.is::<crate::decision_transport::DecisionContextLimit>() => {
                pages.split_current(intent)?;
            }
            Err(error) => return Err(error),
        }
    }
    pages.finish(retrieval)
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
    pub fn accept(&mut self, raw: &str) -> Result<()> {
        let issued = self.pending.front().context("no pending relevance page")?;
        let answers = decode_batch(issued, raw)?;
        self.answers.extend(answers);
        self.accepted
            .push(self.pending.pop_front().expect("validated pending page"));
        Ok(())
    }
    pub fn split_current(&mut self, intent: &IntentProvenance) -> Result<()> {
        let batch = self.pending.front().context("no rejected relevance page")?;
        let [left, right] = repartition(batch, intent)?;
        self.pending.pop_front();
        self.pending.push_front(right);
        self.pending.push_front(left);
        Ok(())
    }
    pub fn finish(self, retrieval: &RetrievalReceipt) -> Result<CapabilityMatchReceipt> {
        ensure!(
            self.pending.is_empty(),
            "relevance selection has unprocessed pages"
        );
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
pub fn decode_batch(issued: &IssuedBatch, raw: &str) -> Result<Vec<SelectionAnswer>> {
    JevCodec::decode(issued, raw)
}
fn decode_jev(issued: &IssuedBatch, raw: &str) -> Result<Vec<SelectionAnswer>> {
    let response: DecisionsResponse =
        serde_json::from_str(raw).context("malformed Jev Decisions envelope")?;
    ensure!(
        resolved_model_matches(&issued.model, &response.model),
        "unexpected Jev resolved model"
    );
    ensure!(response.provider == "TypeSafe", "unexpected Jev provider");
    ensure!(
        response.answers.len() == issued.bindings.len(),
        "Jev response has missing or extra answers"
    );
    let mut matches = Vec::new();
    for (question, capability_id) in &issued.bindings {
        let answer = response
            .answers
            .get(question)
            .context("Jev response omitted requested question")?;
        ensure!(answer.kind == "choice", "Jev answer is not a choice");
        ensure!(
            ANSWERS.contains(&answer.choice.as_str()),
            "Jev answer has unknown choice"
        );
        ensure!(
            answer.probabilities.len() == ANSWERS.len()
                && ANSWERS
                    .iter()
                    .all(|key| answer.probabilities.contains_key(*key)),
            "Jev answer probability keys differ from requested choices"
        );
        let total: f64 = answer.probabilities.values().sum();
        ensure!(
            answer
                .probabilities
                .values()
                .all(|p| p.is_finite() && (0.0..=1.0).contains(p))
                && total > 0.0
                && (total - 1.0).abs() <= MAX_ROUNDED_PROBABILITY_DRIFT,
            "invalid Jev probability vector"
        );
        ensure!(
            answer.confidence.is_finite() && (0.0..=1.0).contains(&answer.confidence),
            "invalid Jev confidence"
        );
        let choice = match answer.choice.as_str() {
            "relevant" => MatchChoice::Relevant,
            "unrelated" => MatchChoice::Unrelated,
            "uncertain" => MatchChoice::Uncertain,
            _ => unreachable!(),
        };
        let probabilities = answer
            .probabilities
            .iter()
            .map(|(key, probability)| (key.clone(), probability / total))
            .collect();
        matches.push(SelectionAnswer {
            question: question_id(issued, question),
            matched: CapabilityIntentMatch {
                capability_id: capability_id.clone(),
                choice,
                probabilities,
                confidence: answer.confidence,
            },
        });
    }
    Ok(matches)
}

fn resolved_model_matches(requested: &str, resolved: &str) -> bool {
    resolved == requested
        || resolved
            .strip_prefix(requested)
            .is_some_and(|suffix| suffix.starts_with('-'))
}

/// Every candidate has exactly one complete question. Never fold conflicting
/// judgments or combine questions from distinct intent revisions.
pub fn finish_selection(
    answers: Vec<SelectionAnswer>,
    retrieval: &RetrievalReceipt,
    issued: &[IssuedBatch],
) -> Result<CapabilityMatchReceipt> {
    let expected: BTreeSet<_> = issued
        .iter()
        .flat_map(|batch| batch.bindings.keys().map(|key| question_id(batch, key)))
        .collect();
    ensure!(
        expected.len() == issued.iter().map(|b| b.bindings.len()).sum::<usize>(),
        "duplicate issued relevance question"
    );
    let mut actual = BTreeSet::new();
    let mut selected = BTreeMap::<String, CapabilityIntentMatch>::new();
    let mut answers = answers;
    answers.sort_by(|a, b| a.question.cmp(&b.question));
    for fragment in answers {
        ensure!(
            actual.insert(fragment.question),
            "duplicate relevance answer"
        );
        let answer = fragment.matched;
        ensure!(
            selected
                .insert(answer.capability_id.clone(), answer)
                .is_none(),
            "candidate received multiple relevance judgments"
        );
    }
    ensure!(
        actual == expected,
        "relevance answers differ from issued questions"
    );
    ensure!(
        selected.keys().collect::<BTreeSet<_>>()
            == retrieval.candidates.iter().map(|c| &c.id).collect(),
        "relevance receipt does not cover candidate universe"
    );
    Ok(CapabilityMatchReceipt {
        matches: selected.into_values().collect(),
    })
}

#[cfg(test)]
mod tests {
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
        let answers:BTreeMap<_,_>=batch.bindings.keys().zip(choices).map(|(key,choice)|{
            let choice=match choice {MatchChoice::Relevant=>"relevant",MatchChoice::Unrelated=>"unrelated",MatchChoice::Uncertain=>"uncertain"};
            (key,json!({"type":"choice","choice":choice,"confidence":1.0,"probabilities":ANSWERS.iter().map(|s|(*s,if *s==choice {1.0}else{0.0})).collect::<BTreeMap<_,_>>()}))
        }).collect();
        serde_json::to_string(&json!({"model":JEV_MODEL,"provider":"TypeSafe","answers":answers}))
            .unwrap()
    }
    proptest! {
        #[test]
        fn relevance_roundtrip_preserves_exact_selection(mask in prop::collection::vec(0u8..3,1..16)) {
            let (intent,mut retrieval)=fixture();let original=retrieval.candidates[0].clone();
            retrieval.candidates=mask.iter().enumerate().map(|(i,_)|{let mut c=original.clone();c.id=format!("candidate{i}");c.reference.capability=format!("read{i}");c}).collect();
            let wire:RetrievalReceipt=serde_json::from_str(&serde_json::to_string(&retrieval).unwrap()).unwrap();
            let batches=issue_batches(JEV_MODEL,&intent,&wire).unwrap();
            let mut answers=Vec::new();
            for batch in &batches {
                let choices:Vec<_>=batch.bindings.values().map(|id| match mask[id.trim_start_matches("candidate").parse::<usize>().unwrap()] {0=>MatchChoice::Unrelated,1=>MatchChoice::Uncertain,_=>MatchChoice::Relevant}).collect();
                answers.extend(decode_batch(batch,&response(batch,&choices)).unwrap());
            }
            answers.reverse();
            let receipt=finish_selection(answers,&wire,&batches).unwrap();
            let decoded:CapabilityMatchReceipt=serde_json::from_str(&serde_json::to_string(&receipt).unwrap()).unwrap();
            prop_assert_eq!(&receipt,&decoded);
            prop_assert_eq!(receipt.selected(&wire).len(),mask.iter().filter(|&&m|m==2).count());
            for batch in batches {prop_assert!(batch.body.len()<=TARGET_PAGE_BYTES);}
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
            let batches = issue_batches(JEV_MODEL, &intent, &retrieval).unwrap();
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
                .filter(|(i,_)| mask[*i] == 2).map(|(_,c)| c.reference.clone()).collect();
            let wire: CapabilityMatchReceipt = serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
            prop_assert_eq!(wire.selected(&retrieval), expected);
        }
    }

    #[test]
    fn incomplete_or_malformed_pages_cannot_commit_selection() {
        let (intent, retrieval) = fixture();
        let batches = issue_batches(JEV_MODEL, &intent, &retrieval).unwrap();
        let mut pages = SelectionPages::new(batches.clone());
        assert!(pages.accept("{}").is_err());
        assert_eq!(pages.current().unwrap().cache_key, batches[0].cache_key);
        assert!(pages.finish(&retrieval).is_err());
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
        let receipt = judge_candidates(JEV_MODEL, &intent, &retrieval, |batch| {
            calls += 1;
            let result = if batch.questions.len() > 1 {
                Err(crate::decision_transport::DecisionContextLimit.into())
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
        assert!(judge_candidates(JEV_MODEL, &intent, &retrieval, |batch| {
            calls += 1;
            std::future::ready(if calls == 2 {
                Err(anyhow::anyhow!("provider unavailable"))
            } else {
                Ok(response(
                    &batch,
                    &vec![MatchChoice::Relevant; batch.questions.len()],
                ))
            })
        })
        .await
        .is_err());
        assert_eq!(calls, 2);
        retrieval.candidates.push(template);
        assert!(issue_batches(JEV_MODEL, &intent, &retrieval).is_err());
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
            let before = issue_batches(JEV_MODEL, &intent, &retrieval).unwrap();
            let wire: RetrievalReceipt = serde_json::from_slice(&serde_json::to_vec(&retrieval).unwrap()).unwrap();
            let after = issue_batches(JEV_MODEL, &intent, &wire).unwrap();
            prop_assert_eq!(&before[0].body, &after[0].body);
            prop_assert_eq!(&before[0].cache_key, &after[0].cache_key);
            let packet: serde_json::Value = serde_json::from_str(&after[0].body).unwrap();
            prop_assert_eq!(&packet["state"]["collections"]["s0"], &json!(meaning));
            prop_assert_eq!(&packet["state"]["capabilities"]["c0"]["operation"]["contract"], &json!(contract));
            prop_assert!(packet["state"]["capabilities"]["c0"].get("projection").is_none());
        }
    }

    #[test]
    fn collection_descriptions_are_interned_without_truncation() {
        let (intent, mut retrieval) = fixture();
        let mut other = retrieval.candidates[0].clone();
        other.id = "second".into();
        other.reference.capability = "second".into();
        retrieval.candidates.push(other);
        let batches = issue_batches(JEV_MODEL, &intent, &retrieval).unwrap();
        let body: serde_json::Value = serde_json::from_str(&batches[0].body).unwrap();
        assert_eq!(body["state"]["collections"].as_object().unwrap().len(), 1);
        assert_eq!(
            body["state"]["collections"]["s0"],
            retrieval.candidates[0].document.collection.meaning
        );
        for card in body["state"]["capabilities"].as_object().unwrap().values() {
            assert_eq!(card["collection_ref"], "s0");
        }
    }

    #[test]
    fn shared_rubric_preserves_each_question_target_and_evidence() {
        let (intent, mut retrieval) = fixture();
        let mut other = retrieval.candidates[0].clone();
        other.id = "second".into();
        other.reference.capability = "second".into();
        retrieval.candidates.push(other);
        let batches = issue_batches(JEV_MODEL, &intent, &retrieval).unwrap();
        let batch = &batches[0];
        let body: serde_json::Value = serde_json::from_str(&batch.body).unwrap();
        assert_eq!(body["state"]["question_format"], QUESTION_FORMAT);
        assert_eq!(body["state"]["rule"], RUBRIC);
        for (label, meaning) in CRITERIA {
            assert_eq!(body["state"]["criteria"][label], meaning);
            assert_eq!(batch.body.matches(meaning).count(), 1);
        }
        for (key, question) in body["questions"].as_object().unwrap() {
            let parts: Vec<_> = question["instructions"]
                .as_str()
                .unwrap()
                .split_whitespace()
                .collect();
            assert_eq!(question["instructions"], format!("Is capability {} relevant to the current discovery need? Judge its documented operation and collection meaning under state.rule. Choose relevant, unrelated, or uncertain.", parts[2]));
            assert!(body["state"].get("evidence").is_none());
            let reference: CapabilityRef = serde_json::from_value(
                body["state"]["capabilities"][parts[2]]["reference"].clone(),
            )
            .unwrap();
            let target = retrieval
                .candidates
                .iter()
                .find(|c| c.id == batch.bindings[key])
                .unwrap();
            assert_eq!(reference, target.reference);
            assert_eq!(
                question["criteria"],
                json!(ANSWERS
                    .into_iter()
                    .map(|label| (label, label))
                    .collect::<BTreeMap<_, _>>())
            );
        }
    }

    #[test]
    fn duplicate_wire_answer_and_probability_keys_are_rejected() {
        let (intent, retrieval) = fixture();
        let batch = issue_batches(JEV_MODEL, &intent, &retrieval)
            .unwrap()
            .remove(0);
        let raw = response(&batch, &[MatchChoice::Relevant]);
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let answer = &value["answers"]["q0"];
        let duplicate = format!(
            r#"{{"model":"{JEV_MODEL}","provider":"TypeSafe","answers":{{"q0":{answer},"q0":{answer}}}}}"#
        );
        assert!(decode_batch(&batch, &duplicate).is_err());
        let duplicate = raw.replace(
            r#""probabilities":{"#,
            r#""probabilities":{"relevant":0.0,"#,
        );
        assert!(decode_batch(&batch, &duplicate).is_err());
    }

    #[test]
    fn duplicate_missing_and_foreign_questions_are_rejected() {
        let (intent, retrieval) = fixture();
        let batches = issue_batches(JEV_MODEL, &intent, &retrieval).unwrap();
        let mut answers = decode_batch(
            &batches[0],
            &response(&batches[0], &[MatchChoice::Relevant]),
        )
        .unwrap();
        assert!(finish_selection(vec![], &retrieval, &batches).is_err());
        answers.push(answers[0].clone());
        assert!(finish_selection(answers, &retrieval, &batches).is_err());
        let mut raw: serde_json::Value =
            serde_json::from_str(&response(&batches[0], &[MatchChoice::Relevant])).unwrap();
        raw["answers"]["q0"]["probabilities"]["relevant"] = json!(-1);
        assert!(decode_batch(&batches[0], &raw.to_string()).is_err());
        raw["answers"]["q0"]["probabilities"]["relevant"] = json!(1);
        raw["answers"]["foreign"] = raw["answers"]["q0"].clone();
        raw["answers"].as_object_mut().unwrap().remove("q0");
        assert!(decode_batch(&batches[0], &raw.to_string()).is_err());
    }
    #[test]
    fn response_contract_rejects_invalid_vectors_models_and_confidence() {
        let (intent, retrieval) = fixture();
        let batch = issue_batches(JEV_MODEL, &intent, &retrieval)
            .unwrap()
            .remove(0);
        let raw: serde_json::Value =
            serde_json::from_str(&response(&batch, &[MatchChoice::Relevant])).unwrap();
        for (pointer, value) in [
            ("/model", json!("different/model")),
            ("/provider", json!("other")),
            ("/answers/q0/confidence", json!(2)),
            ("/answers/q0/type", json!("text")),
            ("/answers/q0/choice", json!("required_source")),
            ("/answers/q0/probabilities/relevant", json!(0.8)),
        ] {
            let mut invalid = raw.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            assert!(
                decode_batch(&batch, &invalid.to_string()).is_err(),
                "{pointer}"
            );
        }
        let mut rounded = raw;
        rounded["usage"] = json!({"input_tokens":1});
        rounded["answers"]["q0"]["probabilities"] =
            json!({"relevant":0.67,"unrelated":0.17,"uncertain":0.17});
        let decoded = decode_batch(&batch, &rounded.to_string()).unwrap();
        assert!((decoded[0].matched.probabilities.values().sum::<f64>() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn distinct_intent_answers_cannot_substitute_or_merge() {
        let (intent, retrieval) = fixture();
        let first = issue_batches(JEV_MODEL, &intent, &retrieval)
            .unwrap()
            .remove(0);
        let second = issue_batches(
            JEV_MODEL,
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
        let first = issue_batches(JEV_MODEL, &intent, &retrieval).unwrap();
        let revised = intent
            .derived("Change of plan: do not publish; inspect only".into())
            .unwrap();
        let next = issue_batches(JEV_MODEL, &revised, &retrieval).unwrap();
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
        let batches = issue_batches(JEV_MODEL, &intent, &retrieval).unwrap();
        assert!(batches[0]
            .body
            .contains(&retrieval.candidates[0].document.operation.contract));
        assert!(repartition(&batches[0], &intent)
            .unwrap_err()
            .to_string()
            .contains("indivisible"));
    }
}
