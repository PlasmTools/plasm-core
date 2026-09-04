//! Resolve composed-view output bindings from node results and scope.

use indexmap::IndexMap;
use plasm_core::schema::ViewOutputBinding;
use plasm_core::{TypedFieldValue, Value, WriteOutcome};

use crate::cache::CachedEntity;
use crate::execution::ExecutionResult;
use crate::value_match::values_semantically_equal;
use crate::RuntimeError;

pub fn resolve_output_binding(
    binding: &ViewOutputBinding,
    scope: &IndexMap<String, Value>,
    node_results: &IndexMap<String, ExecutionResult>,
    write_outcomes: &IndexMap<String, WriteOutcome>,
) -> Result<Value, RuntimeError> {
    match binding {
        ViewOutputBinding::Scope { param } => Ok(scope.get(param).cloned().unwrap_or(Value::Null)),
        ViewOutputBinding::NodeRowCount { node } => {
            let r = node_results
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view output references unknown node `{node}`"),
                })?;
            Ok(Value::Integer(r.count as i64))
        }
        ViewOutputBinding::NodeField { node, field } => {
            let r = node_results
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view output references unknown node `{node}`"),
                })?;
            let Some(row) = r.entities.first() else {
                return Ok(Value::Null);
            };
            Ok(row
                .fields
                .get(field)
                .map(TypedFieldValue::to_value)
                .unwrap_or(Value::Null))
        }
        ViewOutputBinding::NodeFieldHistogramJson { node, field } => {
            let r = node_results
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view output references unknown node `{node}`"),
                })?;
            Ok(field_histogram_json(&r.entities, field.as_str()))
        }
        ViewOutputBinding::NodeAnyRowFieldEquals {
            node,
            field,
            equals,
        } => {
            let r = node_results
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view output references unknown node `{node}`"),
                })?;
            let hit = r.entities.iter().any(|row| {
                let v = row
                    .fields
                    .get(field)
                    .map(TypedFieldValue::to_value)
                    .unwrap_or(Value::Null);
                values_semantically_equal(&v, equals)
            });
            Ok(Value::Bool(hit))
        }
        ViewOutputBinding::NodeRowCountPositive { node } => {
            let r = node_results
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view output references unknown node `{node}`"),
                })?;
            Ok(Value::Bool(r.count > 0))
        }
        ViewOutputBinding::WriteCreated { node } => Ok(Value::Bool(matches!(
            write_outcomes.get(node),
            Some(WriteOutcome::Created)
        ))),
        ViewOutputBinding::WriteReused { node } => Ok(Value::Bool(matches!(
            write_outcomes.get(node),
            Some(WriteOutcome::Reused)
        ))),
        ViewOutputBinding::WriteSkipped { node } => Ok(Value::Bool(matches!(
            write_outcomes.get(node),
            Some(WriteOutcome::Skipped)
        ))),
        ViewOutputBinding::Computed { .. } => Err(RuntimeError::ConfigurationError {
            message: "computed output bindings are resolved in a separate phase".into(),
        }),
    }
}

fn field_histogram_json(rows: &[CachedEntity], field: &str) -> Value {
    let mut counts: IndexMap<String, i64> = IndexMap::new();
    for row in rows {
        let k = row
            .fields
            .get(field)
            .map(TypedFieldValue::to_value)
            .map(|v| match v {
                Value::String(s) => s,
                Value::Integer(i) => i.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Float(f) => f.to_string(),
                _ => "<non_scalar>".into(),
            })
            .unwrap_or_else(|| "<missing>".into());
        *counts.entry(k).or_insert(0) += 1;
    }
    let obj: serde_json::Map<String, serde_json::Value> = counts
        .into_iter()
        .map(|(k, v)| (k, serde_json::Value::from(v)))
        .collect();
    plasm_core::json_value_to_plasm_value(&serde_json::Value::Object(obj))
}
