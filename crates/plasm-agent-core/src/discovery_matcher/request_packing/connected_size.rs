//! Exact JSON byte accounting for repeated connected-packet trials.
//! Static values are serialized once; only shared-reference IDs vary per trial.
use super::*;

type DefinitionIdentity = (String, String, String);

pub(crate) struct PreparedLocal {
    model: String,
    context: Context,
    context_size: usize,
    model_size: usize,
    definitions: Vec<(DefinitionIdentity, usize)>,
    bindings: Vec<(String, usize)>,
    evidence_size: usize,
    questions: BTreeMap<String, usize>,
}

fn size(value: &impl Serialize) -> Result<usize, DiscoveryMatcherError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(DiscoveryMatcherError::Packing)
}

/// JSON object with pre-serialized value lengths. Keys still use serde escaping.
fn object_size(fields: &BTreeMap<String, usize>) -> Result<usize, DiscoveryMatcherError> {
    let mut bytes = 2 + fields.len().saturating_sub(1);
    for (key, value) in fields {
        bytes += size(key)? + 1 + value;
    }
    Ok(bytes)
}

impl PreparedLocal {
    pub(crate) fn for_scope(
        model: &str,
        intent: &super::super::IntentProvenance,
        id: &str,
        scope: &witnesses::Scope,
    ) -> Result<Self, DiscoveryMatcherError> {
        let context = Context {
            intent_turns: intent.turns().map(str::to_owned).collect(),
            rule: super::super::RUBRIC.into(),
            discovery_need: intent.current().into(),
            outcome: intent.current().into(),
        };
        let questions = BTreeMap::from([(
            format!("q{}", &id[1..]),
            Question {
                kind: "choice".into(),
                instructions: super::super::local_question_instructions(id),
                criteria: super::super::CRITERIA
                    .into_iter()
                    .map(|(k, v)| (k.into(), v.into()))
                    .collect(),
            },
        )]);
        Self::prepare(
            model,
            context,
            &scope.types,
            &scope.bindings,
            size(&serde_json::json!({"capabilities":scope.cards,"type_refs":scope.references}))?,
            questions,
        )
    }

    #[cfg(test)]
    pub(crate) fn new(batch: &IssuedBatch) -> Result<Self, DiscoveryMatcherError> {
        let request: LocalRequest =
            serde_json::from_str(batch.body()).map_err(DiscoveryMatcherError::Packing)?;
        Self::prepare(
            &request.model,
            request.state.context,
            &request.state.evidence.types,
            &request.state.evidence.bindings,
            size(
                &serde_json::json!({"capabilities":request.state.evidence.capabilities,
                "type_refs":request.state.evidence.type_refs}),
            )?,
            request.questions,
        )
    }

    fn prepare(
        model: &str,
        context: Context,
        types: &witnesses::ProjectedDefinitions,
        local_bindings: &[Value],
        evidence_size: usize,
        local_questions: BTreeMap<String, Question>,
    ) -> Result<Self, DiscoveryMatcherError> {
        let mut definitions = Vec::new();
        for (kind, values) in [("values", &types.values), ("records", &types.records)] {
            for (name, value) in values.as_object().expect("validated definition map") {
                definitions.push((
                    (
                        kind.into(),
                        name.clone(),
                        serde_json::to_string(value).map_err(DiscoveryMatcherError::Packing)?,
                    ),
                    size(&serde_json::json!({"kind":kind,"name":name,"definition":value}))?,
                ));
            }
        }
        let bindings = local_bindings
            .iter()
            .map(|binding| {
                let identity =
                    serde_json::to_string(binding).map_err(DiscoveryMatcherError::Packing)?;
                let bytes = identity.len();
                Ok((identity, bytes))
            })
            .collect::<Result<_, DiscoveryMatcherError>>()?;
        let mut questions = BTreeMap::new();
        for (key, question) in local_questions {
            let bytes = size(&connected_question(&key, question))?;
            questions.insert(key, bytes);
        }
        Ok(Self {
            model_size: size(&model)?,
            model: model.into(),
            context_size: size(&connected_context(&context)?)?,
            context,
            definitions,
            bindings,
            evidence_size,
            questions,
        })
    }
}

pub(crate) fn connected_size(group: &[&PreparedLocal]) -> Result<usize, DiscoveryMatcherError> {
    let Some(first) = group.first() else {
        return Ok(0);
    };
    if group
        .iter()
        .any(|r| r.model != first.model || r.context != first.context)
    {
        return Err(DiscoveryMatcherError::InvalidConnectedContext);
    }
    let mut definition_ids = BTreeMap::new();
    let mut definitions = BTreeMap::new();
    let mut binding_ids = BTreeMap::new();
    let mut bindings = BTreeMap::new();
    let mut evidence = BTreeMap::new();
    let mut questions = BTreeMap::new();
    for request in group {
        let mut refs = Vec::new();
        for (identity, bytes) in &request.definitions {
            let next = format!("d{}", definition_ids.len());
            let id = definition_ids.entry(identity).or_insert_with(|| {
                definitions.insert(next.clone(), *bytes);
                next
            });
            refs.push(id.clone());
        }
        let mut binding_refs = Vec::new();
        for (identity, bytes) in &request.bindings {
            let next = format!("b{}", binding_ids.len());
            let id = binding_ids.entry(identity).or_insert_with(|| {
                bindings.insert(next.clone(), *bytes);
                next
            });
            binding_refs.push(id.clone());
        }
        // The base object has two fields; appending each field adds a comma.
        let local_size = request.evidence_size
            + size(&"definition_refs")?
            + 2
            + size(&refs)?
            + size(&"binding_refs")?
            + 2
            + size(&binding_refs)?;
        for (key, bytes) in &request.questions {
            if evidence.insert(key.clone(), local_size).is_some() {
                return Err(DiscoveryMatcherError::DuplicateIssuedQuestion);
            }
            questions.insert(key.clone(), *bytes);
        }
    }
    let mut state_size = first.context_size;
    for (key, fields) in [
        ("definitions", definitions),
        ("binding_witnesses", bindings),
        ("question_evidence", evidence),
    ] {
        state_size += size(&key)? + 2 + object_size(&fields)?;
    }
    object_size(&BTreeMap::from([
        ("model".into(), first.model_size),
        ("questions".into(), object_size(&questions)?),
        ("state".into(), state_size),
    ]))
}
