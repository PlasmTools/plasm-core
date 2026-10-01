//! Declared read-backed postconditions for mutating capabilities; service errors are opaque.

use indexmap::IndexMap;
use plasm_core::plasm_value_to_json;
use plasm_core::preflight::PLASM_EXISTENCE_SKIP_WRITE_ENV;
use plasm_core::schema::{CapabilityKind, CapabilitySchema};
use plasm_core::TypedFieldValue;
use plasm_core::{
    CompOp, Predicate, QueryExpr, ReconcileBindSource, Value, WorkflowConflict,
    WorkflowConflictKind, WriteOutcome, CGS,
};
use serde_json::Value as JsonValue;

use crate::execution::{
    CapabilityParamEnv, ExecutionEngine, ExecutionMode, ExecutionResult, OperationLedger,
    StreamConsumeOpts,
};
use crate::materialization::SessionMaterialization;
use crate::RuntimeError;

impl ExecutionEngine {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn try_reconcile_mutator_error(
        &self,
        err: RuntimeError,
        capability: &CapabilitySchema,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        env_input: &Value,
        entity: &str,
    ) -> Result<ExecutionResult, RuntimeError> {
        // Even an opaque rejection may follow a commit. Reconciliation must not
        // consult a pre-dispatch observation, including when no contract exists.
        mat.poison_read_caches_after_mutation();
        mat.apply_post_mutation_cache_effects(capability, cgs)?;
        let Some(output) = capability
            .output_schema
            .as_ref()
            .filter(|output| output.idempotent)
        else {
            return Err(err);
        };
        let Some(reconcile) = &output.reconcile else {
            return Err(err);
        };
        // Service-level failures are opaque. Only the declared read postcondition
        // can establish that the requested state exists; neither status nor body
        // grants success, no-effect evidence, or retry authority.
        let via_cap = cgs.get_capability(reconcile.via.as_str()).ok_or_else(|| {
            RuntimeError::ConfigurationError {
                message: format!(
                    "reconcile.via '{}' not found for capability '{}'",
                    reconcile.via, capability.name
                ),
            }
        })?;
        let identity =
            identity_values_from_env(capability, env_input, reconcile.bind_identity_from);
        if identity.is_empty()
            || capability
                .identity_key
                .as_ref()
                .is_none_or(|keys| keys.len() != identity.len())
        {
            return Err(err);
        }
        let res = self
            .fetch_reconcile_row(
                via_cap,
                cgs,
                mat,
                mode,
                &identity,
                env_input.as_object().expect("identity object"),
                entity,
            )
            .await?;
        if res.count() != 1
            || res
                .collection
                .materialize(plasm_core::collection_codec::Demand::Whole)
                .is_err()
            || identity.iter().any(|(key, value)| {
                res.entities()
                    .first()
                    .and_then(|row| row.fields.get(key))
                    .is_none_or(|field| !values_equal(value, &field.to_value()))
            })
        {
            return Err(err);
        }
        if let Some(mismatch) = detect_identity_mismatch(capability, env_input, &res) {
            let md = mismatch.markdown_block();
            return Err(RuntimeError::WorkflowConflict {
                conflict: Box::new(mismatch),
                message: md,
                attempts: 1,
            });
        }
        stamp_outcome_on_result(res, WriteOutcome::Reused)
    }
}

fn identity_values_from_env(
    capability: &CapabilitySchema,
    env_input: &Value,
    source: ReconcileBindSource,
) -> IndexMap<String, Value> {
    let mut out = IndexMap::new();
    let Some(keys) = &capability.identity_key else {
        return out;
    };
    let map = match source {
        ReconcileBindSource::Params | ReconcileBindSource::Scope => env_input.as_object(),
    };
    let Some(map) = map else {
        return out;
    };
    for key in keys {
        if let Some(v) = map
            .get(key.as_str())
            .filter(|value| !matches!(value, Value::Null))
        {
            out.insert(key.clone(), v.clone());
        }
    }
    out
}

