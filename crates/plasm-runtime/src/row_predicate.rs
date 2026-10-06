//! Shared row field paths and native predicate evaluation on [`CachedEntity`].

use crate::cache::CachedEntity;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RowPredicateError {
    #[error("money comparison has incompatible currencies `{left}` and `{right}`")]
    MoneyCurrencyMismatch { left: String, right: String },
    #[error("ordered predicate operands are incomparable")]
    IncomparableOperands,
    #[error("ordered predicate comparison requires finite numbers")]
    NonFiniteNumber,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundRowPredicate {
    pub field_path: plasm_core::FieldPath,
    pub op: plasm_core::PlanPredicateOp,
    pub value: plasm_core::operand_binding::ResolvedValue,
}

/// Unavailable is an acquisition failure, not a Python null or a false predicate.
/// Call when a field is actually consumed so Boolean short-circuiting stays lazy.
pub(crate) fn require_entity_field_available(
    entity: &CachedEntity,
    field: &str,
) -> Result<(), crate::RuntimeError> {
    if entity.unavailable_fields.contains(field) {
        return Err(crate::RuntimeError::FieldUnavailable {
            reference: entity.reference.clone(),
            field: field.into(),
        });
    }
    Ok(())
}

pub fn entity_field_path_value(
    entity: &CachedEntity,
    path: &[String],
) -> Option<plasm_core::Value> {
    if path.is_empty() {
        return None;
    }
    let mut cur = entity.fields.get(path[0].as_str()).map(|v| v.to_value())?;
    for seg in path.iter().skip(1) {
        cur = cur.get(seg.as_str())?.clone();
    }
    Some(cur)
}

pub fn row_value_field_path(
    value: &plasm_core::Value,
    path: &[String],
) -> Option<plasm_core::Value> {
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
    if let Some(field) = pred.field_path.segments().first() {
        require_entity_field_available(entity, field)?;
    }
    let lhs = entity_field_path_value(entity, pred.field_path.segments())
        .unwrap_or(plasm_core::Value::Null);
    value_predicate_matches(&lhs, pred.op, pred.value.value())
        .map_err(crate::RuntimeError::RowPredicate)
}

pub fn row_matches_predicate(
    value: &plasm_core::Value,
    pred: &BoundRowPredicate,
) -> Result<bool, crate::RuntimeError> {
    let lhs =
        row_value_field_path(value, pred.field_path.segments()).unwrap_or(plasm_core::Value::Null);
    value_predicate_matches(&lhs, pred.op, pred.value.value())
        .map_err(crate::RuntimeError::RowPredicate)
}

pub fn value_predicate_matches(
    lhs: &plasm_core::Value,
    op: plasm_core::PlanPredicateOp,
    rhs: &plasm_core::Value,
) -> Result<bool, RowPredicateError> {
    let equal = |lhs: &plasm_core::Value,
                 rhs: &plasm_core::Value|
     -> Result<bool, RowPredicateError> {
        if matches!(lhs, plasm_core::Value::Money(_)) || matches!(rhs, plasm_core::Value::Money(_))
        {
            plasm_core::money::values_eq(lhs, rhs).map_err(|error| {
                RowPredicateError::MoneyCurrencyMismatch {
                    left: error.left().to_owned(),
                    right: error.right().to_owned(),
                }
            })
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
        let ordering = plasm_core::money::values_ord(lhs, rhs).map_err(|error| {
            RowPredicateError::MoneyCurrencyMismatch {
                left: error.left().to_owned(),
                right: error.right().to_owned(),
            }
        })?;
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
        plasm_core::PlanPredicateOp::Lt
        | plasm_core::PlanPredicateOp::Lte
        | plasm_core::PlanPredicateOp::Gt
        | plasm_core::PlanPredicateOp::Gte => compare_ordered(lhs, rhs, op)?,
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
    op: plasm_core::PlanPredicateOp,
) -> Result<bool, RowPredicateError> {
    fn number(value: &plasm_core::Value) -> Option<std::borrow::Cow<'_, plasm_core::Value>> {
        use plasm_core::Value;
        if value.is_number() {
            return Some(std::borrow::Cow::Borrowed(value));
        }
        let text = value.as_str()?;
        // Residual wire strings retain exact integer magnitude before considering floats.
        let parsed = text
            .parse::<i64>()
            .map(Value::Integer)
            .or_else(|_| text.parse::<u64>().map(Value::Unsigned))
            .or_else(|_| text.parse::<f64>().map(Value::Float))
            .ok()?;
        Some(std::borrow::Cow::Owned(parsed))
    }
    let Some((l, r)) = number(lhs).zip(number(rhs)) else {
        return Err(RowPredicateError::IncomparableOperands);
    };
    plasm_core::value_expression::compare(op, &l, &r)
        .map_err(|_| RowPredicateError::NonFiniteNumber)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_native_and_wire_integers_do_not_round_through_float() {
        use plasm_core::{PlanPredicateOp::Gt, Value};
        for high in [
            Value::Integer(9007199254740993),
            Value::String("9007199254740993".into()),
        ] {
            assert!(value_predicate_matches(&high, Gt, &Value::Integer(9007199254740992)).unwrap());
        }
        assert!(value_predicate_matches(
            &Value::Unsigned(u64::MAX),
            Gt,
            &Value::Unsigned(u64::MAX - 1)
        )
        .unwrap());
    }

    #[test]
    fn ordered_compare_unifies_numeric_string_lhs() {
        assert!(value_predicate_matches(
            &plasm_core::Value::String("5".into()),
            plasm_core::PlanPredicateOp::Gt,
            &plasm_core::Value::Integer(0),
        )
        .unwrap());
        assert_eq!(
            value_predicate_matches(
                &plasm_core::Value::String("nope".into()),
                plasm_core::PlanPredicateOp::Gt,
                &plasm_core::Value::Integer(0),
            )
            .unwrap_err(),
            RowPredicateError::IncomparableOperands
        );
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

    #[test]
    fn malformed_ordered_operands_are_not_silently_treated_as_non_matches() {
        let error = value_predicate_matches(
            &plasm_core::Value::String("nope".into()),
            plasm_core::PlanPredicateOp::Gt,
            &plasm_core::Value::Integer(0),
        )
        .unwrap_err();
        assert_eq!(error, RowPredicateError::IncomparableOperands);

        let error = value_predicate_matches(
            &plasm_core::Value::Float(f64::INFINITY),
            plasm_core::PlanPredicateOp::Gt,
            &plasm_core::Value::Float(0.0),
        )
        .unwrap_err();
        assert_eq!(error, RowPredicateError::NonFiniteNumber);
    }
}
