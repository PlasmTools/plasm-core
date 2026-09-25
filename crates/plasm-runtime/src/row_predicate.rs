//! Shared row field paths and JSON predicate evaluation on [`CachedEntity`].

use crate::cache::CachedEntity;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundRowPredicate {
    pub field_path: plasm_core::FieldPath,
    pub op: plasm_core::PlanPredicateOp,
    pub value: plasm_core::operand_binding::ResolvedValue,
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

pub fn entity_matches_predicates(
    entity: &CachedEntity,
    predicates: &[BoundRowPredicate],
) -> Result<bool, crate::RuntimeError> {
    for predicate in predicates {
        if !entity_matches_predicate(entity, predicate)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn entity_matches_predicate(
    entity: &CachedEntity,
    pred: &BoundRowPredicate,
) -> Result<bool, crate::RuntimeError> {
    let lhs = entity_field_path_value(entity, pred.field_path.segments())
        .unwrap_or(serde_json::Value::Null);
    value_predicate_matches(
        &plasm_core::json_value_to_plasm_value(&lhs),
        pred.op,
        pred.value.value(),
    )
}

pub fn json_matches_predicate(
    value: &serde_json::Value,
    pred: &BoundRowPredicate,
) -> Result<bool, crate::RuntimeError> {
    let lhs =
        json_value_field_path(value, pred.field_path.segments()).unwrap_or(serde_json::Value::Null);
    value_predicate_matches(
        &plasm_core::json_value_to_plasm_value(&lhs),
        pred.op,
        pred.value.value(),
    )
}

pub fn value_predicate_matches(
    lhs: &plasm_core::Value,
    op: plasm_core::PlanPredicateOp,
    rhs: &plasm_core::Value,
) -> Result<bool, crate::RuntimeError> {
    let equal = |lhs: &plasm_core::Value,
                 rhs: &plasm_core::Value|
     -> Result<bool, crate::RuntimeError> {
        if matches!(lhs, plasm_core::Value::Money(_)) || matches!(rhs, plasm_core::Value::Money(_))
        {
            plasm_core::money::values_eq(lhs, rhs)
                .map_err(|e| crate::RuntimeError::from(plasm_core::TypeError::from(e)))
        } else {
            Ok(values_eq_loose(lhs, rhs))
        }
    };
    if (matches!(lhs, plasm_core::Value::Money(_)) || matches!(rhs, plasm_core::Value::Money(_)))
        && matches!(
            op,
            plasm_core::PlanPredicateOp::Lt
                | plasm_core::PlanPredicateOp::Lte
                | plasm_core::PlanPredicateOp::Gt
                | plasm_core::PlanPredicateOp::Gte
        )
    {
        let ordering = plasm_core::money::values_ord(lhs, rhs)
            .map_err(|e| crate::RuntimeError::from(plasm_core::TypeError::from(e)))?;
        return Ok(ordering.is_some_and(|o| match op {
            plasm_core::PlanPredicateOp::Lt => o.is_lt(),
            plasm_core::PlanPredicateOp::Lte => o.is_le(),
            plasm_core::PlanPredicateOp::Gt => o.is_gt(),
            _ => o.is_ge(),
        }));
    }
    Ok(match op {
        plasm_core::PlanPredicateOp::Eq => equal(lhs, rhs)?,
        plasm_core::PlanPredicateOp::Ne => !equal(lhs, rhs)?,
        plasm_core::PlanPredicateOp::Exists => !matches!(lhs, plasm_core::Value::Null),
        plasm_core::PlanPredicateOp::Contains => lhs
            .as_str()
            .zip(rhs.as_str())
            .is_some_and(|(l, r)| l.contains(r)),
        plasm_core::PlanPredicateOp::In | plasm_core::PlanPredicateOp::NotIn => {
            let mut found = false;
            if let Some(items) = rhs.as_array() {
                for item in items {
                    found |= equal(lhs, item)?;
                }
            }
            if op == plasm_core::PlanPredicateOp::NotIn {
                !found
            } else {
                found
            }
        }
        plasm_core::PlanPredicateOp::Lt => compare_ordered(lhs, rhs, |l, r| l < r),
        plasm_core::PlanPredicateOp::Lte => compare_ordered(lhs, rhs, |l, r| l <= r),
        plasm_core::PlanPredicateOp::Gt => compare_ordered(lhs, rhs, |l, r| l > r),
        plasm_core::PlanPredicateOp::Gte => compare_ordered(lhs, rhs, |l, r| l >= r),
    })
}

/// Typed data equality plus bool ↔ boolish-string (`true`/`True`/`false`/`False`).
fn values_eq_loose(lhs: &plasm_core::Value, rhs: &plasm_core::Value) -> bool {
    if lhs == rhs {
        return true;
    }
    match (value_as_boolish(lhs), value_as_boolish(rhs)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn value_as_boolish(v: &plasm_core::Value) -> Option<bool> {
    match v {
        plasm_core::Value::Bool(b) => Some(*b),
        plasm_core::Value::String(s) if s.eq_ignore_ascii_case("true") => Some(true),
        plasm_core::Value::String(s) if s.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

fn compare_ordered(
    lhs: &plasm_core::Value,
    rhs: &plasm_core::Value,
    op: impl Fn(f64, f64) -> bool,
) -> bool {
    let number = |value: &plasm_core::Value| {
        value
            .as_number()
            .or_else(|| value.as_str()?.parse::<f64>().ok())
    };
    number(lhs).zip(number(rhs)).is_some_and(|(l, r)| op(l, r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_compare_unifies_numeric_string_lhs() {
        assert!(value_predicate_matches(
            &plasm_core::Value::String("5".into()),
            plasm_core::PlanPredicateOp::Gt,
            &plasm_core::Value::Integer(0),
        )
        .unwrap());
        assert!(!value_predicate_matches(
            &plasm_core::Value::String("nope".into()),
            plasm_core::PlanPredicateOp::Gt,
            &plasm_core::Value::Integer(0),
        )
        .unwrap());
    }

    #[test]
    fn eq_unifies_bool_and_boolish_strings() {
        assert!(value_predicate_matches(
            &plasm_core::Value::Bool(true),
            plasm_core::PlanPredicateOp::Eq,
            &plasm_core::Value::String("true".into()),
        )
        .unwrap());
        assert!(value_predicate_matches(
            &plasm_core::Value::String("True".into()),
            plasm_core::PlanPredicateOp::Eq,
            &plasm_core::Value::Bool(true),
        )
        .unwrap());
        assert!(!value_predicate_matches(
            &plasm_core::Value::String("True".into()),
            plasm_core::PlanPredicateOp::Eq,
            &plasm_core::Value::Bool(false),
        )
        .unwrap());
    }
}