pub fn detect_identity_mismatch(
    capability: &CapabilitySchema,
    env_input: &Value,
    fetched: &ExecutionResult,
) -> Option<WorkflowConflict> {
    let row = fetched.entities().first()?;
    let input_obj = env_input.as_object()?;
    let identity = capability.identity_key.as_deref().unwrap_or(&[]);
    let mut key_map = IndexMap::new();
    for k in identity {
        if let Some(v) = input_obj.get(k.as_str()) {
            key_map.insert(k.clone(), plasm_value_to_json(v));
            if let Some(tf) = row.fields.get(k.as_str()) {
                if !values_equal(v, &tf.to_value()) {
                    return Some(WorkflowConflict {
                        kind: WorkflowConflictKind::IdentityMismatch,
                        entity: capability.domain.to_string(),
                        key: key_map,
                        hint: format!(
                            "identity_key field '{k}' differs between requested input and existing row"
                        ),
                        existing: Some(row_fields_to_json(&row.fields)),
                    });
                }
            }
        }
    }
    for (k, v) in input_obj {
        if identity.contains(k) {
            continue;
        }
        if let Some(tf) = row.fields.get(k.as_str()) {
            if !values_equal(v, &tf.to_value()) {
                return Some(WorkflowConflict {
                    kind: WorkflowConflictKind::IdentityMismatch,
                    entity: capability.domain.to_string(),
                    key: key_map,
                    hint: format!("field '{k}' differs between requested input and existing row"),
                    existing: Some(row_fields_to_json(&row.fields)),
                });
            }
        }
    }
    None
}

fn row_fields_to_json(fields: &IndexMap<String, TypedFieldValue>) -> IndexMap<String, JsonValue> {
    fields
        .iter()
        .map(|(k, v)| (k.clone(), plasm_value_to_json(&v.to_value())))
        .collect()
}

fn values_equal(a: &Value, b: &Value) -> bool {
    a == b
}

impl ExecutionEngine {
    pub(crate) async fn fetch_reconcile_row(
        &self,
        via_cap: &CapabilitySchema,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        identity: &IndexMap<String, Value>,
        bindings: &IndexMap<String, Value>,
        entity: &str,
    ) -> Result<ExecutionResult, RuntimeError> {
        match via_cap.kind {
            CapabilityKind::Get => {
                let mut bound = IndexMap::new();
                for (k, v) in identity {
                    let token = match v {
                        Value::String(s) => s.clone(),
                        Value::Integer(n) => n.to_string(),
                        _ => {
                            return Err(RuntimeError::ConfigurationError {
                                message: "read-backed identity must be a string or integer".into(),
                            })
                        }
                    };
                    bound.insert(k.clone(), token);
                }
                let target_ent = cgs.get_entity(via_cap.domain.as_str()).ok_or_else(|| {
                    RuntimeError::ConfigurationError {
                        message: format!("unknown entity {entity}"),
                    }
                })?;
                let bound: std::collections::BTreeMap<String, String> = bound.into_iter().collect();
                let reference =
                    crate::view_plan::ref_from_view_get_node(target_ent, via_cap, &bound)?;
                let inherit = CapabilityParamEnv::from_bindings(bindings, via_cap);
                let get =
                    plasm_core::GetExpr::from_ref(reference).with_capability(via_cap.name.clone());
                self.execute_get(
                    &get,
                    cgs,
                    mat,
                    mode,
                    &crate::view_plan::ViewAmbientContext::default()
                        .with_capability_params(inherit.bindings().clone()),
                )
                .await
            }
            CapabilityKind::Query | CapabilityKind::Search => {
                let pred = identity_predicate(identity);
                let q = QueryExpr::filtered(via_cap.domain.as_str(), pred)
                    .with_capability(via_cap.name.clone());
                let inherit = CapabilityParamEnv::from_bindings(bindings, via_cap);
                self.execute_query(
                    &q,
                    cgs,
                    mat,
                    mode,
                    StreamConsumeOpts {
                        fetch_all: true,
                        ..Default::default()
                    },
                    &crate::view_plan::ViewAmbientContext::default()
                        .with_capability_params(inherit.bindings().clone()),
                )
                .await
            }
            _ => Err(RuntimeError::ConfigurationError {
                message: format!(
                    "reconcile via '{}' must be kind get, query, or search (got {:?})",
                    via_cap.name, via_cap.kind
                ),
            }),
        }
    }
}

