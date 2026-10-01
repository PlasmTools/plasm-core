use super::atoms::FieldPath;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Predicate/template values in the Plasm comp DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlasmDataValue {
    /// A lexically bound, pure Boolean reduction over a complete array value.
    Quantified {
        all: bool,
        collection: Box<PlasmDataValue>,
        binding: String,
        predicate: Box<PlasmDataValue>,
    },
    Expression {
        expression: crate::value_expression::ValueOperation<Box<PlasmDataValue>>,
    },
    Literal {
        value: crate::operand_binding::ResolvedValue,
    },
    BindingSymbol {
        binding: String,
        #[serde(default)]
        path: Vec<String>,
    },
    NodeSymbol {
        node: String,
        alias: String,
        #[serde(default)]
        path: Vec<String>,
    },
    Symbol {
        path: String,
    },
    Template {
        #[serde(with = "crate::program_string_template::source_wire")]
        template: crate::program_string_template::CompiledProgramString,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        input_bindings: Vec<PlanInputBinding>,
    },
    EntityRefKey {
        api: String,
        entity: String,
        key: Box<PlasmDataValue>,
    },
    Array {
        #[serde(default)]
        items: Vec<PlasmDataValue>,
    },
    Object {
        #[serde(default)]
        fields: BTreeMap<String, PlasmDataValue>,
    },
}

/// A structured predicate preserved alongside the rendered Plasm expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanPredicate {
    pub field_path: FieldPath,
    pub op: PlanPredicateOp,
    pub value: PlasmDataValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanPredicateOp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
    Contains,
    In,
    /// Row-plane anti-join (`| where field not in rhs`).
    NotIn,
    Exists,
}

/// Reference to a prior node for symbolic `uses_result` edges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanResultUse {
    /// Step id (sandbox-local string).
    pub node: String,
    /// Local binding name.
    pub r#as: String,
}

/// Cardinality contract for a data input consumed by a derived node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputCardinality {
    /// A typed receipt derived from the operation ledger, never an entity row.
    Acknowledgement,
    /// Preserve the entire complete rowset as a typed array, including empty.
    Collection,
    /// Host may broadcast only when the dependency is statically provable as singleton.
    Auto,
    /// The author explicitly requested singleton broadcast; runtime still verifies one row.
    Singleton,
}

fn default_input_cardinality() -> InputCardinality {
    InputCardinality::Auto
}

/// Explicit dataflow input for derived comp steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDataInput {
    pub node: String,
    pub alias: String,
    #[serde(default = "default_input_cardinality")]
    pub cardinality: InputCardinality,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanInputBinding {
    pub from: String,
    pub to: String,
}

impl TryFrom<crate::Value> for PlasmDataValue {
    type Error = String;
    fn try_from(value: crate::Value) -> Result<Self, String> {
        use crate::{PlasmInputRef, Value};
        Ok(match value {
            Value::PlasmInputRef(PlasmInputRef::NodeInput { node, path }) => Self::NodeSymbol {
                alias: node.clone(),
                node,
                path,
            },
            Value::PlasmInputRef(PlasmInputRef::RowBinding { binding, path }) => {
                Self::BindingSymbol { binding, path }
            }
            Value::StringTemplate(template) => {
                let input_bindings = template
                    .roots()
                    .iter()
                    .map(|root| PlanInputBinding {
                        from: root.clone(),
                        to: root.clone(),
                    })
                    .collect();
                Self::Template {
                    template,
                    input_bindings,
                }
            }
            Value::Array(items) => Self::Array {
                items: items
                    .into_iter()
                    .map(Self::try_from)
                    .collect::<Result<_, _>>()?,
            },
            Value::Object(fields) => Self::Object {
                fields: fields
                    .into_iter()
                    .map(|(k, v)| Ok((k, Self::try_from(v)?)))
                    .collect::<Result<_, String>>()?,
            },
            value => Self::Literal {
                value: crate::operand_binding::ResolvedValue::new(value).map_err(str::to_owned)?,
            },
        })
    }
}

