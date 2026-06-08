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
        JsonRowPredicateOp::Eq => lhs == rhs,
        JsonRowPredicateOp::Ne => lhs != rhs,
        JsonRowPredicateOp::Exists => !lhs.is_null(),
        JsonRowPredicateOp::Contains => lhs
            .as_str()
            .zip(rhs.as_str())
            .is_some_and(|(l, r)| l.contains(r)),
        JsonRowPredicateOp::In => rhs
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item == lhs)),
        JsonRowPredicateOp::Lt => json_number(lhs) < json_number(rhs),
        JsonRowPredicateOp::Lte => json_number(lhs) <= json_number(rhs),
        JsonRowPredicateOp::Gt => json_number(lhs) > json_number(rhs),
        JsonRowPredicateOp::Gte => json_number(lhs) >= json_number(rhs),
    }
}

fn json_number(v: &serde_json::Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_i64().map(|i| i as f64))
        .or_else(|| v.as_u64().map(|u| u as f64))
        .unwrap_or(f64::NAN)
}
