use super::value::PlanInputBinding;
use serde::{Deserialize, Serialize};

/// Executable Plasm IR for a program-plan node. `display_expr` is inert provenance only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanExprIr {
    pub expr: crate::Expr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_expr: Option<String>,
}

/// Structural expression with deferred operands. Serialization is a wire concern only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanExprTemplate {
    pub expr: crate::Expr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_expr: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_bindings: Vec<PlanInputBinding>,
}