impl PlasmDataValue {
    fn substitute_local(&self, binding: &str, value: &crate::Value) -> Result<Self, String> {
        Ok(match self {
            Self::BindingSymbol {
                binding: name,
                path,
            } if name == binding => {
                let mut operand = Self::Literal {
                    value: crate::operand_binding::ResolvedValue::new(value.clone())
                        .map_err(str::to_owned)?,
                };
                for field in path {
                    operand = Self::Expression {
                        expression: crate::value_expression::ValueOperation::Field {
                            value: Box::new(operand),
                            name: field.clone(),
                        },
                    };
                }
                operand
            }
            Self::Expression { expression } => Self::Expression {
                expression: expression
                    .try_map(|v| v.substitute_local(binding, value).map(Box::new))?,
            },
            Self::Quantified {
                all,
                collection,
                binding: local,
                predicate,
            } => Self::Quantified {
                all: *all,
                collection: Box::new(collection.substitute_local(binding, value)?),
                binding: local.clone(),
                predicate: if local == binding {
                    predicate.clone()
                } else {
                    Box::new(predicate.substitute_local(binding, value)?)
                },
            },
            Self::Array { items } => Self::Array {
                items: items
                    .iter()
                    .map(|v| v.substitute_local(binding, value))
                    .collect::<Result<_, _>>()?,
            },
            Self::Object { fields } => Self::Object {
                fields: fields
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), v.substitute_local(binding, value)?)))
                    .collect::<Result<_, String>>()?,
            },
            Self::EntityRefKey { api, entity, key } => Self::EntityRefKey {
                api: api.clone(),
                entity: entity.clone(),
                key: Box::new(key.substitute_local(binding, value)?),
            },
            _ => self.clone(),
        })
    }
    /// Execute only demanded operands; unlike structural binding, branch
    /// evaluation must not read absent fields in an unselected branch.
    pub fn evaluate<R: crate::operand_binding::OperandResolver<Error = String>>(
        &self,
        resolver: &mut R,
    ) -> Result<crate::operand_binding::ResolvedValue, String> {
        self.evaluate_bounded(resolver, &mut 65_536)
    }

    fn evaluate_bounded<R: crate::operand_binding::OperandResolver<Error = String>>(
        &self,
        resolver: &mut R,
        remaining: &mut usize,
    ) -> Result<crate::operand_binding::ResolvedValue, String> {
        use crate::operand_binding::{BindOperands, ResolvedValue};
        match self {
            Self::Quantified {
                all,
                collection,
                binding,
                predicate,
            } => {
                let collection = collection.evaluate_bounded(resolver, remaining)?;
                let crate::Value::Array(items) = collection.value() else {
                    return Err("quantification requires an array".into());
                };
                for item in items {
                    *remaining = remaining
                        .checked_sub(1)
                        .ok_or("quantification exceeds 65536 value occurrences")?;
                    let value = predicate
                        .substitute_local(binding, item)?
                        .evaluate_bounded(resolver, remaining)?;
                    let crate::Value::Bool(value) = value.value() else {
                        return Err("quantified predicate requires a Boolean".into());
                    };
                    if *value != *all {
                        return ResolvedValue::new(crate::Value::Bool(!all)).map_err(str::to_owned);
                    }
                }
                ResolvedValue::new(crate::Value::Bool(*all)).map_err(str::to_owned)
            }
            Self::Expression { expression } => {
                ResolvedValue::new(expression.evaluate(|value| {
                    Ok(value.evaluate_bounded(resolver, remaining)?.into_value())
                })?)
                .map_err(str::to_owned)
            }
            Self::Array { items } => ResolvedValue::new(crate::Value::Array(
                items
                    .iter()
                    .map(|v| {
                        v.evaluate_bounded(resolver, remaining)
                            .map(ResolvedValue::into_value)
                    })
                    .collect::<Result<_, _>>()?,
            ))
            .map_err(str::to_owned),
            Self::Object { fields } => ResolvedValue::new(crate::Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| {
                        Ok((
                            k.clone(),
                            v.evaluate_bounded(resolver, remaining)?.into_value(),
                        ))
                    })
                    .collect::<Result<_, String>>()?,
            ))
            .map_err(str::to_owned),
            _ => self.bind_operands(resolver)?.into_resolved(),
        }
    }

    /// Finish binding without interpreting serialized markers or display strings.
    pub fn into_resolved(self) -> Result<crate::operand_binding::ResolvedValue, String> {
        use crate::{operand_binding::ResolvedValue, Value};
        let value = match self {
            Self::Literal { value } => return Ok(value),
            Self::EntityRefKey { key, .. } => return key.into_resolved(),
            Self::Array { items } => Value::Array(
                items
                    .into_iter()
                    .map(|v| v.into_resolved().map(ResolvedValue::into_value))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Object { fields } => Value::Object(
                fields
                    .into_iter()
                    .map(|(k, v)| Ok((k, v.into_resolved()?.into_value())))
                    .collect::<Result<_, String>>()?,
            ),
            unresolved => return Err(format!("unbound data operand {unresolved:?}")),
        };
        ResolvedValue::new(value).map_err(str::to_owned)
    }
}

#[cfg(test)]
mod operand_tests {
    use super::*;
    use crate::{operand_binding::ResolvedValue, PlasmInputRef, Value};

