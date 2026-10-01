//! Recursive, context-independent value operations. Consumers own cardinality
//! and authority; operations own type transfer and lazy value evaluation.
use crate::Value;
use crate::{value_contract::ValueContract, ArithOp, FieldType, PlanPredicateOp};
use serde::{Deserialize, Serialize};
#[cfg(test)]
macro_rules! json { ($($tokens:tt)*) => { crate::json_value_to_plasm_value(&serde_json::json!($($tokens)*)) }; }
mod evaluate;
pub use evaluate::{arithmetic, compare, evaluate_with, ordering, predicate};

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
    pub fn infer(
        &self,
        mut resolve: impl FnMut(&T) -> Result<ValueContract, String>,
    ) -> Result<ValueContract, String> {
        use ValueOperation::*;
        let boolean = |value: &ValueContract| {
            if value.summary() == crate::SyntheticValueKind::Boolean {
                Ok(())
            } else {
                Err("condition requires a boolean value".to_string())
            }
        };
        Ok(match self.try_map(&mut resolve)? {
            Field { value, name } => value.field(&name)?,
            Arithmetic {
                operator,
                left,
                right,
            } => ValueContract::arithmetic(operator, &left, &right)?,
            Length { value } => {
                if !matches!(
                    value.summary(),
                    crate::SyntheticValueKind::String
                        | crate::SyntheticValueKind::Array
                        | crate::SyntheticValueKind::Object
                ) {
                    return Err("length requires a string, array or record".into());
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
    pub fn evaluate(
        &self,
        mut resolve: impl FnMut(&T) -> Result<Value, String>,
    ) -> Result<Value, String> {
        use ValueOperation::*;
        let boolean = |v: Value| {
            if matches!(v, Value::Null) {
                Ok(false)
            } else {
                v.as_bool()
                    .ok_or_else(|| "condition requires a boolean value".to_string())
            }
        };
        Ok(match self {
            Field { value, name } => resolve(value)?
                .as_object()
                .ok_or("field access requires a record value")?
                .get(name)
                .cloned()
                .ok_or_else(|| format!("field {name} is unobserved (not null)"))?,
            Arithmetic {
                operator,
                left,
                right,
            } => arithmetic(*operator, resolve(left)?, resolve(right)?)?,
            Refine { value, contract } => {
                let value = resolve(value)?;
                contract.validate_refinement_value(&value)?;
                value
            }
            Length { value } => match resolve(value)? {
                Value::Null => Value::Null,
                Value::String(s) => Value::from(s.chars().count()),
                Value::Array(a) => Value::from(a.len()),
                Value::Object(o) => Value::from(o.len()),
                _ => return Err("length requires a string, array or record".into()),
            },
            Compare {
                operator,
                left,
                right,
            } => Value::Bool(predicate(*operator, &resolve(left)?, &resolve(right)?)?),
            Choose {
                condition,
                then,
                otherwise,
            } => {
                let branch = boolean(resolve(condition)?)?;
                resolve(if branch { then } else { otherwise })?
            }
            And { left, right } => {
                Value::Bool(boolean(resolve(left)?)? && boolean(resolve(right)?)?)
            }
            Or { left, right } => {
                Value::Bool(boolean(resolve(left)?)? || boolean(resolve(right)?)?)
            }
            Not { value } => Value::Bool(!boolean(resolve(value)?)?),
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
) -> Result<(), String> {
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
        Err(format!(
            "unsupported comparison value contract: {l:?} {op:?} {r:?}"
        ))
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
        let result = field.infer(|t| Ok(t.clone())).unwrap();
        assert!(matches!(result.shape, ValueShape::Union { .. }));
        assert_eq!(observed.field("x").unwrap(), integer);
        assert!(observed.field("missing").is_err());
        let op = ValueOperation::Field {
            value: (),
            name: "x".into(),
        };
        assert_eq!(
            op.evaluate(|_| Ok(json!({"x": null}))).unwrap(),
            Value::Null
        );
        assert!(op
            .evaluate(|_| Ok(json!({})))
            .unwrap_err()
            .contains("unobserved"));
        assert!(op
            .evaluate(|_| Ok(Value::Null))
            .unwrap_err()
            .contains("record value"));
        let wire = serde_json::to_vec(&op).unwrap();
        let restored: ValueOperation<()> = serde_json::from_slice(&wire).unwrap();
        assert_eq!(
            restored.evaluate(|_| Ok(json!({"x": 7}))).unwrap(),
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
                    _ => Err("unobserved".into()),
                }
            })
            .unwrap();
        assert_eq!(result, json!(42));
        assert_eq!(visited, vec!["condition", "chosen"]);
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
                    Err("must not evaluate".into())
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
        assert_eq!(op.infer(|_| Ok(nullable.clone())).unwrap(), original);
        assert_eq!(
            op.evaluate(|_| Ok(json!({"items":[1,2]}))).unwrap(),
            json!({"items":[1,2]})
        );
        assert!(op.evaluate(|_| Ok(Value::Null)).is_err());
        assert!(op.evaluate(|_| Ok(json!({"items":["wrong"]}))).is_err());
        assert_eq!(op.evaluate(|_| Ok(json!({}))).unwrap(), json!({}));
        assert_eq!(
            op.infer(|_| Ok(ValueContract {
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
