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
/// Provider decision. For partitioned evidence, probabilities and confidence
/// belong to the decisive fragment; they are not an aggregate posterior.
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
    bindings: BTreeMap<String, SourceQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SourceQuestionId(String);

#[derive(Debug, Clone)]
struct SourceQuestion {
    id: SourceQuestionId,
    provider: CapabilityRef,
}

#[derive(Debug, Clone)]
pub struct InputSourceAnswer {
    question_id: SourceQuestionId,
    matched: InputSourceMatch,
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
                ensure!(
                    o200k_token_count(&issue_batch(model, intent_provenance, &pending)?.body)
                        <= MAX_BATCH_TOKENS,
                    "one Jev slot-match question exceeds packet token budget"
                );
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
    let mut candidates = candidates.to_vec();
    candidates.sort_by(|left, right| left.provider.cmp(&right.provider));
    let mut fragments = Vec::new();
    for candidate in candidates {
        partition_source_question(model, intent, candidate, documents, &mut fragments)?;
    }
    let mut batches = Vec::new();
    let mut pending = Vec::new();
    for candidate in &fragments {
        pending.push(candidate);
        if o200k_token_count(&issue_input_source_batch(model, intent, &pending, documents)?.body)
            > MAX_BATCH_TOKENS
        {
            let last = pending.pop().expect("just pushed source candidate");
            ensure!(
                !pending.is_empty(),
                "validated source fragment exceeds packet budget"
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
    ensure!(
        batches
            .iter()
            .all(|b| o200k_token_count(&b.body) <= MAX_BATCH_TOKENS),
        "input-source packet exceeds budget"
    );
    Ok(batches)
}

fn partition_source_question(
    model: &str,
    intent: &IntentProvenance,
    mut candidate: InputSourceCandidate,
    documents: &BTreeMap<CapabilityRef, CapabilityDocument>,
    out: &mut Vec<InputSourceCandidate>,
) -> Result<()> {
    let count = candidate.bindings.len() + candidate.membership.len();
    ensure!(count > 0, "input-source candidate has no typed evidence");
    let issued = issue_input_source_batch(model, intent, &[&candidate], documents)?;
    if o200k_token_count(&issued.body) <= MAX_BATCH_TOKENS {
        out.push(candidate);
        return Ok(());
    }
    ensure!(count > 1, "input-source {}/{} has one indivisible witness whose intent and capability cards exceed the {} token packet limit; reduce catalog card or intent size before retrying", candidate.provider.catalog, candidate.provider.capability, MAX_BATCH_TOKENS);
    let midpoint = count / 2;
    let mut right = InputSourceCandidate {
        provider: candidate.provider.clone(),
        projection: candidate.projection.clone(),
        bindings: Vec::new(),
        membership: Vec::new(),
    };
    if midpoint < candidate.bindings.len() {
        right.bindings = candidate.bindings.split_off(midpoint);
        right.membership = std::mem::take(&mut candidate.membership);
    } else {
        right.membership = candidate
            .membership
            .split_off(midpoint - candidate.bindings.len());
    }
    partition_source_question(model, intent, candidate, documents, out)?;
    partition_source_question(model, intent, right, documents, out)
}

fn issue_input_source_batch(
    model: &str,
    intent: &IntentProvenance,
    candidates: &[&InputSourceCandidate],
    documents: &BTreeMap<CapabilityRef, CapabilityDocument>,
) -> Result<IssuedInputSourceBatch> {
    let references: BTreeSet<_> = candidates
        .iter()
        .flat_map(|candidate| {
            std::iter::once(&candidate.provider)
                .chain(candidate.bindings.iter().map(|binding| &binding.consumer))
                .chain(candidate.membership.iter().map(|witness| &witness.source))
        })
        .collect();
    let ids: BTreeMap<_, _> = references
        .iter()
        .enumerate()
        .map(|(i, reference)| (*reference, format!("c{i}")))
        .collect();
    let cards: BTreeMap<_, _> = references.into_iter().map(|reference| {
        let doc = documents.get(reference).context("projected source has no capability document")?;
        // Hashes and related-entity retrieval metadata are not judgment evidence.
        Ok((ids[reference].clone(), json!({"catalog": reference.catalog, "capability": reference.capability, "entity": doc.entity, "operation": doc.operation, "collection": doc.collection})))
    }).collect::<Result<_>>()?;
    let mut questions = serde_json::Map::new();
    let mut bindings = BTreeMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let key = format!("q{index}");
        let evidence = json!({
            "provider": ids[&candidate.provider],
            "entity_projection": candidate.projection,
            "input_bindings": candidate.bindings.iter().map(|binding| json!({
                "consumer": ids[&binding.consumer], "input": binding.input,
                "output_field": binding.output_field, "collect": binding.collect,
            })).collect::<Vec<_>>(),
            "membership": candidate.membership.iter().map(|witness| json!({
                "source": ids[&witness.source], "source_identity": witness.source_identity,
                "provider_identity": witness.provider_identity,
            })).collect::<Vec<_>>(),
        });
        bindings.insert(
            key.clone(),
            SourceQuestion {
                id: SourceQuestionId(content_hash(serde_json::to_string(candidate)?.as_bytes())),
                provider: candidate.provider.clone(),
            },
        );
        questions.insert(key, json!({
            "type": "choice",
            "instructions": format!("Apply state.input_source_rule to this host-projected input-source evidence fragment. Resolve cN references through state.capabilities. Judge only the listed witnesses; other fragments may establish other reasons to select this provider. Evidence: {}", evidence),
            "criteria": {
                "required_source": "At least one listed witness supplies required discovered values or establishes a required identity membership test.",
                "not_required": "None of the listed witnesses requires this provider for the intent.",
                "uncertain": "No witness is established as required, but at least one remains uncertain."
            }
        }));
    }
    let body = serde_json::to_string(&json!({
        "model": model,
        "state": {
            "intent_provenance": intent,
            "input_source_rule": "Intent provenance is ordered root to current. The current need governs; ancestors provide context, not immutable constraints. Explicit revisions may replace earlier goals. Select a provider when its documented domain meaning is suitable for a listed typed witness required by the intent. Input bindings supply required arguments or typed entity receivers. The entity projection distinguishes direct fields, fields obtainable by identity-preserving hydration, and unavailable fields; it does not prove successful retrieval. A typed binding proves structural compatibility, not semantic suitability. For membership, identify the precise collection and fact required by the current need. Shared identity fields do not make collections equivalent: absence in a subset does not establish absence in its superset. Exact identity and complete successful retrieval are required for absence; fuzzy search rank is not identity. Neither witness proves the task condition is already satisfied. Resolve explicit qualifiers, categories, membership, status and identity before effects. Values already supplied by the user need no discovery. Judge the provider as an input source, not the final effect.",
            "capabilities": cards,
        },
        "questions": questions,
    }))?;
    Ok(IssuedInputSourceBatch {
        cache_key: content_hash(format!("jev-input-source-match-v6\n{body}").as_bytes()),
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
                "Classify this capability against the explicit affirmative effect slot `{}`. The host has already determined that this slot is required work; do not decide that it is optional because another branch or step is also requested. The current discovery need governs. Provenance supplies context, not immutable constraints; explicit revisions may replace earlier goals. Judge operation applicability from its receiver, inputs, documented restrictions and effect. The collection used to select receivers is a separate question and must not become an operation restriction. For a requested read, use collection evidence to distinguish the information it actually establishes. Choose `direct_match` only when the capability directly fulfils this slot's requested effect or requested information outcome. Judge this slot only and do not judge sufficiency for the whole intent. Choose `does_not_match` for different or conflicting work. Choose `uncertain` only when the slot, intent provenance, and card do not establish either relationship. Do not infer prerequisite or selector work here; the host projects typed input-source candidates for matched capabilities independently of unresolved slots.\n\nIntent provenance (root to current):\n{}\n\nAffirmative effect slot:\n{}\n\nCapability card:\n{}",
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
        cache_key: content_hash(format!("jev-effect-slot-match-v3\n{body}").as_bytes()),
        body,
        model: model.to_owned(),
        bindings,
    })
}

fn card(index: usize, capability: &RetrievedCapability) -> Value {
    let document = &capability.document;
    let mut card = json!({
        "id": format!("c{index}"),
        "capability": document.capability,
        "entity": document.entity,
        "operation": document.operation,
    });
    // Read outcomes depend on collection meaning. Effects do not inherit the
    // selection semantics of whichever collection supplied their receiver.
    if matches!(
        document.operation.kind,
        plasm_core::schema::CapabilityKind::Query
            | plasm_core::schema::CapabilityKind::Search
            | plasm_core::schema::CapabilityKind::Get
    ) {
        card["collection"] = serde_json::to_value(&document.collection)
            .expect("collection evidence is serializable");
    }
    card
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
) -> Result<Vec<InputSourceAnswer>> {
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
    for (question, source_question) in &issued.bindings {
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
        matches.push(InputSourceAnswer {
            question_id: source_question.id.clone(),
            matched: InputSourceMatch {
                provider: source_question.provider.clone(),
                choice,
                probabilities: answer
                    .probabilities
                    .iter()
                    .map(|(key, probability)| (key.clone(), probability / total))
                    .collect(),
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

pub fn finish_input_sources(
    mut answers: Vec<InputSourceAnswer>,
    candidates: &[InputSourceCandidate],
    batches: &[IssuedInputSourceBatch],
) -> Result<InputSourceMatchReceipt> {
    let expected: BTreeMap<_, _> = batches
        .iter()
        .flat_map(|batch| batch.bindings.values())
        .map(|question| (&question.id, &question.provider))
        .collect();
    let actual: BTreeMap<_, _> = answers
        .iter()
        .map(|answer| (&answer.question_id, &answer.matched.provider))
        .collect();
    ensure!(
        actual == expected && actual.len() == answers.len(),
        "Jev input-source receipt has missing, duplicate or foreign evidence answers"
    );
    answers.sort_by(|a, b| a.question_id.cmp(&b.question_id));
    let mut grouped: BTreeMap<CapabilityRef, Vec<InputSourceMatch>> = BTreeMap::new();
    for answer in answers {
        grouped
            .entry(answer.matched.provider.clone())
            .or_default()
            .push(answer.matched);
    }
    ensure!(
        grouped.keys().collect::<BTreeSet<_>>() == candidates.iter().map(|c| &c.provider).collect(),
        "Jev input-source receipt does not cover exactly the projected candidates"
    );
    let priority = |choice: &InputSourceChoice| match choice {
        InputSourceChoice::RequiredSource => 2,
        InputSourceChoice::Uncertain => 1,
        InputSourceChoice::NotRequired => 0,
    };
    // Existential selection: any positive witness selects, otherwise uncertainty
    // survives, and rejection requires every fragment to reject. Probability and
    // confidence remain those of a decisive fragment, not a fabricated aggregate.
    // All-negative groups retain the least confident rejection.
    let matches: Vec<_> = grouped
        .into_values()
        .map(|answers| {
            answers
                .into_iter()
                .max_by(|a, b| {
                    priority(&a.choice).cmp(&priority(&b.choice)).then_with(|| {
                        if a.choice == InputSourceChoice::NotRequired {
                            b.confidence.total_cmp(&a.confidence)
                        } else {
                            a.confidence.total_cmp(&b.confidence)
                        }
                    })
                })
                .expect("nonempty answer group")
        })
        .collect();
    let selected = matches
        .iter()
        .filter(|m| m.choice == InputSourceChoice::RequiredSource)
        .map(|m| m.provider.clone())
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
                operation: plasm_core::catalog_discovery::OperationEvidence {
                    kind: plasm_core::schema::CapabilityKind::Query,
                    receiver: None,
                    contract: "Read abstract records".into(),
                },
                collection: plasm_core::catalog_discovery::CollectionEvidence {
                    meaning: "Abstract records".into(),
                },
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
    fn effects_use_operation_evidence_reads_also_use_collection_evidence() {
        let (_, mut retrieval) = packet();
        let candidate = &mut retrieval.candidates[0];
        candidate.document.operation.kind = plasm_core::schema::CapabilityKind::Action;
        candidate.document.operation.contract = "Activate a record only when suspended".into();
        candidate.document.collection.meaning = "Only records in the preferred collection".into();
        let effect = card(0, candidate);
        assert!(effect.get("collection").is_none());
        assert!(effect.to_string().contains("only when suspended"));
        assert!(!effect.to_string().contains("preferred collection"));
        candidate.document.operation.kind = plasm_core::schema::CapabilityKind::Query;
        assert!(card(0, candidate)
            .to_string()
            .contains("preferred collection"));
    }

    #[test]
    fn current_need_can_replace_ancestor_goals() {
        let (slots, retrieval) = packet();
        let intent = IntentProvenance::from_turns([
            "Activate preferred records".to_owned(),
            "Instead, read the current account balance".to_owned(),
        ])
        .unwrap();
        let batches = issue_batches(JEV_MODEL, &intent, &slots, &retrieval).unwrap();
        assert!(batches[0]
            .body
            .contains("Instead, read the current account balance"));
        assert!(batches[0]
            .body
            .contains("explicit revisions may replace earlier goals"));
        assert!(!batches[0].body.contains("inherited constraint"));
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
    fn input_source_packet_teaches_membership_without_claiming_an_input_binding() {
        use plasm_core::prerequisites::{InputSourceMembership, RowIdentity};
        let source = CapabilityRef {
            catalog: "matrix".into(),
            capability: "records".into(),
        };
        let provider = CapabilityRef {
            catalog: "directory".into(),
            capability: "members".into(),
        };
        let candidates = vec![InputSourceCandidate {
            projection: plasm_core::entity_projection::EntityReadProjection {
                entity: "Record".into(),
                fields: Default::default(),
            },
            provider: provider.clone(),
            bindings: Vec::new(),
            membership: vec![InputSourceMembership {
                source: source.clone(),
                source_identity: RowIdentity::Field {
                    field: "email".into(),
                },
                provider_identity: RowIdentity::Field {
                    field: "email".into(),
                },
            }],
        }];
        let documents = [source, provider]
            .into_iter()
            .map(|reference| {
                let document = CapabilityDocument {
                    capability: reference.capability.clone(),
                    entity: "Record".into(),
                    text: format!("Read identities from {}", reference.catalog),
                    operation: plasm_core::catalog_discovery::OperationEvidence {
                        kind: plasm_core::schema::CapabilityKind::Query,
                        receiver: None,
                        contract: format!("Read identities from {}", reference.catalog),
                    },
                    collection: plasm_core::catalog_discovery::CollectionEvidence {
                        meaning: "Abstract records".into(),
                    },
                    text_hash: "fixture".into(),
                    related_entities: vec![],
                };
                (reference, document)
            })
            .collect();
        let batch = issue_input_source_batches(
            JEV_MODEL,
            &provenance("Select records absent from the directory"),
            &candidates,
            &documents,
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&batch[0].body).unwrap();
        let instructions = body["questions"]["q0"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("membership"));
        assert!(body["state"]["input_source_rule"]
            .as_str()
            .unwrap()
            .contains("absence in a subset does not establish absence in its superset"));
        assert!(body["state"]["capabilities"]
            .to_string()
            .contains("Read identities from matrix"));
        assert!(body["state"]["capabilities"]
            .to_string()
            .contains("Read identities from directory"));
        assert!(instructions.contains("\"input_bindings\":[]"));
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
            projection: plasm_core::entity_projection::EntityReadProjection {
                entity: "Record".into(),
                fields: Default::default(),
            },
            membership: Vec::new(),
            provider: provider.clone(),
            bindings: vec![plasm_core::prerequisites::InputSourceBinding {
                consumer: consumer.clone(),
                input: plasm_core::prerequisites::InputSourceTarget::Argument {
                    input: plasm_core::prerequisites::InputPath {
                        lane: plasm_core::prerequisites::InputLane::Payload,
                        path: vec!["participant_emails".into()],
                    },
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
            operation: plasm_core::catalog_discovery::OperationEvidence {
                kind: plasm_core::schema::CapabilityKind::Query,
                receiver: None,
                contract: "Read abstract records".into(),
            },
            collection: plasm_core::catalog_discovery::CollectionEvidence {
                meaning: "Abstract records".into(),
            },
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
        let receipt = finish_input_sources(matches, &candidates, &[batch]).unwrap();
        assert_eq!(receipt.selected, vec![provider]);
    }
    #[test]
    fn oversized_membership_candidate_is_partitioned_without_loss() {
        use plasm_core::prerequisites::{InputSourceMembership, RowIdentity};
        let provider = CapabilityRef {
            catalog: "matrix".into(),
            capability: "directory".into(),
        };
        let source = CapabilityRef {
            catalog: "matrix".into(),
            capability: "records".into(),
        };
        let candidate = InputSourceCandidate {
            projection: plasm_core::entity_projection::EntityReadProjection {
                entity: "Record".into(),
                fields: Default::default(),
            },
            provider: provider.clone(),
            bindings: vec![],
            membership: (0..1500)
                .map(|i| InputSourceMembership {
                    source: source.clone(),
                    source_identity: RowIdentity::Field {
                        field: format!("identity_{i}").into(),
                    },
                    provider_identity: RowIdentity::Field {
                        field: "email".into(),
                    },
                })
                .collect(),
        };
        let documents = [provider, source]
            .into_iter()
            .map(|reference| {
                let document = CapabilityDocument {
                    capability: reference.capability.clone(),
                    entity: "Record".into(),
                    text: "Read identity-bearing rows".into(),
                    operation: plasm_core::catalog_discovery::OperationEvidence {
                        kind: plasm_core::schema::CapabilityKind::Query,
                        receiver: None,
                        contract: "Read abstract records".into(),
                    },
                    collection: plasm_core::catalog_discovery::CollectionEvidence {
                        meaning: "Abstract records".into(),
                    },
                    text_hash: "fixture".into(),
                    related_entities: vec![],
                };
                (reference, document)
            })
            .collect();
        let batches = issue_input_source_batches(
            JEV_MODEL,
            &provenance("Find records absent from directory"),
            &[candidate],
            &documents,
        )
        .unwrap();
        assert!(batches.len() > 1);
        for batch in &batches {
            assert!(o200k_token_count(&batch.body) <= MAX_BATCH_TOKENS);
        }
        let bodies = batches
            .iter()
            .map(|b| b.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for i in 0..1500 {
            assert_eq!(bodies.matches(&format!("identity_{i}\\\"")).count(), 1);
        }
    }
    fn source_fixture(
        count: usize,
    ) -> (
        InputSourceCandidate,
        BTreeMap<CapabilityRef, CapabilityDocument>,
    ) {
        use plasm_core::prerequisites::{InputSourceMembership, RowIdentity};
        let provider = CapabilityRef {
            catalog: "matrix".into(),
            capability: "directory".into(),
        };
        let source = CapabilityRef {
            catalog: "matrix".into(),
            capability: "records".into(),
        };
        let candidate = InputSourceCandidate {
            projection: plasm_core::entity_projection::EntityReadProjection {
                entity: "Record".into(),
                fields: Default::default(),
            },
            provider: provider.clone(),
            bindings: vec![],
            membership: (0..count)
                .map(|i| InputSourceMembership {
                    source: source.clone(),
                    source_identity: RowIdentity::Field {
                        field: format!("identity_{i}").into(),
                    },
                    provider_identity: RowIdentity::Field {
                        field: "email".into(),
                    },
                })
                .collect(),
        };
        let documents = [provider, source]
            .into_iter()
            .map(|reference| {
                let document = CapabilityDocument {
                    capability: reference.capability.clone(),
                    entity: "Record".into(),
                    text: "Read identity-bearing rows".into(),
                    operation: plasm_core::catalog_discovery::OperationEvidence {
                        kind: plasm_core::schema::CapabilityKind::Query,
                        receiver: None,
                        contract: "Read abstract records".into(),
                    },
                    collection: plasm_core::catalog_discovery::CollectionEvidence {
                        meaning: "Abstract records".into(),
                    },
                    text_hash: "retrieval_metadata_not_for_jev".into(),
                    related_entities: vec![],
                };
                (reference, document)
            })
            .collect();
        (candidate, documents)
    }

    #[test]
    fn source_packet_interns_cards_and_rejects_indivisible_oversize() {
        let (candidate, mut documents) = source_fixture(1);
        let source = candidate.membership[0].source.clone();
        documents.get_mut(&source).unwrap().collection.meaning =
            "UNIQUE_CARD_MARKER ".to_owned() + &"Detailed identity semantics. ".repeat(700);
        let mut candidates = vec![];
        for i in 0..8 {
            let mut next = candidate.clone();
            next.provider.capability = format!("directory_{i}");
            documents.insert(
                next.provider.clone(),
                documents[&candidate.provider].clone(),
            );
            candidates.push(next);
        }
        let batches = issue_input_source_batches(
            JEV_MODEL,
            &provenance("Find absent identities"),
            &candidates,
            &documents,
        )
        .unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].body.matches("UNIQUE_CARD_MARKER").count(), 1);
        assert!(!batches[0].body.contains("retrieval_metadata_not_for_jev"));
        let repeated_cards =
            candidates.len() * o200k_token_count(&documents[&source].collection.meaning);
        let compact = o200k_token_count(&batches[0].body);
        eprintln!("source card repetition alone: {repeated_cards} tokens; full compact packet: {compact} tokens");
        assert!(compact < repeated_cards / 2);
        documents.get_mut(&source).unwrap().collection.meaning =
            "Very large indivisible domain semantics. ".repeat(6000);
        let error = issue_input_source_batches(
            JEV_MODEL,
            &provenance("Find absent identities"),
            &candidates,
            &documents,
        )
        .unwrap_err();
        assert!(error.to_string().contains("indivisible witness"));
    }

    #[test]
    fn oversized_source_document_set_is_partitioned_and_every_card_retained() {
        let (mut candidate, mut documents) = source_fixture(40);
        for (i, witness) in candidate.membership.iter_mut().enumerate() {
            let mut doc = documents[&witness.source].clone();
            witness.source.capability = format!("records_{i}");
            doc.capability = witness.source.capability.clone();
            doc.collection.meaning = format!("CARD_MARKER_{i} ")
                + &"Detailed selection semantics for this domain. ".repeat(250);
            documents.insert(witness.source.clone(), doc);
        }
        let batches = issue_input_source_batches(
            JEV_MODEL,
            &provenance("Find absent identities"),
            &[candidate],
            &documents,
        )
        .unwrap();
        assert!(batches.len() > 1);
        let mut cards = BTreeSet::new();
        for batch in batches {
            assert!(o200k_token_count(&batch.body) <= MAX_BATCH_TOKENS);
            let body: Value = serde_json::from_str(&batch.body).unwrap();
            for card in body["state"]["capabilities"].as_object().unwrap().values() {
                if card["capability"].as_str().unwrap().starts_with("records_") {
                    assert!(cards.insert(card["capability"].as_str().unwrap().to_owned()));
                }
            }
        }
        assert_eq!(cards.len(), 40);
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(32))]
        #[test]
        fn source_fragment_reduction_is_complete_and_order_independent(choices in proptest::collection::vec(0u8..3, 1..24)) {
            let (candidate, _) = source_fixture(1);
            let question = |i: usize| SourceQuestion { id: SourceQuestionId(format!("fragment_{i}")), provider: candidate.provider.clone() };
            let batch = IssuedInputSourceBatch {
                body: String::new(), cache_key: String::new(), model: JEV_MODEL.into(),
                bindings: (0..choices.len()).map(|i| (format!("q{i}"), question(i))).collect(),
            };
            let names = ["not_required", "uncertain", "required_source"];
            let envelope = json!({"model": JEV_MODEL, "provider": "TypeSafe", "answers": choices.iter().enumerate().map(|(i, choice)| {
                (format!("q{i}"), json!({"type": "choice", "choice": names[*choice as usize], "probabilities": names.iter().map(|name| (name.to_string(), if *name == names[*choice as usize] {1.0} else {0.0})).collect::<BTreeMap<_, _>>(), "confidence": (i + 1) as f64 / (choices.len() + 1) as f64}))
            }).collect::<BTreeMap<_, _>>()});
            let mut answers = decode_input_source_batch(&batch, &envelope.to_string()).unwrap();
            let receipt = finish_input_sources(answers.clone(), std::slice::from_ref(&candidate), std::slice::from_ref(&batch)).unwrap();
            let expected = match *choices.iter().max().unwrap() { 2 => InputSourceChoice::RequiredSource, 1 => InputSourceChoice::Uncertain, _ => InputSourceChoice::NotRequired };
            proptest::prop_assert_eq!(&receipt.matches[0].choice, &expected);
            proptest::prop_assert_eq!(receipt.selected.is_empty(), expected != InputSourceChoice::RequiredSource);
            let winning = *choices.iter().max().unwrap();
            let decisive = if winning == 0 { 0 } else { choices.iter().rposition(|choice| *choice == winning).unwrap() };
            proptest::prop_assert_eq!(receipt.matches[0].confidence, (decisive + 1) as f64 / (choices.len() + 1) as f64);

            answers.reverse();
            proptest::prop_assert_eq!(finish_input_sources(answers.clone(), std::slice::from_ref(&candidate), std::slice::from_ref(&batch)).unwrap(), receipt.clone());
            let roundtrip: InputSourceMatchReceipt = serde_json::from_str(&serde_json::to_string(&receipt).unwrap()).unwrap();
            proptest::prop_assert_eq!(roundtrip, receipt);
            let removed = answers.pop().unwrap();
            proptest::prop_assert!(finish_input_sources(answers.clone(), std::slice::from_ref(&candidate), std::slice::from_ref(&batch)).is_err());
            answers.push(removed.clone()); answers.push(removed);
            proptest::prop_assert!(finish_input_sources(answers, &[candidate], &[batch]).is_err());
        }
    }
    #[test]
    fn oversized_final_slot_question_cannot_escape_after_batch_flush() {
        let (slots, mut retrieval) = packet();
        let mut oversized = retrieval.candidates[0].clone();
        oversized.id = "rev/z.read".into();
        oversized.reference.capability = "z.read".into();
        oversized.document.operation.contract =
            "Large domain card with many details. ".repeat(6000);
        retrieval.candidates.push(oversized);
        assert!(
            issue_batches(JEV_MODEL, &provenance("Read balances"), &slots, &retrieval).is_err()
        );
    }
}
