//! Scalar equality helpers for view output matching and preflight resolution.

use plasm_core::Value;

use crate::cache::CachedEntity;

/// Compare a row field value to a JSON literal from the view binding.
pub(crate) fn values_semantically_equal(
    row_val: &Value,
    expected_json: &serde_json::Value,
) -> bool {
    let expected = plasm_core::json_value_to_plasm_value(expected_json);
    row_val == &expected
}

/// Canonical string form for row/scope field equality (derived Get unique-match, preflight pick).
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
