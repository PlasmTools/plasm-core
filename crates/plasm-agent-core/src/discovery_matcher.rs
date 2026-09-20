//! Native TypeSafe Jev capability-to-intent matching.
//!
//! This adapter deliberately does *not* select a workflow graph or pronounce a
//! request sufficient. It asks only whether one retrieved capability matches
//! one host-owned affirmative effect slot. Packet construction, batching, answer
//! validation, exposure, closure, and recovery remain host responsibilities.

use crate::intent_provenance::IntentProvenance;
use anyhow::{ensure, Context, Result};
use plasm_core::{
    catalog_discovery::{content_hash, CapabilityDocument},
    o200k_token_count,
    prerequisites::{CapabilityRef, InputSourceCandidate},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::discovery_store::{RetrievalReceipt, RetrievedCapability};

pub const JEV_MODEL: &str = "typesafe/jev-1.13";
const MAX_BATCH_TOKENS: usize = 24_000;
const ANSWERS: [&str; 3] = ["direct_match", "does_not_match", "uncertain"];
const SOURCE_ANSWERS: [&str; 3] = ["required_source", "not_required", "uncertain"];
/// TypeSafe Decisions rounds displayed probabilities to two decimal places.
const MAX_ROUNDED_PROBABILITY_DRIFT: f64 = 0.011;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectSlot {
    pub id: String,
    pub statement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatchChoice {
    DirectMatch,
    DoesNotMatch,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityIntentMatch {
    pub slot_id: String,
    pub capability_id: String,
    pub choice: MatchChoice,
    pub probabilities: BTreeMap<String, f64>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityMatchReceipt {
    pub slots: Vec<EffectSlot>,
    pub matches: Vec<CapabilityIntentMatch>,
    /// Host-derived completeness proof: every affirmative slot has a positive match.
    pub complete: bool,
    /// Slots with no positive match in the presented authorized packet.
    pub unmatched_slot_ids: Vec<String>,
    /// Host-derived union; exposed capabilities are not re-added.
    pub additional_capability_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputSourceChoice {
    RequiredSource,
    NotRequired,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InputSourceMatch {
    pub provider: CapabilityRef,
    pub choice: InputSourceChoice,
    pub probabilities: BTreeMap<String, f64>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InputSourceMatchReceipt {
    pub matches: Vec<InputSourceMatch>,
    pub selected: Vec<CapabilityRef>,
}

#[derive(Debug, Clone)]
pub struct IssuedBatch {
    pub body: String,
    pub cache_key: String,
    model: String,
    bindings: BTreeMap<String, (String, String)>,
}

#[derive(Debug, Clone)]
pub struct IssuedInputSourceBatch {
    pub body: String,
    pub cache_key: String,
    model: String,
    bindings: BTreeMap<String, CapabilityRef>,
}

#[derive(Debug, Deserialize)]
// OpenRouter may add transport/accounting metadata (for example `usage`) to a
// Decisions envelope. The semantic payload below remains strictly validated.
struct DecisionsResponse {
    model: String,
    provider: String,
    answers: BTreeMap<String, DecisionAnswer>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionAnswer {
    #[serde(rename = "type")]
    kind: String,
    choice: String,
    probabilities: BTreeMap<String, f64>,
    confidence: f64,
}

/// Build deterministic Decisions batches over the complete slot × candidate matrix.
/// Each question owns one complete card and one affirmative effect slot; aliases never
/// escape this module.
pub fn issue_batches(
    model: &str,
    intent_provenance: &IntentProvenance,
    slots: &[EffectSlot],
    retrieval: &RetrievalReceipt,
) -> Result<Vec<IssuedBatch>> {
    ensure!(!model.trim().is_empty(), "Jev model required");
    validate_slots(slots)?;
    let mut slots: Vec<_> = slots.iter().collect();
    slots.sort_by(|left, right| left.id.cmp(&right.id));
    let mut candidates: Vec<_> = retrieval.candidates.iter().collect();
    candidates.sort_by(|left, right| {
        left.reference
            .cmp(&right.reference)
            .then(left.id.cmp(&right.id))
    });
    let mut batches = Vec::new();
    let mut pending = Vec::new();
    for slot in slots {
        for candidate in &candidates {
            pending.push((slot, *candidate));
            let issued = issue_batch(model, intent_provenance, &pending)?;
            if o200k_token_count(&issued.body) > MAX_BATCH_TOKENS {
                let last = pending.pop().expect("just pushed pair");
                ensure!(
                    !pending.is_empty(),
                    "one Jev slot-match question exceeds packet token budget"
                );
                batches.push(issue_batch(model, intent_provenance, &pending)?);
                pending = vec![last];
            }
        }
    }
    if !pending.is_empty() {
        batches.push(issue_batch(model, intent_provenance, &pending)?);
    }
    Ok(batches)
}

pub fn validate_slots(slots: &[EffectSlot]) -> Result<()> {
    ensure!(!slots.is_empty(), "at least one effect slot required");
    let mut ids = BTreeSet::new();
    for slot in slots {
        ensure!(
            !slot.id.trim().is_empty() && !slot.statement.trim().is_empty(),
            "effect slot id and statement required"
        );
        ensure!(ids.insert(slot.id.as_str()), "duplicate effect slot id");
    }
    Ok(())
}

/// Ask Jev only to select or eliminate host-projected producer capabilities.
/// Every offered binding is already lawful under the CGS type system.
pub fn issue_input_source_batches(
    model: &str,
    intent: &IntentProvenance,
    candidates: &[InputSourceCandidate],
    documents: &BTreeMap<CapabilityRef, CapabilityDocument>,
) -> Result<Vec<IssuedInputSourceBatch>> {
    ensure!(!model.trim().is_empty(), "Jev model required");
    let mut candidates: Vec<_> = candidates.iter().collect();
    candidates.sort_by(|left, right| left.provider.cmp(&right.provider));
    let mut batches = Vec::new();
    let mut pending = Vec::new();
    for candidate in candidates {
        pending.push(candidate);
        let issued = issue_input_source_batch(model, intent, &pending, documents)?;
        if o200k_token_count(&issued.body) > MAX_BATCH_TOKENS {
            let last = pending.pop().expect("just pushed source candidate");
            ensure!(
                !pending.is_empty(),
                "one Jev input-source question exceeds packet token budget"
            );
            batches.push(issue_input_source_batch(
                model, intent, &pending, documents,
            )?);
            pending = vec![last];
        }
    }
    if !pending.is_empty() {
        batches.push(issue_input_source_batch(
            model, intent, &pending, documents,
        )?);
    }
    Ok(batches)
}

fn issue_input_source_batch(
    model: &str,
    intent: &IntentProvenance,
    candidates: &[&InputSourceCandidate],
    documents: &BTreeMap<CapabilityRef, CapabilityDocument>,
) -> Result<IssuedInputSourceBatch> {
    let mut questions = serde_json::Map::new();
    let mut bindings = BTreeMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let key = format!("q{index}");
        let provider = documents
            .get(&candidate.provider)
            .context("projected input source has no capability document")?;
        let mut consumer_refs = BTreeSet::new();
        for binding in &candidate.bindings {
            consumer_refs.insert(binding.consumer.clone());
        }
        let consumers: Vec<_> = consumer_refs
            .iter()
            .map(|reference| {
                documents
                    .get(reference)
                    .context("projected consumer has no capability document")
            })
            .collect::<Result<_>>()?;
        bindings.insert(key.clone(), candidate.provider.clone());
        questions.insert(
            key,
            json!({
                "type":"choice",
                "instructions": format!(
                    "Select or eliminate this host-projected input-source capability. The CGS type system has already established that each listed producer output can lawfully populate the listed required consumer input; do not reconsider type compatibility. Choose `required_source` only when the intent provenance requires values to be discovered or classified and this producer's documented domain meaning is suitable for at least one listed input. This includes resolving an explicit qualifier, category, membership, status, or identity before the final effect. Choose `not_required` when the user already supplies the values, the producer's domain meaning is unrelated, or the operation is not needed for this intent. Choose `uncertain` only when the intent provenance and cards do not establish either result. Judge the producer as an input source, not as the final requested effect.\n\nIntent provenance (root to current):\n{}\n\nConsumer capabilities:\n{}\n\nProjected typed bindings:\n{}\n\nProducer capability:\n{}",
                    intent.judgment_context()?,
                    serde_json::to_string(&consumers)?,
                    serde_json::to_string(&candidate.bindings)?,
                    serde_json::to_string(provider)?,
                ),
                "criteria": {
                    "required_source":"The intent provenance requires discovered or classified values that this producer can supply to at least one projected consumer input.",
                    "not_required":"The values are supplied without this producer, or its documented domain meaning is unrelated to the intent.",
                    "uncertain":"The intent and cards do not establish whether this producer is required."
                }
            }),
        );
    }
    let body = serde_json::to_string(&json!({
        "model": model,
        "state": {"intent_provenance": intent},
        "questions": questions,
    }))?;
    Ok(IssuedInputSourceBatch {
        cache_key: content_hash(format!("jev-input-source-match-v2\n{body}").as_bytes()),
        body,
        model: model.to_owned(),
        bindings,
    })
}

fn issue_batch(
    model: &str,
    intent_provenance: &IntentProvenance,
    pairs: &[(&EffectSlot, &RetrievedCapability)],
) -> Result<IssuedBatch> {
    let mut questions = serde_json::Map::new();
    let mut bindings = BTreeMap::new();
    let mut slots = BTreeMap::new();
    for (index, (slot, capability)) in pairs.iter().enumerate() {
        let key = format!("q{index}");
        bindings.insert(key.clone(), (slot.id.clone(), capability.id.clone()));
        slots.insert(slot.id.clone(), (*slot).clone());
        questions.insert(key, json!({
            "type":"choice",
            "instructions": format!(
                "Classify this capability against the explicit affirmative effect slot `{}`. The host has already determined that this slot is required work; do not decide that it is optional because another branch or step is also requested. The provenance nodes run from root to current; each derived intent retains the qualifiers, conditions, restrictions, and ordering inherited from its ancestors. Omission in a later node does not erase an inherited constraint. Use this ancestry to interpret the current slot. Choose `direct_match` only when the capability directly fulfils this slot's requested effect or requested information outcome. Judge this slot only and do not judge sufficiency for the whole intent. Choose `does_not_match` for different or conflicting work. Choose `uncertain` only when the slot, intent provenance, and card do not establish either relationship. Do not infer prerequisite or selector work here; the host projects typed input-source candidates for matched capabilities independently of unresolved slots.\n\nIntent provenance (root to current):\n{}\n\nAffirmative effect slot:\n{}\n\nCapability card:\n{}",
                slot.id,
                intent_provenance.judgment_context()?,
                serde_json::to_string(slot)?,
                serde_json::to_string(&card(index, capability))?,
            ),
            "criteria": {
                "direct_match":"The capability directly fulfils the named affirmative effect or requested information slot.",
                "does_not_match":"The documented capability is different work or conflicts with the named slot.",
                "uncertain":"The card and named slot do not establish direct work or non-correspondence."
            }
        }));
    }
    let body = serde_json::to_string(&json!({
        "model": model,
        "state": {
            "intent_provenance": intent_provenance,
            "affirmative_effect_slots": slots.into_values().collect::<Vec<_>>()
        },
        "questions": questions,
    }))?;
    Ok(IssuedBatch {
        cache_key: content_hash(format!("jev-effect-slot-match-v2\n{body}").as_bytes()),
        body,
        model: model.to_owned(),
        bindings,
    })
}

fn card(index: usize, capability: &RetrievedCapability) -> Value {
    let text = &capability.document.text;
    json!({
        "id": format!("c{index}"),
        "capability": capability.document.capability,
        "entity": capability.document.entity,
        "description": text,
        "related_entities": capability.document.related_entities,
    })
}

pub fn decode_batch(issued: &IssuedBatch, raw: &str) -> Result<Vec<CapabilityIntentMatch>> {
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
    for (question, (slot_id, capability_id)) in &issued.bindings {
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
            "direct_match" => MatchChoice::DirectMatch,
            "does_not_match" => MatchChoice::DoesNotMatch,
            "uncertain" => MatchChoice::Uncertain,
            _ => unreachable!(),
        };
        let probabilities = answer
            .probabilities
            .iter()
            .map(|(key, probability)| (key.clone(), probability / total))
            .collect();
        matches.push(CapabilityIntentMatch {
            slot_id: slot_id.clone(),
            capability_id: capability_id.clone(),
            choice,
            probabilities,
            confidence: answer.confidence,
        });
    }
    Ok(matches)
}

pub fn finish(
    slots: Vec<EffectSlot>,
    matches: Vec<CapabilityIntentMatch>,
    retrieval: &RetrievalReceipt,
) -> Result<CapabilityMatchReceipt> {
    validate_slots(&slots)?;
    let expected: BTreeSet<_> = slots
        .iter()
        .flat_map(|slot| {
            retrieval
                .candidates
                .iter()
                .map(move |capability| (slot.id.as_str(), capability.id.as_str()))
        })
        .collect();
    let actual: BTreeSet<_> = matches
        .iter()
        .map(|m| (m.slot_id.as_str(), m.capability_id.as_str()))
        .collect();
    ensure!(
        actual == expected && actual.len() == matches.len(),
        "Jev match receipt does not cover the complete slot-candidate matrix"
    );
    let positive: BTreeSet<_> = matches
        .iter()
        .filter(|m| matches!(m.choice, MatchChoice::DirectMatch))
        .map(|m| (m.slot_id.as_str(), m.capability_id.as_str()))
        .collect();
    let unmatched_slot_ids = slots
        .iter()
        .filter(|slot| !positive.iter().any(|(id, _)| *id == slot.id))
        .map(|slot| slot.id.clone())
        .collect::<Vec<_>>();
    let complete = unmatched_slot_ids.is_empty();
    let exposed: BTreeSet<_> = retrieval
        .candidates
        .iter()
        .filter(|c| c.admissions.contains("already_exposed"))
        .map(|c| c.id.as_str())
        .collect();
    let additional_capability_ids = positive
        .into_iter()
        .map(|(_, capability)| capability.to_owned())
        .filter(|id| !exposed.contains(id.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(CapabilityMatchReceipt {
        slots,
        matches,
        complete,
        unmatched_slot_ids,
        additional_capability_ids,
    })
}

pub fn decode_input_source_batch(
    issued: &IssuedInputSourceBatch,
    raw: &str,
) -> Result<Vec<InputSourceMatch>> {
    let response: DecisionsResponse =
        serde_json::from_str(raw).context("malformed Jev Decisions envelope")?;
    ensure!(
        resolved_model_matches(&issued.model, &response.model),
        "unexpected Jev resolved model"
    );
    ensure!(response.provider == "TypeSafe", "unexpected Jev provider");
    ensure!(
        response.answers.len() == issued.bindings.len(),
        "Jev response has missing or extra input-source answers"
    );
    let mut matches = Vec::new();
    for (question, provider) in &issued.bindings {
        let answer = response
            .answers
            .get(question)
            .context("Jev response omitted input-source question")?;
        ensure!(answer.kind == "choice", "Jev answer is not a choice");
        ensure!(
            SOURCE_ANSWERS.contains(&answer.choice.as_str()),
            "Jev answer has unknown input-source choice"
        );
        ensure!(
            answer.probabilities.len() == SOURCE_ANSWERS.len()
                && SOURCE_ANSWERS
                    .iter()
                    .all(|key| answer.probabilities.contains_key(*key)),
            "Jev input-source probability keys differ from requested choices"
        );
        let total: f64 = answer.probabilities.values().sum();
        ensure!(
            answer
                .probabilities
                .values()
                .all(|probability| probability.is_finite() && (0.0..=1.0).contains(probability))
                && total > 0.0
                && (total - 1.0).abs() <= MAX_ROUNDED_PROBABILITY_DRIFT,
            "invalid Jev input-source probability vector"
        );
        ensure!(
            answer.confidence.is_finite() && (0.0..=1.0).contains(&answer.confidence),
            "invalid Jev input-source confidence"
        );
        let choice = match answer.choice.as_str() {
            "required_source" => InputSourceChoice::RequiredSource,
            "not_required" => InputSourceChoice::NotRequired,
            "uncertain" => InputSourceChoice::Uncertain,
            _ => unreachable!(),
        };
        matches.push(InputSourceMatch {
            provider: provider.clone(),
            choice,
            probabilities: answer
                .probabilities
                .iter()
                .map(|(key, probability)| (key.clone(), probability / total))
                .collect(),
            confidence: answer.confidence,
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

pub fn finish_input_sources(
    matches: Vec<InputSourceMatch>,
    candidates: &[InputSourceCandidate],
) -> Result<InputSourceMatchReceipt> {
    let expected: BTreeSet<_> = candidates
        .iter()
        .map(|candidate| &candidate.provider)
        .collect();
    let actual: BTreeSet<_> = matches.iter().map(|matched| &matched.provider).collect();
    ensure!(
        actual == expected && actual.len() == matches.len(),
        "Jev input-source receipt does not cover exactly the projected candidates"
    );
    let selected = matches
        .iter()
        .filter(|matched| matches!(matched.choice, InputSourceChoice::RequiredSource))
        .map(|matched| matched.provider.clone())
        .collect();
    Ok(InputSourceMatchReceipt { matches, selected })
}

#[cfg(test)]
mod tests {
    fn provenance(intent: &str) -> super::IntentProvenance {
        super::IntentProvenance::from_turns([intent.to_owned()]).unwrap()
    }
    use super::*;
    use crate::discovery_store::RetrievedCapability;
    use plasm_core::catalog_discovery::CapabilityDocument;
    use plasm_core::prerequisites::CapabilityRef;

    fn packet() -> (Vec<EffectSlot>, RetrievalReceipt) {
        let slots = vec![EffectSlot {
            id: "r0".into(),
            statement: "Read the current account balance.".into(),
        }];
        let capability = RetrievedCapability {
            id: "rev/balance.read".into(),
            reference: CapabilityRef {
                catalog: "fixture".into(),
                capability: "balance.read".into(),
            },
            document: CapabilityDocument {
                capability: "balance.read".into(),
                entity: "Balance".into(),
                text: "Reads the current account balance.".into(),
                text_hash: "fixture".into(),
                related_entities: vec![],
            },
            admissions: BTreeSet::new(),
        };
        (
            slots,
            RetrievalReceipt {
                generation: "fixture".into(),
                candidates: vec![capability],
                lexical_count: 1,
                vector_count: 0,
                lexical_truncated: false,
                vector_truncated: false,
                fusion_truncated: 0,
                relation_truncated: 0,
            },
        )
    }

    #[test]
    fn selector_receives_lossless_ancestry_separately_from_current_retrieval_queries() {
        let chain = IntentProvenance::from_turns([
            format!("Preserve this constraint {}", "x".repeat(4096)),
            "Resolve the selected relation".into(),
            "Read the required records".into(),
        ])
        .unwrap();
        let (slots, retrieval) = packet();
        let batches = issue_batches(JEV_MODEL, &chain, &slots, &retrieval).unwrap();
        let body: serde_json::Value = serde_json::from_str(&batches[0].body).unwrap();
        assert_eq!(
            body["state"]["intent_provenance"],
            serde_json::to_value(&chain).unwrap()
        );
        assert_eq!(
            chain.retrieval_queries(&["Read records".into()]).unwrap(),
            ["Read records", "Read the required records"]
        );
    }

    #[test]
    fn native_choice_packet_binds_and_derives_only_positive_matches() {
        let (slots, retrieval) = packet();
        let batches = issue_batches(
            JEV_MODEL,
            &provenance("Read the balance."),
            &slots,
            &retrieval,
        )
        .unwrap();
        assert_eq!(batches.len(), 1);
        assert!(batches[0].body.contains("api/alpha/decisions") == false);
        let raw = json!({"model":"typesafe/jev-1.13-20260917","provider":"TypeSafe","answers":{"q0":{"type":"choice","choice":"direct_match","probabilities":{"direct_match":0.98,"does_not_match":0.01,"uncertain":0.01},"confidence":0.98}}}).to_string();
        let matches = decode_batch(&batches[0], &raw).unwrap();
        let receipt = finish(slots, matches, &retrieval).unwrap();
        assert_eq!(receipt.additional_capability_ids, vec!["rev/balance.read"]);
        assert!(receipt.complete);
        assert!(receipt.unmatched_slot_ids.is_empty());
    }

    #[test]
    fn every_affirmative_slot_requires_a_direct_match() {
        let (mut slots, retrieval) = packet();
        slots.push(EffectSlot {
            id: "r1".into(),
            statement: "Publish the account statement.".into(),
        });
        let batch = issue_batches(
            JEV_MODEL,
            &provenance("Read and publish the balance."),
            &slots,
            &retrieval,
        )
        .unwrap()
        .pop()
        .unwrap();
        let raw = json!({
            "model":"typesafe/jev-1.13-20260917",
            "provider":"TypeSafe",
            "answers":{
                "q0":{"type":"choice","choice":"direct_match","probabilities":{"direct_match":0.98,"does_not_match":0.01,"uncertain":0.01},"confidence":0.98},
                "q1":{"type":"choice","choice":"does_not_match","probabilities":{"direct_match":0.01,"does_not_match":0.98,"uncertain":0.01},"confidence":0.98}
            }
        }).to_string();
        let matches = decode_batch(&batch, &raw).unwrap();
        let receipt = finish(slots, matches, &retrieval).unwrap();
        assert!(!receipt.complete);
        assert_eq!(receipt.unmatched_slot_ids, vec!["r1"]);
        assert_eq!(receipt.additional_capability_ids, vec!["rev/balance.read"]);
    }

    #[test]
    fn native_choice_packet_accepts_additive_transport_metadata() {
        let (slots, retrieval) = packet();
        let batch = issue_batches(
            JEV_MODEL,
            &provenance("Read the balance."),
            &slots,
            &retrieval,
        )
        .unwrap()
        .pop()
        .unwrap();
        let raw = json!({
            "model":"typesafe/jev-1.13-20260917",
            "provider":"TypeSafe",
            "answers":{"q0":{"type":"choice","choice":"direct_match","probabilities":{"direct_match":0.98,"does_not_match":0.01,"uncertain":0.01},"confidence":0.98}},
            "usage":{"input_tokens":12,"output_tokens":3,"cost":0.001}
        })
        .to_string();
        assert_eq!(decode_batch(&batch, &raw).unwrap().len(), 1);
    }

    #[test]
    fn response_model_must_resolve_the_requested_configured_model() {
        let (slots, retrieval) = packet();
        let batch = issue_batches(
            "typesafe/jev-2.0",
            &provenance("Read the balance."),
            &slots,
            &retrieval,
        )
        .unwrap()
        .pop()
        .unwrap();
        let answer = json!({
            "q0": {
                "type": "choice",
                "choice": "direct_match",
                "probabilities": {
                    "direct_match": 0.98,
                    "does_not_match": 0.01,
                    "uncertain": 0.01
                },
                "confidence": 0.98
            }
        });
        let expected = json!({
            "model": "typesafe/jev-2.0-20261001",
            "provider": "TypeSafe",
            "answers": answer
        })
        .to_string();
        assert_eq!(decode_batch(&batch, &expected).unwrap().len(), 1);

        let wrong = json!({
            "model": "typesafe/jev-1.13-20260917",
            "provider": "TypeSafe",
            "answers": answer
        })
        .to_string();
        assert!(decode_batch(&batch, &wrong).is_err());
    }

    #[test]
    fn direct_match_packet_defers_input_source_decisions() {
        let (slots, retrieval) = packet();
        let batch = issue_batches(
            JEV_MODEL,
            &provenance("Read the balance."),
            &slots,
            &retrieval,
        )
        .unwrap()
        .pop()
        .unwrap();
        assert_eq!(ANSWERS, ["direct_match", "does_not_match", "uncertain"]);
        assert!(batch
            .body
            .contains("projects typed input-source candidates"));
    }

    #[test]
    fn native_choice_packet_normalizes_provider_probability_rounding() {
        let (slots, retrieval) = packet();
        let batch = issue_batches(
            JEV_MODEL,
            &provenance("Read the balance."),
            &slots,
            &retrieval,
        )
        .unwrap()
        .pop()
        .unwrap();
        let raw = json!({"model":"typesafe/jev-1.13-20260917","provider":"TypeSafe","answers":{"q0":{"type":"choice","choice":"does_not_match","probabilities":{"direct_match":0.03,"does_not_match":0.94,"uncertain":0.02},"confidence":0.93}}}).to_string();
        let matched = decode_batch(&batch, &raw).unwrap();
        let total: f64 = matched[0].probabilities.values().sum();
        assert!((total - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn native_choice_packet_rejects_partial_answers() {
        let (slots, retrieval) = packet();
        let batch = issue_batches(
            JEV_MODEL,
            &provenance("Read the balance."),
            &slots,
            &retrieval,
        )
        .unwrap()
        .pop()
        .unwrap();
        let raw = json!({"model":"typesafe/jev-1.13-20260917","provider":"TypeSafe","answers":{}})
            .to_string();
        assert!(decode_batch(&batch, &raw).is_err());
    }

    #[test]
    fn input_source_packet_asks_jev_to_select_a_projected_edge() {
        let consumer = CapabilityRef {
            catalog: "ledger".into(),
            capability: "expense_create".into(),
        };
        let provider = CapabilityRef {
            catalog: "directory".into(),
            capability: "contact_query".into(),
        };
        let candidates = vec![InputSourceCandidate {
            provider: provider.clone(),
            bindings: vec![plasm_core::prerequisites::InputSourceBinding {
                consumer: consumer.clone(),
                input: plasm_core::prerequisites::InputPath {
                    lane: plasm_core::prerequisites::InputLane::Payload,
                    path: vec!["participant_emails".into()],
                },
                provider: provider.clone(),
                output_field: "email".into(),
                collect: true,
            }],
        }];
        let document = |capability: &str, entity: &str, text: &str| CapabilityDocument {
            capability: capability.into(),
            entity: entity.into(),
            text: text.into(),
            text_hash: "fixture".into(),
            related_entities: vec![],
        };
        let documents = BTreeMap::from([
            (
                consumer,
                document(
                    "expense_create",
                    "Expense",
                    "Create an expense with participants.",
                ),
            ),
            (
                provider.clone(),
                document(
                    "contact_query",
                    "Contact",
                    "Find contacts and return their emails.",
                ),
            ),
        ]);
        let batch = issue_input_source_batches(
            JEV_MODEL,
            &provenance("Create an expense with my coworkers."),
            &candidates,
            &documents,
        )
        .unwrap()
        .pop()
        .unwrap();
        assert!(batch.body.contains("host-projected input-source"));
        assert!(batch.body.contains("participant_emails"));
        assert!(batch.body.contains("Intent provenance"));
        let raw = json!({"model":"typesafe/jev-1.13-20260917","provider":"TypeSafe","answers":{"q0":{"type":"choice","choice":"required_source","probabilities":{"required_source":0.97,"not_required":0.02,"uncertain":0.01},"confidence":0.97}}}).to_string();
        let matches = decode_input_source_batch(&batch, &raw).unwrap();
        let receipt = finish_input_sources(matches, &candidates).unwrap();
        assert_eq!(receipt.selected, vec![provider]);
    }
}
