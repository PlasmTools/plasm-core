//! Idempotent reconcile and conflict mapping for mutating capabilities.

use indexmap::IndexMap;
use plasm_core::preflight::PLASM_EXISTENCE_SKIP_WRITE_ENV;
use plasm_core::schema::{CapabilityKind, CapabilitySchema};
use plasm_core::{
    conflict_rules_from_mapping_template, GetExpr, QueryExpr, ReconcileBindSource, Value,
    WorkflowConflict, WorkflowConflictKind, WriteOutcome, CGS, CompOp, Predicate,
};
use plasm_core::TypedFieldValue;
use plasm_core::plasm_value_to_json;
use serde_json::Value as JsonValue;

use crate::api_error_detail::workflow_conflict_from_http;
use crate::execution::{ExecutionEngine, ExecutionMode, ExecutionResult, StreamConsumeOpts};
use crate::materialization::SessionMaterialization;
use crate::RuntimeError;

pub fn map_capability_http_error(
    capability: &CapabilitySchema,
    status: u16,
    body: &serde_json::Value,
    fallback_message: String,
) -> RuntimeError {
    if let Some(conflict) =
        workflow_conflict_from_http(&capability.mapping.template.0, status, body)
    {
        let md = conflict.markdown_block();
        return RuntimeError::WorkflowConflict {
            conflict,
            message: format!("{fallback_message}\n\n{md}"),
            attempts: 1,
        };
    }
    RuntimeError::RequestError {
        message: fallback_message,
        attempts: 1,
        status: Some(status),
        body: Some(body.clone()),
    }
}

pub fn extract_http_error_parts(err: &RuntimeError) -> Option<(u16, serde_json::Value, String)> {
    match err {
        RuntimeError::RequestError {
            message,
            status: Some(status),
            body: Some(body),
            ..
        } => Some((*status, body.clone(), message.clone())),
        _ => None,
    }
}

impl ExecutionEngine {
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
        let Some(output) = capability.output_schema.as_ref() else {
            return map_request_to_conflict_or_return(err, capability);
        };
        if !output.idempotent {
            return map_request_to_conflict_or_return(err, capability);
        }
        let Some(reconcile) = &output.reconcile else {
            return Err(err);
        };
        let (status, body, message) = match extract_http_error_parts(&err) {
            Some(parts) => parts,
            None => return Err(err),
        };
        let rules = conflict_rules_from_mapping_template(&capability.mapping.template.0);
        let Some(conflict) = plasm_core::match_conflict_rule(&rules, status, &body) else {
            return Err(err);
        };
        if conflict.kind != reconcile.on {
            let md = conflict.markdown_block();
            return Err(RuntimeError::WorkflowConflict {
                conflict,
                message: format!("{message}\n\n{md}"),
                attempts: 1,
            });
        }
        let via_cap = cgs.get_capability(reconcile.via.as_str()).ok_or_else(|| {
            RuntimeError::ConfigurationError {
                message: format!(
                    "reconcile.via '{}' not found for capability '{}'",
                    reconcile.via, capability.name
                ),
            }
        })?;
        let identity = identity_values_from_env(capability, env_input, reconcile.bind_identity_from);
        let res = self
            .fetch_reconcile_row(via_cap, cgs, mat, mode, &identity, entity)
            .await?;
        if let Some(mismatch) = detect_identity_mismatch(capability, env_input, &res) {
            let md = mismatch.markdown_block();
            return Err(RuntimeError::WorkflowConflict {
                conflict: mismatch,
                message: format!("{message}\n\n{md}"),
                attempts: 1,
            });
        }
        Ok(stamp_outcome_on_result(res, WriteOutcome::Reused))
    }
}

