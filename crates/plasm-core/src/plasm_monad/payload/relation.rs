use super::atoms::PlanQualifiedEntityKey;
use super::expr::PlanExprIr;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanRelationTraversal {
    pub source: String,
    pub relation: String,
    pub target: PlanQualifiedEntityKey,
    pub cardinality: RelationCardinality,
    pub source_cardinality: RelationSourceCardinality,
    pub expr: String,
    pub ir: PlanExprIr,
    /// Catalog-derived `(cap_param ← parent_field)` witnesses for scoped materialization.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binding_proofs: Vec<crate::RelationBindingProof>,
    #[serde(default, skip_serializing_if = "missing_materialize")]
    pub materialize: Option<crate::RelationMaterialization>,
}

fn missing_materialize(m: &Option<crate::RelationMaterialization>) -> bool {
    m.is_none()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationCardinality {
    One,
    Many,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationSourceCardinality {
    Single,
    Many,
    RuntimeCheckedSingleton,
}
