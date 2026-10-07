//! Canonical logical rowset algebra.
//!
//! This owned IR is the seam between surface parsing and `PlasmComp` lowering. Catalog
//! references are resolved before construction; runtime rows and CML wire environments never
//! enter this module.

mod normalize;

pub use normalize::normalize_query_expr_to_rowset;

use crate::cgs_federation::QualifiedEntityKey;
use crate::identity::{CapabilityName, CapabilityParamName, RelationName};
use crate::plasm_monad::{AggregateSpec, BindingName, ComputeOp, FieldPath};
use crate::{CompOp, QueryPagination, RowPredicate, TypedComparisonValue};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RowsetNormalizeError {
    #[error("capability input contract: {source}")]
    InputContract {
        #[source]
        source: Box<crate::TypeError>,
    },
    #[error(transparent)]
    Capability(Box<crate::query_resolve::QueryCapabilityResolveError>),
    #[error(transparent)]
    FieldPath(#[from] crate::plasm_monad::PlanAtomError),
    #[error("query predicate form is not a valid source selection")]
    InvalidPredicate { reason: SourcePredicateError },
    #[error("RA-1: `{field}` is a capability control, not a query selection argument")]
    CapabilityControl { field: String },
    #[error(
        "RA-2: `{field}` is not a declared selection/scope parameter for capability `{capability}`"
    )]
    UndeclaredSelection { field: String, capability: String },
}

impl From<crate::query_resolve::QueryCapabilityResolveError> for RowsetNormalizeError {
    fn from(error: crate::query_resolve::QueryCapabilityResolveError) -> Self {
        Self::Capability(Box::new(error))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SourcePredicateError {
    #[error("false predicates cannot be expressed as source selection")]
    False,
    #[error("OR predicates cannot be expressed as source selection")]
    Disjunction,
    #[error("NOT predicates cannot be expressed as source selection")]
    Negation,
    #[error("relation existence predicates belong on materialized rows")]
    RelationExistence,
}
use serde::{Deserialize, Serialize};

/// Parent identity for one external invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParentScope {
    #[default]
    Root,
    Relation {
        parent: BindingName,
        relation: RelationName,
    },
}

/// Semantics-visible invocation controls. Runtime safety policy is deliberately separate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct InvocationControls {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagination: Option<QueryPagination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hydrate: Option<bool>,
}

/// One catalog-declared backend-selection slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackendSelectionBinding {
    pub slot: CapabilityParamName,
    pub op: CompOp,
    pub value: TypedComparisonValue,
}

/// Backend selection is distinct from a materialized-row [`RowPredicate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BackendSelection(pub Vec<BackendSelectionBinding>);

/// Resolved source of a logical rowset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowSource {
    External {
        qualified_entity: QualifiedEntityKey,
        capability: CapabilityName,
        #[serde(default)]
        parent_scope: ParentScope,
        #[serde(default)]
        controls: InvocationControls,
    },
    Binding {
        binding: BindingName,
        qualified_entity: QualifiedEntityKey,
    },
}

/// Ordered row-preserving transforms after backend selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowTransform {
    Relation { relation: RelationName },
    Filter { predicate: RowPredicate },
    Compute { op: Box<ComputeOp> },
}

/// Terminal rowset operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowTerminal {
    #[default]
    Rows,
    Aggregate {
        specs: Vec<AggregateSpec>,
    },
}

/// One resolved, canonical source → select → transform → project → aggregate pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedRowset {
    pub source: RowSource,
    #[serde(default)]
    pub selection: BackendSelection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transforms: Vec<RowTransform>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projection: Vec<FieldPath>,
    #[serde(default)]
    pub terminal: RowTerminal,
}
