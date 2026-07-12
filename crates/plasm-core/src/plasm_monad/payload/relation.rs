//! Relation traversal payload on a plan step.
//!
//! Field layout must stay aligned with `plasm-agent-core::plasm_plan::PlanRelationTraversal`
//! for wire/serde compatibility across host and monad payloads.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_embed_proof: Option<crate::ValidatedViewEmbedProof>,
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
