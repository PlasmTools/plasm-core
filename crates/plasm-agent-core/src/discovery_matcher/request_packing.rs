//! Store shared definitions once with explicit question-local evidence scopes.
use super::{witnesses, DiscoveryMatcherError, IssuedBatch};
use plasm_core::catalog_discovery::content_hash;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

mod connected_size;
pub(super) use connected_size::{connected_size, PreparedLocal};

const MAX_QUESTIONS: usize = 4;

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum ContextSource {
    Outcome,
    DiscoveryNeed,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Fragment {
    Statement(usize),
    Source(ContextSource),
}

#[derive(Serialize)]
struct IntentHistory {
    statements: Vec<String>,
    turns: Vec<Vec<Fragment>>,
}

/// Exact text factoring, not summarization: concatenating a turn's references
/// reconstructs every byte, including negations, conditions and revision order.
fn compact_history(context: &Context) -> IntentHistory {
    let mut ids = BTreeMap::<String, usize>::new();
    let mut statements = Vec::new();
    let turns = context
        .intent_turns
        .iter()
        .map(|turn| {
            if turn == &context.outcome {
                return vec![Fragment::Source(ContextSource::Outcome)];
            }
            if turn == &context.discovery_need {
                return vec![Fragment::Source(ContextSource::DiscoveryNeed)];
            }
            turn.split_inclusive(['.', ';', '\n'])
                .map(|text| {
                    let next = statements.len();
                    let id = *ids.entry(text.to_owned()).or_insert_with(|| {
                        statements.push(text.to_owned());
                        next
                    });
                    Fragment::Statement(id)
                })
                .collect()
        })
        .collect();
    IntentHistory { statements, turns }
}

#[cfg(test)]
mod ancestry_tests {
    use super::*;

    fn roundtrip(context: &Context) {
        let history = compact_history(context);
        let restored: Vec<String> = history
            .turns
            .iter()
            .map(|turn| {
                turn.iter()
                    .map(|fragment| match fragment {
                        Fragment::Statement(id) => history.statements[*id].as_str(),
                        Fragment::Source(ContextSource::Outcome) => context.outcome.as_str(),
                        Fragment::Source(ContextSource::DiscoveryNeed) => {
                            context.discovery_need.as_str()
                        }
                    })
                    .collect()
            })
            .collect();
        assert_eq!(restored, context.intent_turns);
    }

    #[test]
    fn history_roundtrip_retains_revisions_constraints_and_conditional_branches() {
        let outcome = "Inspect specimens. Do not modify originals.".to_owned();
        let need = "Cancel processing; report inspection results only.".to_owned();
        let context = Context {
            outcome: outcome.clone(),
            discovery_need: need.clone(),
            rule: "unchanged".into(),
            intent_turns: vec![
                outcome.clone(),
                outcome,
                "If inspection fails, calibrate; otherwise do not calibrate.\nLimit: 2.5 units."
                    .into(),
                "Never contact external services. ".into(),
                need.clone(),
                "Allow processing.".into(),
                need,
            ],
        };
        roundtrip(&context);
        assert_eq!(
            serde_json::to_string(&compact_history(&context)).unwrap(),
            serde_json::to_string(&compact_history(&context)).unwrap()
        );
    }

    #[test]
    fn current_target_retains_prior_constraints_and_branches_in_order() {
        for need in [
            "Inspect available inputs.",
            "The inspection is complete. Read ambient temperature instead.",
            "Readiness is unknown.",
        ] {
            let context = Context {
                outcome: need.into(),
                discovery_need: need.into(),
                rule: "unchanged".into(),
                intent_turns: vec![
                    "Inspect specimens. Preserve originals. If inspection fails, report missing inputs.".into(),
                    need.into(),
                ],
            };
            roundtrip(&context);
            let packed = connected_context(&context).unwrap();
            assert_eq!(packed["outcome"], need);
            assert_eq!(packed["rule"], "unchanged");
            assert_eq!(
                packed["intent_history"]["turns"][1],
                serde_json::json!(["outcome"])
            );
            assert!(!packed["intent_history"]["statements"]
                .as_array()
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn shared_statements_reduce_bytes_without_discarding_unique_text() {
        let context = Context {outcome:"Inspect".into(), discovery_need:"Report".into(), rule:"unchanged".into(),
            intent_turns:(0..32).map(|n| format!("Revision {n}.{}", " Preserve originals; use local observations only. If verification fails, retain the pending obligation.".repeat(8))).collect()};
        roundtrip(&context);
        assert!(
            serde_json::to_vec(&compact_history(&context))
                .unwrap()
                .len()
                < serde_json::to_vec(&context.intent_turns).unwrap().len() / 2
        );
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
struct Context {
    intent_turns: Vec<String>,
    rule: String,
    discovery_need: String,
    outcome: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct Evidence {
    capabilities: witnesses::OrderedCards,
    types: witnesses::ProjectedDefinitions,
    type_refs: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bindings: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalState {
    #[serde(flatten)]
    context: Context,
    #[serde(flatten)]
    evidence: Evidence,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Question {
    #[serde(rename = "type")]
    kind: String,
    instructions: String,
    criteria: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalRequest {
    model: String,
    state: LocalState,
    questions: BTreeMap<String, Question>,
}

#[derive(Serialize)]
struct SharedState<'a> {
    #[serde(flatten)]
    context: &'a Context,
    types: witnesses::ProjectedDefinitions,
}

#[derive(Serialize)]
struct PackedRequest<'a> {
    model: &'a str,
    state: SharedState<'a>,
    questions: BTreeMap<String, Question>,
}

fn intersection(group: &[LocalRequest], values: bool) -> Value {
    let select = |request: &LocalRequest| {
        if values {
            request.state.evidence.types.values.clone()
        } else {
            request.state.evidence.types.records.clone()
        }
    };
    let first = select(&group[0]);
    let mut common = first
        .as_object()
        .expect("definition map validated during decoding")
        .clone();
    for request in &group[1..] {
        let definitions = select(request);
        common.retain(|name, value| definitions.get(name) == Some(value));
    }
    Value::Object(common)
}

fn factor(group: &[LocalRequest]) -> Result<String, DiscoveryMatcherError> {
    let common = witnesses::ProjectedDefinitions {
        values: intersection(group, true),
        records: intersection(group, false),
    };
    let mut questions = BTreeMap::new();
    for request in group {
        let mut local = request.state.evidence.clone();
        for (definitions, shared) in [
            (&mut local.types.values, &common.values),
            (&mut local.types.records, &common.records),
        ] {
            if let (Some(definitions), Some(shared)) =
                (definitions.as_object_mut(), shared.as_object())
            {
                definitions.retain(|name, _| !shared.contains_key(name));
            }
        }
        let local = serde_json::to_string(&local).map_err(DiscoveryMatcherError::Packing)?;
        for (id, original) in &request.questions {
            let mut question = original.clone();
            question.instructions.push('\n');
            question.instructions.push_str(&local);
            if questions.insert(id.clone(), question).is_some() {
                return Err(DiscoveryMatcherError::DuplicateIssuedQuestion);
            }
        }
    }
    serde_json::to_string(&PackedRequest {
        model: &group[0].model,
        state: SharedState {
            context: &group[0].state.context,
            types: common,
        },
        questions,
    })
    .map_err(DiscoveryMatcherError::Packing)
}

/// One connected request. Definitions are stored once, but every question names
/// only its own evidence. Equal names with different definitions remain distinct.
fn connected_body(group: &[LocalRequest]) -> Result<String, DiscoveryMatcherError> {
    let mut definitions = BTreeMap::<String, Value>::new();
    let mut definition_ids = BTreeMap::<(String, String, String), String>::new();
    let mut bindings = BTreeMap::<String, Value>::new();
    let mut binding_ids = BTreeMap::<String, String>::new();
    let mut evidence = BTreeMap::new();
    let mut questions = BTreeMap::new();
    for request in group {
        let mut refs = Vec::new();
        for (kind, values) in [
            ("values", &request.state.evidence.types.values),
            ("records", &request.state.evidence.types.records),
        ] {
            for (name, value) in values.as_object().expect("validated definition map") {
                let identity = (
                    kind.to_owned(),
                    name.clone(),
                    serde_json::to_string(value).map_err(DiscoveryMatcherError::Packing)?,
                );
                let next = format!("d{}", definition_ids.len());
                let reference = definition_ids.entry(identity).or_insert_with(|| {
                    definitions.insert(
                        next.clone(),
                        serde_json::json!({"kind":kind,"name":name,"definition":value}),
                    );
                    next
                });
                refs.push(reference.clone());
            }
        }
        let mut binding_refs = Vec::new();
        for binding in &request.state.evidence.bindings {
            let identity =
                serde_json::to_string(binding).map_err(DiscoveryMatcherError::Packing)?;
            let next = format!("b{}", binding_ids.len());
            let reference = binding_ids.entry(identity).or_insert_with(|| {
                bindings.insert(next.clone(), binding.clone());
                next
            });
            binding_refs.push(reference.clone());
        }
        for (key, question) in &request.questions {
            let local = serde_json::json!({
                "capabilities": request.state.evidence.capabilities,
                "type_refs": request.state.evidence.type_refs,
                "definition_refs": refs,
                "binding_refs": binding_refs,
            });
            if evidence.insert(key.clone(), local).is_some() {
                return Err(DiscoveryMatcherError::DuplicateIssuedQuestion);
            }
            questions.insert(key.clone(), connected_question(key, question.clone()));
        }
    }
    let mut state = connected_context(&group[0].state.context)?;
    state.insert(
        "definitions".into(),
        serde_json::to_value(definitions).map_err(DiscoveryMatcherError::Packing)?,
    );
    state.insert(
        "binding_witnesses".into(),
        serde_json::to_value(bindings).map_err(DiscoveryMatcherError::Packing)?,
    );
    state.insert(
        "question_evidence".into(),
        serde_json::to_value(evidence).map_err(DiscoveryMatcherError::Packing)?,
    );
    serde_json::to_string(
        &serde_json::json!({"model":group[0].model,"state":state,"questions":questions}),
    )
    .map_err(DiscoveryMatcherError::Packing)
}

fn connected_question(key: &str, mut question: Question) -> Question {
    question.instructions = question
        .instructions
        .replace(
            "`capabilities.",
            &format!("`question_evidence.{key}.capabilities."),
        )
        .replace(
            "`capabilities`",
            &format!("`question_evidence.{key}.capabilities`"),
        );
    question
}

fn connected_context(
    context: &Context,
) -> Result<serde_json::Map<String, Value>, DiscoveryMatcherError> {
    let Value::Object(mut state) =
        serde_json::to_value(context).map_err(DiscoveryMatcherError::Packing)?
    else {
        unreachable!("context serializes as object")
    };
    state.remove("intent_turns");
    if context.intent_turns != [context.outcome.clone()] {
        state.insert(
            "intent_history".into(),
            serde_json::to_value(compact_history(context))
                .map_err(DiscoveryMatcherError::Packing)?,
        );
    }
    if context.outcome == context.discovery_need
        && context.intent_turns == [context.outcome.clone()]
    {
        state.remove("discovery_need");
    }
    Ok(state)
}

/// Admission checks the complete serialized body. Never split a connected set
/// into additional provider requests or discard a definition to fit a budget.
pub(super) fn connected(
    batches: Vec<IssuedBatch>,
) -> Result<Option<IssuedBatch>, DiscoveryMatcherError> {
    let Some(body) = connected_request_body(&batches)? else {
        return Ok(None);
    };
    let mut merged = finish(batches, Some(body));
    merged.cache_key =
        content_hash(format!("decisions-connected-references-v13\n{}", merged.body).as_bytes());
    Ok(Some(merged))
}

fn connected_request_body(
    batches: &[IssuedBatch],
) -> Result<Option<String>, DiscoveryMatcherError> {
    if batches.is_empty() {
        return Ok(None);
    }
    let requests: Vec<LocalRequest> = batches
        .iter()
        .map(|batch| serde_json::from_str(&batch.body).map_err(DiscoveryMatcherError::Packing))
        .collect::<Result<_, _>>()?;
    if requests.iter().any(|request| {
        request.model != requests[0].model || request.state.context != requests[0].state.context
    }) {
        return Err(DiscoveryMatcherError::InvalidConnectedContext);
    }
    connected_body(&requests).map(Some)
}

fn finish(group: Vec<IssuedBatch>, body: Option<String>) -> IssuedBatch {
    let mut batches = group.into_iter();
    let mut merged = batches.next().expect("nonempty packing group");
    if let Some(body) = body {
        merged.cache_key = content_hash(format!("decisions-scoped-sharing-v11\n{body}").as_bytes());
        merged.body = body;
        for batch in batches {
            merged.bindings.extend(batch.bindings);
            #[cfg(test)]
            merged.questions.extend(batch.questions);
        }
    }
    merged
}

pub(super) fn pack(batches: Vec<IssuedBatch>) -> Result<Vec<IssuedBatch>, DiscoveryMatcherError> {
    let mut output = Vec::new();
    let mut group: Vec<IssuedBatch> = Vec::new();
    let mut requests: Vec<LocalRequest> = Vec::new();
    let mut body = None;
    for batch in batches {
        let request: LocalRequest =
            serde_json::from_str(&batch.body).map_err(DiscoveryMatcherError::Packing)?;
        if requests.first().is_some_and(|first| {
            first.model != request.model || first.state.context != request.state.context
        }) || group.len() == MAX_QUESTIONS
        {
            output.push(finish(std::mem::take(&mut group), body.take()));
            requests.clear();
        }
        requests.push(request);
        let proposed = if requests.len() > 1 {
            Some(factor(&requests)?)
        } else {
            None
        };
        let separate_bytes =
            group.iter().map(|issued| issued.body.len()).sum::<usize>() + batch.body.len();
        if proposed
            .as_ref()
            .is_some_and(|body| body.len() > witnesses::MAX_BYTES || body.len() >= separate_bytes)
        {
            let last = requests.pop().expect("new request");
            output.push(finish(std::mem::take(&mut group), body.take()));
            requests.clear();
            requests.push(last);
        } else {
            body = proposed;
        }
        group.push(batch);
    }
    if !group.is_empty() {
        output.push(finish(group, body));
    }
    Ok(output)
}