fn identity_predicate(identity: &IndexMap<String, Value>) -> Predicate {
    let mut pred = Predicate::True;
    for (field, value) in identity {
        let cmp = Predicate::Comparison {
            field: field.clone(),
            op: CompOp::Eq,
            value: value.clone().into(),
        };
        pred = Predicate::And {
            args: vec![pred, cmp],
        };
    }
    pred
}

pub fn stamp_outcome_on_result(
    mut result: ExecutionResult,
    outcome: WriteOutcome,
) -> Result<ExecutionResult, RuntimeError> {
    if let Some(observed) = result.entities().first() {
        let mut row = observed.clone();
        row.fields.insert(
            "outcome".to_string(),
            TypedFieldValue::from_value(Value::String(outcome_label(outcome).into())),
        );
        result.collection = result.collection.replace(0, row)?;
    }
    Ok(result)
}

pub fn skipped_write_result(
    entity: &str,
    identity: plasm_core::collection_codec::CollectionIdentity,
) -> Result<ExecutionResult, RuntimeError> {
    Ok(ExecutionResult {
        collection: crate::execution::ExecutionCollection::observe(
            identity,
            vec![crate::cache::CachedEntity::from_decoded(
                plasm_core::Ref::new(entity, ""),
                IndexMap::from([("outcome".to_string(), Value::String("skipped".into()))]),
                IndexMap::new(),
                crate::execution::current_timestamp(),
                crate::cache::EntityCompleteness::Complete,
            )],
            plasm_core::collection_codec::Observation::ExactOutput { decoded: 1 },
        )?,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: crate::execution::ExecutionSource::Cache,
        stats: Default::default(),
        request_fingerprints: Vec::new(),
        operations: OperationLedger::empty(),
    })
}

pub fn should_skip_write_after_preflight(env: &plasm_compile::CmlEnv) -> bool {
    env.get(PLASM_EXISTENCE_SKIP_WRITE_ENV)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn outcome_label(outcome: WriteOutcome) -> &'static str {
    match outcome {
        WriteOutcome::Created => "created",
        WriteOutcome::Reused => "reused",
        WriteOutcome::Skipped => "skipped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::ResultCoverage;
    use plasm_core::schema::{CapabilityMapping, CapabilityTemplateJson, OutputSchema, OutputType};
    use plasm_core::{CapabilityName, EntityName, ReconcileSpec};

    fn idempotent_cap() -> CapabilitySchema {
        CapabilitySchema {
            name: CapabilityName::from("workitem_create_idempotent"),
            description: String::new(),
            kind: CapabilityKind::Action,
            domain: EntityName::from("WorkItem"),
            identity_key: Some(vec!["title".into()]),
            mapping: Some(CapabilityMapping {
                template: CapabilityTemplateJson(serde_json::json!({
                    "method": "POST",

                })),
            }),
            derived: None,
            output_schema: Some(OutputSchema {
                output_type: OutputType::Entity {
                    entity_type: "WorkItem".into(),
                },
                decoder: serde_json::json!({}),
                idempotent: true,
                reconcile: Some(ReconcileSpec {
                    via: "workitem_query".into(),
                    bind_identity_from: ReconcileBindSource::Params,
                }),
            }),
            ..CapabilitySchema::minimal_test()
        }
    }

    #[test]
    fn detect_identity_mismatch_on_body_field() {
        let cap = idempotent_cap();
        let input = Value::Object(IndexMap::from([
            ("title".into(), Value::String("a".into())),
            ("extra".into(), Value::String("new".into())),
        ]));
        let mut fields = IndexMap::new();
        fields.insert("title".into(), Value::String("a".into()));
        fields.insert("extra".into(), Value::String("old".into()));
        let fetched = ExecutionResult {
            collection: crate::execution::test_collection(
                vec![crate::cache::CachedEntity::from_decoded(
                    plasm_core::Ref::new("WorkItem", ""),
                    fields,
                    IndexMap::new(),
                    0,
                    crate::cache::EntityCompleteness::Complete,
                )],
                ResultCoverage::Complete,
            ),
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: crate::execution::ExecutionSource::Cache,
            stats: Default::default(),
            request_fingerprints: Vec::new(),
            operations: OperationLedger::empty(),
        };
        let conflict = detect_identity_mismatch(&cap, &input, &fetched).expect("mismatch");
        assert_eq!(conflict.kind, WorkflowConflictKind::IdentityMismatch);
    }
}
