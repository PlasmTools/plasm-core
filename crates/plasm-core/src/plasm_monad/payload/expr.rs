use super::value::PlanInputBinding;
use serde::{Deserialize, Serialize};

/// Executable Plasm IR for a program-plan node. `display_expr` is inert provenance only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanExprIr {
    pub expr: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_expr: Option<String>,
}

/// IR template with value holes. The `expr` JSON must become `crate::Expr`
/// after holes are instantiated; strings are never reparsed as Plasm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanExprTemplate {
    pub expr: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_expr: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_bindings: Vec<PlanInputBinding>,
}
