use super::atoms::FieldPath;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Predicate/template values in the Plasm comp DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlasmDataValue {
    Literal {
        value: serde_json::Value,
    },
    Helper {
        name: String,
        #[serde(default)]
        args: Vec<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<String>,
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
        template: String,
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
