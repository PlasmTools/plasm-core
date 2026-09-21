use super::atoms::FieldPath;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Predicate/template values in the Plasm comp DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlasmDataValue {
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