    #[test]
    fn quantified_values_preserve_scope_and_short_circuit() {
        use crate::operand_binding::{IdentityTarget, OperandResolver};
        struct Closed;
        impl OperandResolver for Closed {
            type Error = String;
            fn resolve(&mut self, _: &PlasmInputRef) -> Result<ResolvedValue, String> {
                Err("free input".into())
            }
            fn identity(
                &mut self,
                _: IdentityTarget<'_>,
                _: &PlasmInputRef,
            ) -> Result<crate::EntityId, String> {
                Err("identity".into())
            }
            fn string(
                &mut self,
                _: &crate::program_string_template::CompiledProgramString,
            ) -> Result<String, String> {
                Err("template".into())
            }
        }
        for all in [false, true] {
            let predicate = PlasmDataValue::BindingSymbol {
                binding: "item".into(),
                path: vec!["ok".into()],
            };
            let quantified = |items| PlasmDataValue::Quantified {
                all,
                collection: Box::new(PlasmDataValue::try_from(Value::Array(items)).unwrap()),
                binding: "item".into(),
                predicate: Box::new(predicate.clone()),
            };
            assert_eq!(
                quantified(vec![]).evaluate(&mut Closed).unwrap().value(),
                &Value::Bool(all)
            );
            let first = Value::Object(indexmap::IndexMap::from([("ok".into(), Value::Bool(!all))]));
            let expression = quantified(vec![first, Value::Object(Default::default())]);
            assert!(expression.dependencies().is_empty());
            let restored: PlasmDataValue =
                serde_json::from_value(serde_json::to_value(&expression).unwrap()).unwrap();
            assert_eq!(
                restored.evaluate(&mut Closed).unwrap().value(),
                &Value::Bool(!all)
            );
            assert!(quantified(vec![Value::Object(Default::default())])
                .evaluate(&mut Closed)
                .unwrap_err()
                .contains("unobserved"));
        }
        let captured = PlasmDataValue::Quantified {
            all: false,
            collection: Box::new(PlasmDataValue::NodeSymbol {
                node: "rows".into(),
                alias: "rows".into(),
                path: vec![],
            }),
            binding: "item".into(),
            predicate: Box::new(PlasmDataValue::Expression {
                expression: crate::value_expression::ValueOperation::Compare {
                    operator: PlanPredicateOp::Eq,
                    left: Box::new(PlasmDataValue::BindingSymbol {
                        binding: "item".into(),
                        path: vec![],
                    }),
                    right: Box::new(PlasmDataValue::NodeSymbol {
                        node: "wanted".into(),
                        alias: "wanted".into(),
                        path: vec![],
                    }),
                },
            }),
        };
        assert_eq!(
            captured.dependencies(),
            ["rows".into(), "wanted".into()].into_iter().collect()
        );
    }

    #[test]
    fn literal_marker_objects_remain_inert_data_even_when_nested() {
        for key in [
            "__plasm_hole",
            "__plasm_string_template",
            "__plasm_get_scalar_extract",
            "__plasm_union_ctor",
        ] {
            for value in [
                serde_json::json!({key: "invalid"}),
                serde_json::json!([{"nested": {key: "invalid"}}]),
            ] {
                let wire = serde_json::json!({"kind":"literal", "value":value});
                let data: PlasmDataValue = serde_json::from_value(wire.clone()).unwrap();
                assert!(data.dependencies().is_empty());
                assert_eq!(serde_json::to_value(&data).unwrap(), wire);
                assert!(data.into_resolved().is_ok());
            }
        }
        assert!(ResolvedValue::new(Value::Float(f64::NAN)).is_err());
        assert!(ResolvedValue::new(Value::Float(f64::INFINITY)).is_err());
    }

    #[test]
    fn nested_lowering_preserves_dependencies_across_serialization() {
        let value = Value::Array(vec![Value::Object(
            [
                (
                    "ref".into(),
                    Value::PlasmInputRef(PlasmInputRef::node_output(
                        "source",
                        vec!["owner".into()],
                    )),
                ),
                (
                    "text".into(),
                    Value::StringTemplate(
                        crate::program_string_template::CompiledProgramString::compile(
                            "{{ profile.email }}".into(),
                        )
                        .unwrap(),
                    ),
                ),
                ("literal".into(), Value::String("source.owner".into())),
            ]
            .into_iter()
            .collect(),
        )]);
        let lowered = PlasmDataValue::try_from(value).unwrap();
        let restored: PlasmDataValue =
            serde_json::from_slice(&serde_json::to_vec(&lowered).unwrap()).unwrap();
        assert_eq!(restored, lowered);
        assert_eq!(
            restored.dependencies(),
            ["profile".into(), "source".into()].into_iter().collect()
        );
        assert!(restored.into_resolved().is_err());
    }
}
