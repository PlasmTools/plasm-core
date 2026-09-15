//! Shared row field paths and JSON predicate evaluation on [`CachedEntity`].

use crate::cache::CachedEntity;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRowPredicate {
    pub field_path: Vec<String>,
    pub op: JsonRowPredicateOp,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonRowPredicateOp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
    Contains,
    In,
    NotIn,
    Exists,
}

pub fn entity_field_path_value(
    entity: &CachedEntity,
    path: &[String],
) -> Option<serde_json::Value> {
    if path.is_empty() {
        return None;
    }
    let mut cur = entity
        .fields
        .get(path[0].as_str())
        .map(|v| plasm_core::plasm_value_to_json(&v.to_value()))?;
    for seg in path.iter().skip(1) {
        cur = cur.get(seg.as_str())?.clone();
    }
    Some(cur)
}

pub fn json_value_field_path(
    value: &serde_json::Value,
    path: &[String],
) -> Option<serde_json::Value> {
    if path.is_empty() {
        return None;
    }
    let mut cur = value.get(path[0].as_str())?.clone();
    for seg in path.iter().skip(1) {
        cur = cur.get(seg.as_str())?.clone();
    }
    Some(cur)
}

pub fn entity_matches_predicates(entity: &CachedEntity, predicates: &[JsonRowPredicate]) -> bool {
    predicates
        .iter()
        .all(|p| entity_matches_predicate(entity, p))
}

pub fn json_matches_predicates(value: &serde_json::Value, predicates: &[JsonRowPredicate]) -> bool {
    predicates.iter().all(|p| json_matches_predicate(value, p))
}

pub fn entity_matches_predicate(entity: &CachedEntity, pred: &JsonRowPredicate) -> bool {
    let lhs = entity_field_path_value(entity, &pred.field_path).unwrap_or(serde_json::Value::Null);
    json_predicate_matches(&lhs, pred.op, &pred.value)
}

pub fn json_matches_predicate(value: &serde_json::Value, pred: &JsonRowPredicate) -> bool {
    let lhs = json_value_field_path(value, &pred.field_path).unwrap_or(serde_json::Value::Null);
    json_predicate_matches(&lhs, pred.op, &pred.value)
}

pub fn json_predicate_matches(
    lhs: &serde_json::Value,
    op: JsonRowPredicateOp,
    rhs: &serde_json::Value,
) -> bool {
    match op {
        JsonRowPredicateOp::Eq => json_values_eq_loose(lhs, rhs),
        JsonRowPredicateOp::Ne => !json_values_eq_loose(lhs, rhs),
        JsonRowPredicateOp::Exists => !lhs.is_null(),
        JsonRowPredicateOp::Contains => lhs
            .as_str()
            .zip(rhs.as_str())
            .is_some_and(|(l, r)| l.contains(r)),
        JsonRowPredicateOp::In => rhs
            .as_array()
            .is_some_and(|items| items.iter().any(|item| json_values_eq_loose(item, lhs))),
        JsonRowPredicateOp::NotIn => !rhs
            .as_array()
            .is_some_and(|items| items.iter().any(|item| json_values_eq_loose(item, lhs))),
        JsonRowPredicateOp::Lt => compare_ordered(lhs, rhs, |l, r| l < r),
        JsonRowPredicateOp::Lte => compare_ordered(lhs, rhs, |l, r| l <= r),
        JsonRowPredicateOp::Gt => compare_ordered(lhs, rhs, |l, r| l > r),
        JsonRowPredicateOp::Gte => compare_ordered(lhs, rhs, |l, r| l >= r),
    }
}

/// Strict JSON equality plus bool ↔ boolish-string (`true`/`True`/`false`/`False`).
fn json_values_eq_loose(lhs: &serde_json::Value, rhs: &serde_json::Value) -> bool {
    if lhs == rhs {
        return true;
    }
    match (json_as_boolish(lhs), json_as_boolish(rhs)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn json_as_boolish(v: &serde_json::Value) -> Option<bool> {
    match v {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(s) if s.eq_ignore_ascii_case("true") => Some(true),
        serde_json::Value::String(s) if s.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

fn compare_ordered(
    lhs: &serde_json::Value,
    rhs: &serde_json::Value,
    op: impl Fn(f64, f64) -> bool,
) -> bool {
    plasm_core::compare_unify_json_ordered_numbers(lhs, rhs).is_some_and(|(l, r)| op(l, r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_compare_unifies_numeric_string_lhs() {
        assert!(json_predicate_matches(
            &serde_json::json!("5"),
            JsonRowPredicateOp::Gt,
            &serde_json::json!(0),
        ));
        assert!(!json_predicate_matches(
            &serde_json::json!("nope"),
            JsonRowPredicateOp::Gt,
            &serde_json::json!(0),
        ));
    }

    #[test]
    fn eq_unifies_bool_and_boolish_strings() {
        assert!(json_predicate_matches(
            &serde_json::json!(true),
            JsonRowPredicateOp::Eq,
            &serde_json::json!("true"),
        ));
        assert!(json_predicate_matches(
            &serde_json::json!("True"),
            JsonRowPredicateOp::Eq,
            &serde_json::json!(true),
        ));
        assert!(!json_predicate_matches(
            &serde_json::json!("True"),
            JsonRowPredicateOp::Eq,
            &serde_json::json!(false),
        ));
    }
}
