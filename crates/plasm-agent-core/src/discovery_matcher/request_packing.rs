//! Store shared definitions once with explicit question-local evidence scopes.
use super::{witnesses, DiscoveryMatcherError, IssuedBatch};
use plasm_core::catalog_discovery::content_hash;
#[cfg(test)]
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

mod connected_size;
pub(super) use connected_size::{connected_lower_bound, connected_size, PreparedLocal};

pub(super) const MAX_QUESTIONS: usize = 4;

#[derive(Clone, Serialize, PartialEq)]
#[cfg_attr(test, derive(Deserialize))]
struct Context {
    rule: String,
    discovery_need: String,
    outcome: String,
}

#[derive(Clone, Serialize)]
#[cfg_attr(test, derive(Deserialize))]
struct Evidence {
    capabilities: witnesses::OrderedCards,
    types: witnesses::ProjectedDefinitions,
    type_refs: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bindings: Vec<Value>,
}

#[derive(Clone)]
#[cfg_attr(test, derive(Deserialize))]
#[cfg_attr(test, serde(deny_unknown_fields))]
struct LocalState {
    #[cfg_attr(test, serde(flatten))]
    context: Context,
    #[cfg_attr(test, serde(flatten))]
    evidence: Evidence,
}

#[derive(Clone, Serialize)]
#[cfg_attr(test, derive(Deserialize))]
#[serde(deny_unknown_fields)]
struct Question {
    #[serde(rename = "type")]
    kind: String,
    instructions: String,
    criteria: BTreeMap<String, String>,
}

#[derive(Clone)]
#[cfg_attr(test, derive(Deserialize))]
#[cfg_attr(test, serde(deny_unknown_fields))]
struct LocalRequest {
    model: String,
    state: LocalState,
    questions: BTreeMap<String, Question>,
}

/// An individually admitted packet retains the typed evidence used to encode it
/// until grouping is finished. Only the finalized wire packet escapes.
pub(super) struct PreparedLocalBatch {
    issued: IssuedBatch,
    request: LocalRequest,
}

impl PreparedLocalBatch {
    pub(super) fn new(
        issued: IssuedBatch,
        model: &str,
        state: super::SharedEvidence<'_>,
        questions: BTreeMap<String, super::decisions::Question>,
    ) -> Self {
        Self {
            issued,
            request: LocalRequest {
                model: model.to_owned(),
                state: LocalState {
                    context: Context {
                        rule: state.rule.to_owned(),
                        discovery_need: state.discovery_need.to_owned(),
                        outcome: state.outcome.to_owned(),
                    },
                    evidence: Evidence {
                        capabilities: state.capabilities,
                        types: state.types,
                        type_refs: state.type_refs,
                        bindings: state.bindings,
                    },
                },
                questions: questions
                    .into_iter()
                    .map(|(id, question)| {
                        (
                            id,
                            Question {
                                kind: question.kind.to_owned(),
                                instructions: question.instructions,
                                criteria: question
                                    .criteria
                                    .into_iter()
                                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                                    .collect(),
                            },
                        )
                    })
                    .collect(),
            },
        }
    }

    #[cfg(test)]
    pub(super) fn into_issued(self) -> IssuedBatch {
        self.issued
    }

    /// Test fixtures may edit raw packet JSON to exercise malformed and divergent
    /// inputs; production never reconstructs a request from its encoded body.
    #[cfg(test)]
    fn from_fixture(issued: IssuedBatch) -> Result<Self, DiscoveryMatcherError> {
        let request = serde_json::from_str(&issued.body).map_err(DiscoveryMatcherError::Packing)?;
        Ok(Self { issued, request })
    }
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
#[cfg(test)]
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
    if context.outcome == context.discovery_need {
        state.remove("discovery_need");
    }
    Ok(state)
}

/// Admission checks the complete serialized body. Never split a connected set
/// into additional provider requests or discard a definition to fit a budget.
#[cfg(test)]
pub(super) fn connected(
    batches: Vec<IssuedBatch>,
) -> Result<Option<IssuedBatch>, DiscoveryMatcherError> {
    let prepared = batches
        .into_iter()
        .map(PreparedLocalBatch::from_fixture)
        .collect::<Result<Vec<_>, _>>()?;
    connected_prepared(prepared)
}

#[cfg(test)]
pub(super) fn connected_prepared(
    batches: Vec<PreparedLocalBatch>,
) -> Result<Option<IssuedBatch>, DiscoveryMatcherError> {
    let Some(body) = connected_request_body(&batches)? else {
        return Ok(None);
    };
    let mut merged = finish(
        batches
            .into_iter()
            .map(PreparedLocalBatch::into_issued)
            .collect(),
        Some(body),
    );
    merged.cache_key =
        content_hash(format!("decisions-connected-references-v13\n{}", merged.body).as_bytes());
    Ok(Some(merged))
}

#[cfg(test)]
fn connected_request_body(
    batches: &[PreparedLocalBatch],
) -> Result<Option<String>, DiscoveryMatcherError> {
    if batches.is_empty() {
        return Ok(None);
    }
    let requests: Vec<LocalRequest> = batches.iter().map(|batch| batch.request.clone()).collect();
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

pub(super) fn pack(
    batches: Vec<PreparedLocalBatch>,
) -> Result<Vec<IssuedBatch>, DiscoveryMatcherError> {
    let mut output = Vec::new();
    let mut group: Vec<IssuedBatch> = Vec::new();
    let mut requests: Vec<LocalRequest> = Vec::new();
    let mut body = None;
    for PreparedLocalBatch {
        issued: batch,
        request,
    } in batches
    {
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

#[cfg(test)]
pub(super) fn pack_fixture(
    batches: Vec<IssuedBatch>,
) -> Result<Vec<IssuedBatch>, DiscoveryMatcherError> {
    pack(
        batches
            .into_iter()
            .map(PreparedLocalBatch::from_fixture)
            .collect::<Result<Vec<_>, _>>()?,
    )
}
