//! Shared row field paths and native predicate evaluation on [`CachedEntity`].

use crate::cache::CachedEntity;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A comparison prepared against the declared value domain, before any rows are read.
#[derive(Debug, Clone)]
pub struct PreparedScalarPredicate {
    contract: plasm_core::value_contract::ValueContract,
    op: plasm_core::PlanPredicateOp,
    rhs: plasm_core::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScalarPredicateError {
    #[error(transparent)]
    Ordering(#[from] plasm_core::value_order::OrderingError),
    #[error(transparent)]
    Equality(#[from] plasm_core::value_equality::ValueEqualityError),
    #[error("contains requires a string, array, set, or record with a compatible operand")]
    ContainsRequiresCompatibleOperands,
    #[error("membership requires a string or array on the right")]
    MembershipRequiresCollection,
    #[error("string membership requires a string on the left")]
    StringMembershipRequiresString,
}

impl PreparedScalarPredicate {
    pub fn new(
        contract: plasm_core::value_contract::ValueContract,
        op: plasm_core::PlanPredicateOp,
        mut rhs: plasm_core::Value,
    ) -> Result<Self, plasm_core::CoercionError> {
        use plasm_core::{value_contract::ValueShape, PlanPredicateOp::*};
        if op == Contains && matches!(rhs, plasm_core::Value::String(_)) {
            if let ValueShape::Array { element } | ValueShape::Set { element } = &contract.shape {
                if let ValueShape::Scalar { field_type } = &element.shape {
                    if matches!(
                        field_type,
                        plasm_core::FieldType::Integer
                            | plasm_core::FieldType::Number
                            | plasm_core::FieldType::Boolean
                    ) {
                        rhs = plasm_core::coerce_value_for_field_type(field_type, None, None, rhs)?;
                    }
                }
            }
        }
        if matches!(op, In | NotIn) {
            if let (ValueShape::Scalar { field_type }, plasm_core::Value::Array(items)) =
                (&contract.shape, &mut rhs)
            {
                if matches!(
                    field_type,
                    plasm_core::FieldType::Integer
                        | plasm_core::FieldType::Number
                        | plasm_core::FieldType::Boolean
                ) {
                    for item in items.iter_mut() {
                        if matches!(item, plasm_core::Value::String(_)) {
                            *item = plasm_core::coerce_value_for_field_type(
                                field_type,
                                None,
                                None,
                                item.clone(),
                            )?;
                        }
                    }
                }
            }
        }
        if matches!(op, Eq | Ne | Lt | Lte | Gt | Gte) {
            if let ValueShape::Scalar { field_type } = &contract.shape {
                if matches!(rhs, plasm_core::Value::String(_))
                    && matches!(
                        field_type,
                        plasm_core::FieldType::Integer
                            | plasm_core::FieldType::Number
                            | plasm_core::FieldType::Boolean
                    )
                {
                    rhs = plasm_core::coerce_value_for_field_type(field_type, None, None, rhs)?;
                }
            }
        }
        Ok(Self { contract, op, rhs })
    }

    /// SQL-style null is returned as unknown; the caller owns missing-field policy.
    pub fn evaluate(&self, lhs: &plasm_core::Value) -> Result<Option<bool>, ScalarPredicateError> {
        use plasm_core::{value_equality::Equatable, value_order::Orderable, PlanPredicateOp::*};
        let rhs = &self.rhs;
        if self.op == Exists {
            return Ok(Some(!lhs.is_null()));
        }
        if lhs.is_null() || rhs.is_null() {
            return Ok(None);
        }
        if matches!(self.op, Lt | Lte | Gt | Gte) {
            let order = self.contract.ordering()?.compare_literal(lhs, rhs)?;
            return Ok(Some(match self.op {
                Lt => order.is_lt(),
                Lte => order.is_le(),
                Gt => order.is_gt(),
                Gte => order.is_ge(),
                _ => unreachable!(),
            }));
        }
        if matches!(self.op, Eq | Ne) {
            let equal = match self.contract.ordering() {
                Ok(order) => order.equal_literal(lhs, rhs)?,
                Err(_) => self.contract.equality()?.equivalent(lhs, rhs)?,
            };
            return Ok(Some(if self.op == Eq { equal } else { !equal }));
        }
        Ok(Some(match self.op {
            Contains => {
                match (&self.contract.shape, lhs, rhs) {
                    (
                        plasm_core::value_contract::ValueShape::Scalar {
                            field_type: plasm_core::FieldType::MultiSelect,
                        },
                        plasm_core::Value::Array(items),
                        plasm_core::Value::String(value),
                    ) => {
                        let mut found = false;
                        for item in items {
                            let text = item.as_str().ok_or(plasm_core::value_equality::ValueEqualityError::InvalidRepresentation)?;
                            found |= text == value;
                        }
                        found
                    }
                    (
                        plasm_core::value_contract::ValueShape::Array { element }
                        | plasm_core::value_contract::ValueShape::Set { element },
                        plasm_core::Value::Array(items),
                        value,
                    ) => {
                        let mut found = false;
                        for item in items {
                            if item.is_null() {
                                continue;
                            }
                            found = match element.ordering() {
                                Ok(order) => order.equal_literal(item, value)?,
                                Err(_) => element.equality()?.equivalent(item, value)?,
                            };
                            if found {
                                break;
                            }
                        }
                        found
                    }
                    (
                        plasm_core::value_contract::ValueShape::Dictionary { .. }
                        | plasm_core::value_contract::ValueShape::Record { .. }
                        | plasm_core::value_contract::ValueShape::ObservedRecord { .. }
                        | plasm_core::value_contract::ValueShape::MappingRecord { .. },
                        plasm_core::Value::Object(fields),
                        plasm_core::Value::String(key),
                    ) => fields.contains_key(key),
                    (_, plasm_core::Value::String(left), plasm_core::Value::String(right)) => {
                        left.contains(right)
                    }
                    _ => return Err(ScalarPredicateError::ContainsRequiresCompatibleOperands),
                }
            }
            In | NotIn => {
                let found = if let Some(haystack) = rhs.as_str() {
                    haystack.contains(
                        lhs.as_str()
                            .ok_or(ScalarPredicateError::StringMembershipRequiresString)?,
                    )
                } else if let Some(items) = rhs.as_array() {
                    let mut found = false;
                    let mut unknown = false;
                    for item in items {
                        if item.is_null() {
                            unknown = true;
                            continue;
                        }
                        found = match self.contract.ordering() {
                            Ok(order) => order.equal_literal(lhs, item)?,
                            Err(_) => self.contract.equality()?.equivalent(lhs, item)?,
                        };
                        if found {
                            break;
                        }
                    }
                    if !found && unknown {
                        return Ok(None);
                    }
                    found
                } else {
                    return Err(ScalarPredicateError::MembershipRequiresCollection);
                };
                if self.op == NotIn {
                    !found
                } else {
                    found
                }
            }
            _ => unreachable!(),
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RowPredicateError {
    #[error("prepared predicate count {prepared} does not match requested count {requested}")]
    PreparationMismatch { requested: usize, prepared: usize },
    #[error(transparent)]
    InvalidPath(#[from] plasm_core::plasm_monad::PlanAtomError),
    #[error(transparent)]
    Scalar(#[from] ScalarPredicateError),
    #[error("predicate field `{field}` is unobserved, not null")]
    Unobserved { field: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundRowPredicate {
    pub field_path: plasm_core::FieldPath,
    pub op: plasm_core::PlanPredicateOp,
    pub value: plasm_core::operand_binding::ResolvedValue,
}

#[derive(Debug, Clone)]
pub struct PreparedRowPredicate {
    path: plasm_core::FieldPath,
    scalar: PreparedScalarPredicate,
}

impl PreparedRowPredicate {
    fn normalize_observation(
        &self,
        value: plasm_core::Value,
    ) -> Result<plasm_core::Value, crate::RuntimeError> {
        use plasm_core::{value_contract::ValueShape, FieldType, PlanPredicateOp, Value};
        if self.scalar.op != PlanPredicateOp::Exists && matches!(value, Value::String(_)) {
            if let ValueShape::Scalar { field_type } = &self.scalar.contract.shape {
                if matches!(
                    field_type,
                    FieldType::Integer | FieldType::Number | FieldType::Boolean
                ) {
                    return Ok(plasm_core::coerce_value_for_field_type(
                        field_type, None, None, value,
                    )?);
                }
            }
        }
        Ok(value)
    }

    pub fn from_contract(
        predicate: &BoundRowPredicate,
        contract: plasm_core::value_contract::ValueContract,
    ) -> Result<Self, plasm_core::CoercionError> {
        Ok(Self {
            path: predicate.field_path.clone(),
            scalar: PreparedScalarPredicate::new(
                contract,
                predicate.op,
                predicate.value.value().clone(),
            )?,
        })
    }

    pub fn prepare(
        cgs: &plasm_core::CGS,
        entity: &str,
        predicate: &BoundRowPredicate,
    ) -> Result<Self, crate::RuntimeError> {
        let (head, tail) = predicate
            .field_path
            .segments()
            .split_first()
            .ok_or(crate::RuntimeError::TopKFieldEmpty)?;
        let field = cgs
            .get_entity(entity)
            .and_then(|e| e.fields.get(head.as_str()))
            .ok_or_else(|| crate::RuntimeError::FieldUnknown {
                entity: entity.to_owned(),
                field: head.clone(),
            })?;
        let mut contract = plasm_core::value_contract::ValueContract::from_domain(
            cgs,
            "",
            field.kind.registry_key(),
        )?;
        contract.nullable = !field.required;
        for segment in tail {
            contract = contract.field(segment)?;
        }
        Ok(Self::from_contract(predicate, contract)?)
    }

    pub fn matches_entity(&self, entity: &CachedEntity) -> Result<bool, crate::RuntimeError> {
        Ok(self.evaluate_entity(entity)? == Some(true))
    }

    pub fn evaluate_entity(
        &self,
        entity: &CachedEntity,
    ) -> Result<Option<bool>, crate::RuntimeError> {
        if let Some(field) = self.path.segments().first() {
            require_entity_field_available(entity, field)?;
        }
        let value = entity_field_path_value(entity, self.path.segments()).ok_or_else(|| {
            RowPredicateError::Unobserved {
                field: self.path.dotted(),
            }
        })?;
        let value = self.normalize_observation(value)?;
        Ok(self
            .scalar
            .evaluate(&value)
            .map_err(RowPredicateError::from)?)
    }

    pub fn matches_row(&self, row: &plasm_core::Value) -> Result<bool, crate::RuntimeError> {
        let value = row_value_field_path(row, self.path.segments()).ok_or_else(|| {
            RowPredicateError::Unobserved {
                field: self.path.dotted(),
            }
        })?;
        let value = self.normalize_observation(value)?;
        Ok(self
            .scalar
            .evaluate(&value)
            .map_err(RowPredicateError::from)?
            == Some(true))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::{value_contract::ValueContract, FieldType, PlanPredicateOp as Op, Value};

    #[test]
    fn declared_domains_control_order_and_coercion() {
        let text = PreparedScalarPredicate::new(
            ValueContract::scalar(FieldType::String),
            Op::Gt,
            Value::String("20".into()),
        )
        .unwrap();
        assert_eq!(
            text.evaluate(&Value::String("3".into())).unwrap(),
            Some(true)
        );
        let number = PreparedScalarPredicate::new(
            ValueContract::scalar(FieldType::Integer),
            Op::Gt,
            Value::String("9007199254740992".into()),
        )
        .unwrap();
        assert_eq!(
            number.evaluate(&Value::Integer(9007199254740993)).unwrap(),
            Some(true)
        );
        assert_eq!(number.evaluate(&Value::Null).unwrap(), None);
        let boolean = PreparedScalarPredicate::new(
            ValueContract::scalar(FieldType::Boolean),
            Op::Eq,
            Value::String("true".into()),
        )
        .unwrap();
        assert_eq!(boolean.evaluate(&Value::Bool(true)).unwrap(), Some(true));
    }

    #[test]
    fn row_adapter_coerces_declared_wire_values_and_rejects_missing_fields() {
        let bound = BoundRowPredicate {
            field_path: plasm_core::FieldPath::from_dotted("n").unwrap(),
            op: Op::Gt,
            value: plasm_core::operand_binding::ResolvedValue::new(Value::Integer(
                9007199254740992,
            ))
            .unwrap(),
        };
        let prepared =
            PreparedRowPredicate::from_contract(&bound, ValueContract::scalar(FieldType::Integer))
                .unwrap();
        let row =
            plasm_core::json_value_to_plasm_value(&serde_json::json!({"n":"9007199254740993"}));
        assert!(prepared.matches_row(&row).unwrap());
        let missing = plasm_core::json_value_to_plasm_value(&serde_json::json!({}));
        assert!(matches!(
            prepared.matches_row(&missing),
            Err(crate::RuntimeError::RowPredicate(
                RowPredicateError::Unobserved { .. }
            ))
        ));
    }

    #[test]
    fn temporal_order_and_typed_faults_are_preserved() {
        let contract = plasm_core::temporal_value::TemporalKind::Datetime.contract();
        let pred = PreparedScalarPredicate::new(
            contract,
            Op::Gt,
            Value::String("2024-01-01T00:00:00Z".into()),
        )
        .unwrap();
        assert_eq!(
            pred.evaluate(&Value::String("2024-01-01T01:00:00+02:00".into()))
                .unwrap(),
            Some(false)
        );
        assert!(matches!(
            pred.evaluate(&Value::String("bad".into())),
            Err(ScalarPredicateError::Ordering(_))
        ));
    }

    #[test]
    fn money_currency_errors_and_null_membership_keep_their_causes() {
        let usd = Value::Money(plasm_core::MoneyValue::new(
            "1".parse().unwrap(),
            Some("USD".into()),
        ));
        let eur = Value::Money(plasm_core::MoneyValue::new(
            "2".parse().unwrap(),
            Some("EUR".into()),
        ));
        let contract = ValueContract::scalar(FieldType::Money);
        let ordered = PreparedScalarPredicate::new(contract.clone(), Op::Gt, usd.clone()).unwrap();
        assert!(matches!(
            ordered.evaluate(&eur),
            Err(ScalarPredicateError::Ordering(
                plasm_core::value_order::OrderingError::InvalidMoney(_)
            ))
        ));
        let member = PreparedScalarPredicate::new(
            contract,
            Op::In,
            Value::Array(vec![Value::Null, usd.clone()]),
        )
        .unwrap();
        assert_eq!(member.evaluate(&usd).unwrap(), Some(true));
        let other = Value::Money(plasm_core::MoneyValue::new(
            "3".parse().unwrap(),
            Some("USD".into()),
        ));
        assert_eq!(member.evaluate(&other).unwrap(), None);
    }

    #[test]
    fn contains_respects_declared_collection_element_domain() {
        let contract = ValueContract {
            shape: plasm_core::value_contract::ValueShape::Array {
                element: Box::new(ValueContract::scalar(FieldType::Integer)),
            },
            domain: None,
            nullable: false,
        };
        let predicate =
            PreparedScalarPredicate::new(contract, Op::Contains, Value::Integer(9007199254740993))
                .unwrap();
        assert_eq!(
            predicate
                .evaluate(&Value::Array(vec![
                    Value::Integer(9007199254740992),
                    Value::Integer(9007199254740993)
                ]))
                .unwrap(),
            Some(true)
        );
    }
}