fn map_request_to_conflict_or_return(
    err: RuntimeError,
    capability: &CapabilitySchema,
) -> Result<ExecutionResult, RuntimeError> {
    if let RuntimeError::RequestError {
        message,
        status: Some(status),
        body: Some(body),
        ..
    } = &err
    {
        if let Some(conflict) =
            workflow_conflict_from_http(&capability.mapping.template.0, *status, body)
        {
            let md = conflict.markdown_block();
            return Err(RuntimeError::WorkflowConflict {
                conflict: conflict.clone(),
                message: format!("{message}\n\n{md}"),
                attempts: 1,
            });
        }
    }
    Err(err)
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
        if let Some(v) = map.get(key.as_str()) {
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
    let row = fetched.entities.first()?;
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
    match (a, b) {
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Integer(x), Value::Integer(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => (x - y).abs() < f64::EPSILON,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

impl ExecutionEngine {
    async fn fetch_reconcile_row(
        &self,
        via_cap: &CapabilitySchema,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        identity: &IndexMap<String, Value>,
        entity: &str,
    ) -> Result<ExecutionResult, RuntimeError> {
        match via_cap.kind {
            CapabilityKind::Get => {
                let mut bound = IndexMap::new();
                for (k, v) in identity {
                    if let Value::String(s) = v {
                        bound.insert(k.clone(), s.clone());
                    }
                }
                let target_ent = cgs.get_entity(entity).ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("unknown entity {entity}"),
                })?;
                let bound: std::collections::BTreeMap<String, String> =
                    bound.into_iter().collect();
                let reference =
                    crate::view_plan::ref_from_view_get_node(target_ent, via_cap, &bound)?;
                let get = GetExpr::from_ref(reference);
                self.execute_get(
                    &get,
                    cgs,
                    mat,
                    mode,
                    &crate::view_plan::ViewAmbientContext::default(),
                )
                .await
            }
            CapabilityKind::Query | CapabilityKind::Search => {
                let pred = identity_predicate(identity);
                let q = QueryExpr::filtered(via_cap.domain.as_str(), pred);
                self.execute_query(
                    &q,
                    cgs,
                    mat,
                    mode,
                    StreamConsumeOpts::default(),
                    &crate::view_plan::ViewAmbientContext::default(),
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

pub fn stamp_outcome_on_result(mut result: ExecutionResult, outcome: WriteOutcome) -> ExecutionResult {
    if let Some(row) = result.entities.first_mut() {
        row.fields.insert(
            "outcome".to_string(),
            TypedFieldValue::from_value(Value::String(outcome_label(outcome).into())),
        );
    }
    result
}

pub fn skipped_write_result(entity: &str) -> ExecutionResult {
    ExecutionResult {
        entities: vec![crate::cache::CachedEntity::from_decoded(
            plasm_core::Ref::new(entity, ""),
            IndexMap::from([(
                "outcome".to_string(),
                Value::String("skipped".into()),
            )]),
            IndexMap::new(),
            crate::execution::current_timestamp(),
            crate::cache::EntityCompleteness::Complete,
        )],
        count: 1,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: crate::execution::ExecutionSource::Cache,
        stats: Default::default(),
        request_fingerprints: Vec::new(),
    }
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
    use plasm_core::schema::{CapabilityMapping, CapabilityTemplateJson, OutputSchema, OutputType};
    use plasm_core::{CapabilityName, EntityName, ReconcileSpec};

    fn idempotent_cap() -> CapabilitySchema {
        CapabilitySchema {
            name: CapabilityName::from("workitem_create_idempotent"),
            description: String::new(),
            kind: CapabilityKind::Action,
            domain: EntityName::from("WorkItem"),
            identity_key: Some(vec!["title".into()]),
            mapping: CapabilityMapping {
                template: CapabilityTemplateJson(serde_json::json!({
                    "method": "POST",
                    "conflict_rules": [{
                        "when": { "status": 422, "body_json_path": "message", "contains": "already exists" },
                        "kind": "resource_exists"
                    }]
                })),
            },
            output_schema: Some(OutputSchema {
                output_type: OutputType::Entity {
                    entity_type: "WorkItem".into(),
                },
                decoder: serde_json::json!({}),
                idempotent: true,
                reconcile: Some(ReconcileSpec {
                    on: WorkflowConflictKind::ResourceExists,
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
            entities: vec![crate::cache::CachedEntity::from_decoded(
                plasm_core::Ref::new("WorkItem", ""),
                fields,
                IndexMap::new(),
                0,
                crate::cache::EntityCompleteness::Complete,
            )],
            count: 1,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: crate::execution::ExecutionSource::Cache,
            stats: Default::default(),
            request_fingerprints: Vec::new(),
        };
        let conflict = detect_identity_mismatch(&cap, &input, &fetched).expect("mismatch");
        assert_eq!(conflict.kind, WorkflowConflictKind::IdentityMismatch);
    }

    #[test]
    fn workflow_conflict_from_mapping_template() {
        let cap = idempotent_cap();
        let body = serde_json::json!({ "message": "title already exists" });
        let c = workflow_conflict_from_http(&cap.mapping.template.0, 422, &body).expect("match");
        assert_eq!(c.kind, WorkflowConflictKind::ResourceExists);
    }
}
