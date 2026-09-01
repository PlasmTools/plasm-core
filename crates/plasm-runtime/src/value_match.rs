//! Scalar equality helpers for view output matching and preflight resolution.

use plasm_core::{TypedFieldValue, Value};

use crate::cache::CachedEntity;
use crate::RuntimeError;

/// Compare a row field value to a JSON literal from the view binding.
pub(crate) fn values_semantically_equal(row_val: &Value, expected_json: &serde_json::Value) -> bool {
    let expected = plasm_core::json_value_to_plasm_value(expected_json);
    row_val == &expected
}

/// Canonical string form for row/scope field equality (view `node_field_where`, preflight pick).
pub fn value_to_match_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => format!("{other:?}"),
    }
}

/// Rows whose `where_field` scalar matches `needle` under [`value_to_match_string`] rules.
pub fn entities_matching_field_value<'a>(
    entities: &'a [CachedEntity],
    where_field: &str,
    needle: &Value,
) -> Vec<&'a CachedEntity> {
    let needle_str = value_to_match_string(needle);
    entities
        .iter()
        .filter(|entity| {
            entity
                .fields
                .get(where_field)
                .is_some_and(|tf| value_to_match_string(&tf.to_value()) == needle_str)
        })
        .collect()
}

/// Pick one output field from the unique row matching `where_field == needle`.
pub fn pick_row_field_where(
    entities: &[CachedEntity],
    node: &str,
    where_field: &str,
    equals_scope: &str,
    needle: &Value,
    field: &str,
) -> Result<Value, RuntimeError> {
    let needle_str = value_to_match_string(needle);
    let matches = entities_matching_field_value(entities, where_field, needle);
    match matches.len() {
        0 => Err(RuntimeError::ConfigurationError {
            message: format!(
                "view node_field_where: no row where {where_field} == {needle_str:?} (scope `{equals_scope}`) on node `{node}` ({} rows)",
                entities.len()
            ),
        }),
        1 => Ok(matches[0]
            .fields
            .get(field)
            .map(TypedFieldValue::to_value)
            .unwrap_or(Value::Null)),
        n => Err(RuntimeError::ConfigurationError {
            message: format!(
                "view node_field_where: {n} rows match {where_field} == {equals_scope} on node `{node}` (ambiguous)"
            ),
        }),
    }
}
