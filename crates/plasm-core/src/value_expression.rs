//! Recursive, context-independent value operations. Consumers own cardinality
//! and authority; operations own type transfer and lazy value evaluation.
use crate::Value;
use crate::{value_contract::ValueContract, ArithOp, FieldType, PlanPredicateOp};
use serde::{Deserialize, Serialize};
use thiserror::Error;
#[cfg(test)]
macro_rules! json { ($($tokens:tt)*) => { crate::json_value_to_plasm_value(&serde_json::json!($($tokens)*)) }; }
mod evaluate;
pub use evaluate::{arithmetic, compare, evaluate_with, ordering, predicate};
pub use evaluate::{ArithmeticError, ComparisonError, WithEvaluationError};

#[derive(Debug, Error)]
pub enum ValueEvaluationError<E: std::fmt::Debug + 'static> {
    #[error("value operand resolution failed")]
    Resolve(E),
    #[error("field access requires a record value")]
    FieldRequiresRecord,
    #[error("field `{field}` is unobserved (not null)")]
    FieldUnobserved { field: String },
    #[error(transparent)]
    Arithmetic(#[from] ArithmeticError),
    #[error("value refinement failed: {0}")]
    Refinement(#[source] crate::value_contract::ValueContractError),
    #[error("length requires a string, array or record")]
    InvalidLengthOperand,
    #[error("condition requires a boolean value")]
    ConditionRequiresBoolean,
    #[error(transparent)]
    Comparison(#[from] ComparisonError),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InferenceError {
    #[error("field contract lookup failed")]
    Field(#[source] crate::value_contract::ValueContractError),
    #[error(transparent)]
    Arithmetic(#[from] crate::value_arithmetic::ArithmeticContractError),
    #[error("condition requires a boolean value")]
    ConditionRequiresBoolean,
    #[error("length requires a string, array or record")]
    InvalidLengthOperand,
    #[error("comparison is unsupported for these value contracts")]
    UnsupportedComparison,
    #[error(transparent)]
    Refinement(#[from] crate::value_contract::ValueContractError),
    #[error("unsupported comparison value contracts")]
    UnsupportedComparisonContracts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ValueOperation<T> {
    Field {
        value: T,
        name: String,
    },
    Arithmetic {
        operator: ArithOp,
        left: T,
        right: T,
    },
    Length {
        value: T,
    },
    /// A branch-proven refinement with a runtime check, never an unchecked cast.
    Refine {
        value: T,
        contract: ValueContract,
    },
    Compare {
        operator: PlanPredicateOp,
        left: T,
        right: T,
    },
    Choose {
        condition: T,
        then: T,
        otherwise: T,
    },
    And {
        left: T,
        right: T,
    },
    Or {
        left: T,
        right: T,
    },
    Not {
        value: T,
    },
}
impl<T> ValueOperation<T> {
    pub fn try_map<U, E>(
        &self,
        mut map: impl FnMut(&T) -> Result<U, E>,
    ) -> Result<ValueOperation<U>, E> {
        use ValueOperation::*;
        Ok(match self {
            Field { value, name } => Field {
                value: map(value)?,
                name: name.clone(),
            },
            Arithmetic {
                operator,
                left,
                right,
            } => Arithmetic {
                operator: *operator,
                left: map(left)?,
                right: map(right)?,
            },
            Length { value } => Length { value: map(value)? },
            Refine { value, contract } => Refine {
                value: map(value)?,
                contract: contract.clone(),
            },
            Compare {
                operator,
                left,
                right,
            } => Compare {
                operator: *operator,
                left: map(left)?,
                right: map(right)?,
            },
            Choose {
                condition,
                then,
                otherwise,
            } => Choose {
                condition: map(condition)?,
                then: map(then)?,
                otherwise: map(otherwise)?,
            },
            And { left, right } => And {
                left: map(left)?,
                right: map(right)?,
            },
            Or { left, right } => Or {
                left: map(left)?,
                right: map(right)?,
            },
            Not { value } => Not { value: map(value)? },
        })
    }
    pub fn infer<E>(
        &self,
        mut resolve: impl FnMut(&T) -> Result<ValueContract, E>,
    ) -> Result<ValueContract, E>
    where
        E: From<InferenceError> + From<crate::value_contract::ValueContractError>,
    {
        use ValueOperation::*;
        let boolean = |value: &ValueContract| -> Result<(), E> {
            if value.summary() == crate::SyntheticValueKind::Boolean {
                Ok(())
            } else {
                Err(InferenceError::ConditionRequiresBoolean.into())
            }
        };
        Ok(match self.try_map(&mut resolve)? {
            Field { value, name } => value.field(&name).map_err(InferenceError::Field)?,
            Arithmetic {
                operator,
                left,
                right,
            } => {
                ValueContract::arithmetic(operator, &left, &right).map_err(InferenceError::from)?
            }
            Length { value } => {
                if !matches!(
                    value.summary(),
                    crate::SyntheticValueKind::String
                        | crate::SyntheticValueKind::Array
                        | crate::SyntheticValueKind::Object
                ) {
                    return Err(InferenceError::InvalidLengthOperand.into());
                }
                let mut result = ValueContract::scalar(FieldType::Integer);
                result.nullable = value.nullable;
                result
            }
            Refine { value, contract } => value.refined_by(&contract)?,
            Compare {
                operator,
                left,
                right,
            } => {
                comparison_contract(operator, &left, &right)?;
                ValueContract::scalar(FieldType::Boolean)
            }
            Choose {
                condition,
                then,
                otherwise,
            } => {
                boolean(&condition)?;
                ValueContract::join(then, otherwise)
            }
            And { left, right } | Or { left, right } => {
                boolean(&left)?;
                boolean(&right)?;
                ValueContract::scalar(FieldType::Boolean)
            }
            Not { value } => {
                boolean(&value)?;
                ValueContract::scalar(FieldType::Boolean)
            }
        })
    }
    pub fn evaluate<E: std::fmt::Debug + 'static>(
        &self,
        mut resolve: impl FnMut(&T) -> Result<Value, E>,
    ) -> Result<Value, ValueEvaluationError<E>> {
        use ValueOperation::*;
        let boolean = |v: Value| {
            if matches!(v, Value::Null) {
                Ok(false)
            } else {
                v.as_bool()
                    .ok_or(ValueEvaluationError::ConditionRequiresBoolean)
            }
        };
        Ok(match self {
            Field { value, name } => resolve(value)
                .map_err(ValueEvaluationError::Resolve)?
                .as_object()
                .ok_or(ValueEvaluationError::FieldRequiresRecord)?
                .get(name)
                .cloned()
                .ok_or_else(|| ValueEvaluationError::FieldUnobserved {
                    field: name.clone(),
                })?,
            Arithmetic {
                operator,
                left,
                right,
            } => arithmetic(
                *operator,
                resolve(left).map_err(ValueEvaluationError::Resolve)?,
                resolve(right).map_err(ValueEvaluationError::Resolve)?,
            )?,
            Refine { value, contract } => {
                let value = resolve(value).map_err(ValueEvaluationError::Resolve)?;
                contract
                    .validate_refinement_value(&value)
                    .map_err(ValueEvaluationError::Refinement)?;
                value
            }
            Length { value } => match resolve(value).map_err(ValueEvaluationError::Resolve)? {
                Value::Null => Value::Null,
                Value::String(s) => Value::from(s.chars().count()),
                Value::Array(a) => Value::from(a.len()),
                Value::Object(o) => Value::from(o.len()),
                _ => return Err(ValueEvaluationError::InvalidLengthOperand),
            },
            Compare {
                operator,
                left,
                right,
            } => Value::Bool(predicate(
                *operator,
                &resolve(left).map_err(ValueEvaluationError::Resolve)?,
                &resolve(right).map_err(ValueEvaluationError::Resolve)?,
            )?),
            Choose {
                condition,
                then,
                otherwise,
            } => {
                let branch = boolean(resolve(condition).map_err(ValueEvaluationError::Resolve)?)?;
                resolve(if branch { then } else { otherwise })
                    .map_err(ValueEvaluationError::Resolve)?
            }
            And { left, right } => Value::Bool(
                boolean(resolve(left).map_err(ValueEvaluationError::Resolve)?)?
                    && boolean(resolve(right).map_err(ValueEvaluationError::Resolve)?)?,
            ),
            Or { left, right } => Value::Bool(
                boolean(resolve(left).map_err(ValueEvaluationError::Resolve)?)?
                    || boolean(resolve(right).map_err(ValueEvaluationError::Resolve)?)?,
            ),
            Not { value } => Value::Bool(!boolean(
                resolve(value).map_err(ValueEvaluationError::Resolve)?,
            )?),
        })
    }
}

impl<T> ValueOperation<T> {
    pub fn render(&self, mut value: impl FnMut(&T) -> String) -> String {
        use ValueOperation::*;
        match self {
            Refine { value: v, contract } => format!("refine({}, {contract:?})", value(v)),
            Field { value: v, name } => format!("{}.{}", value(v), name),
            Arithmetic {
                operator,
                left,
                right,
            } => format!(
                "({} {} {})",
                value(left),
                match operator {
                    ArithOp::Add => "+",
                    ArithOp::Sub => "-",
                    ArithOp::Mul => "*",
                    ArithOp::Div => "/",
                },
                value(right)
            ),
            Length { value: v } => format!("len({})", value(v)),
            Compare {
                operator,
                left,
                right,
            } => format!("({} {:?} {})", value(left), operator, value(right)),
            Choose {
                condition,
                then,
                otherwise,
            } => format!(
                "({} if {} else {})",
                value(then),
                value(condition),
                value(otherwise)
            ),
            And { left, right } => format!("({} and {})", value(left), value(right)),
            Or { left, right } => format!("({} or {})", value(left), value(right)),
            Not { value: v } => format!("not ({})", value(v)),
        }
    }
}

fn comparison_contract(
    op: PlanPredicateOp,
    left: &ValueContract,
    right: &ValueContract,
) -> Result<(), InferenceError> {
    use crate::{value_contract::ValueShape, SyntheticValueKind as K};
    if let ValueShape::Union { variants } = &left.shape {
        return variants
            .iter()
            .try_for_each(|v| comparison_contract(op, v, right));
    }
    if let ValueShape::Union { variants } = &right.shape {
        return variants
            .iter()
            .try_for_each(|v| comparison_contract(op, left, v));
    }
    let (l, r) = (left.summary(), right.summary());
    let numeric = |k| matches!(k, K::Integer | K::Number);
    use PlanPredicateOp::*;
    let valid = match op {
        Eq | Ne | Exists => true,
        In | NotIn => r == K::Array || (l == K::String && r == K::String),
        Contains => l == K::String && r == K::String,
        Lt | Lte | Gt | Gte => {
            l == K::Null
                || r == K::Null
                || (numeric(l) && numeric(r))
                || (l == r && matches!(l, K::String | K::Temporal | K::Boolean | K::Money))
        }
    };
    if valid {
        Ok(())
    } else {
        Err(InferenceError::UnsupportedComparisonContracts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structural_field_selection_preserves_presence_and_union_types() {
        use crate::value_contract::ValueShape;
        use std::collections::{BTreeMap, BTreeSet};
        let integer = ValueContract::scalar(FieldType::Integer);
        let text = ValueContract::scalar(FieldType::String);
        let observed = ValueContract::record(
            BTreeMap::from([("x".into(), integer.clone())]),
            BTreeSet::from(["x".into()]),
        );
        let record = ValueContract::record(
            BTreeMap::from([("x".into(), text.clone())]),
            BTreeSet::new(),
        );
        let union = ValueContract::join(observed.clone(), record);
        let field = ValueOperation::Field {
            value: union,
            name: "x".into(),
        };
        let result = field
            .infer::<crate::value_contract::ValueContractError>(|t| Ok(t.clone()))
            .unwrap();
        assert!(matches!(result.shape, ValueShape::Union { .. }));
        assert_eq!(observed.field("x").unwrap(), integer);
        assert!(observed.field("missing").is_err());
        let op = ValueOperation::Field {
            value: (),
            name: "x".into(),
        };
        assert_eq!(
            op.evaluate(|_| Ok::<_, std::io::Error>(json!({"x": null})))
                .unwrap(),
            Value::Null
        );
        assert!(matches!(
            op.evaluate(|_| Ok::<_, std::io::Error>(json!({}))),
            Err(ValueEvaluationError::FieldUnobserved { field }) if field == "x"
        ));
        assert!(matches!(
            op.evaluate(|_| Ok::<_, std::io::Error>(Value::Null)),
            Err(ValueEvaluationError::FieldRequiresRecord)
        ));
        let wire = serde_json::to_vec(&op).unwrap();
        let restored: ValueOperation<()> = serde_json::from_slice(&wire).unwrap();
        assert_eq!(
            restored
                .evaluate(|_| Ok::<_, std::io::Error>(json!({"x": 7})))
                .unwrap(),
            json!(7)
        );
    }
    #[test]
    fn selection_and_boolean_evaluation_are_lazy() {
        let choice = ValueOperation::Choose {
            condition: "condition",
            then: "chosen",
            otherwise: "absent",
        };
        let mut visited = vec![];
        let result = choice
            .evaluate(|key| {
                visited.push(*key);
                match *key {
                    "condition" => Ok(json!(true)),
                    "chosen" => Ok(json!(42)),
                    _ => Err(std::io::Error::other("unobserved")),
                }
            })
            .unwrap();
        assert_eq!(result, json!(42));
        assert_eq!(visited, vec!["condition", "chosen"]);
        let resolver_error = std::io::Error::other("input unavailable");
        let failed = ValueOperation::Field {
            value: (),
            name: "x".into(),
        }
        .evaluate(|_| Err::<Value, _>(std::io::Error::other("input unavailable")))
        .expect_err("resolver errors remain typed");
        assert!(matches!(
            failed,
            ValueEvaluationError::Resolve(error) if error.kind() == resolver_error.kind()
        ));
        for op in [
            ValueOperation::And {
                left: false,
                right: true,
            },
            ValueOperation::Or {
                left: true,
                right: false,
            },
        ] {
            let mut count = 0;
            op.evaluate(|v| {
                count += 1;
                if count > 1 {
                    Err(std::io::Error::other("must not evaluate"))
                } else {
                    Ok(json!(v))
                }
            })
            .unwrap();
            assert_eq!(count, 1);
        }
    }
    #[test]
    fn comparison_admission_distributes_over_variants() {
        let integer = ValueContract::scalar(FieldType::Integer);
        let boolean = ValueContract::scalar(FieldType::Boolean);
        let mixed = ValueContract::join(integer.clone(), boolean);
        assert!(comparison_contract(PlanPredicateOp::Lt, &mixed, &integer).is_err());
        assert!(comparison_contract(PlanPredicateOp::Eq, &mixed, &integer).is_ok());
        assert!(comparison_contract(PlanPredicateOp::In, &integer, &integer).is_err());
    }
}

#[cfg(test)]
mod refinement_tests {
    use super::*;
    use crate::value_contract::{DomainRef, ValueShape};
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn non_null_refinement_preserves_the_complete_contract_and_checks_runtime_values() {
        let mut original = ValueContract::record(
            BTreeMap::from([(
                "items".into(),
                ValueContract {
                    shape: ValueShape::Array {
                        element: Box::new(ValueContract::scalar(FieldType::Integer)),
                    },
                    domain: None,
                    nullable: false,
                },
            )]),
            BTreeSet::from(["items".into()]),
        );
        original.domain = Some(DomainRef {
            entry_id: "fixture".into(),
            catalog_hash: "pinned".into(),
            value_ref: crate::ValueDomainKey::new("record").unwrap(),
        });
        let mut nullable = original.clone();
        nullable.nullable = true;
        let op = ValueOperation::Refine {
            value: (),
            contract: original.clone(),
        };
        assert_eq!(
            op.infer::<crate::value_contract::ValueContractError>(|_| Ok(nullable.clone()))
                .unwrap(),
            original
        );
        assert_eq!(
            op.evaluate(|_| Ok::<_, std::io::Error>(json!({"items":[1,2]})))
                .unwrap(),
            json!({"items":[1,2]})
        );
        assert!(op
            .evaluate(|_| Ok::<_, std::io::Error>(Value::Null))
            .is_err());
        assert!(op
            .evaluate(|_| Ok::<_, std::io::Error>(json!({"items":["wrong"]})))
            .is_err());
        assert_eq!(
            op.evaluate(|_| Ok::<_, std::io::Error>(json!({}))).unwrap(),
            json!({})
        );
        assert_eq!(
            op.infer::<crate::value_contract::ValueContractError>(|_| Ok(ValueContract {
                shape: ValueShape::Null,
                domain: None,
                nullable: true
            }))
            .unwrap()
            .shape,
            ValueShape::Never
        );
        let wire = serde_json::to_vec(&op).unwrap();
        assert_eq!(
            serde_json::from_slice::<ValueOperation<()>>(&wire).unwrap(),
            op
        );
    }
}
