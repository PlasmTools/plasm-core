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
use crate::{CompOp, QueryPagination, RowPredicate, TypedComparisonValue, Value};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// A validated reference to a prior singleton binding that supplies source execution context.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecutionContextRef(BindingName);

impl ExecutionContextRef {
    pub fn new(binding: impl Into<String>) -> Result<Self, String> {
        let binding = binding.into();
        let mut chars = binding.chars();
        let valid_head = chars
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_');
        if !valid_head || !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
            return Err(format!(
                "execution context binding `{binding}` must be an identifier"
            ));
        }
        BindingName::new(binding).map(Self)
    }

    pub fn binding(&self) -> &BindingName {
        &self.0
    }
}

/// Ephemeral materialized source context.
///
/// This value is never serialized into plans or artifacts; it exists only between plan input
/// materialization and CML invocation-frame construction.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionContext {
    pub reference: ExecutionContextRef,
    pub qualified_entity: QualifiedEntityKey,
    pub fields: IndexMap<String, Value>,
}

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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<ExecutionContextRef>,
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
    Compute { op: ComputeOp },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ra1_context_reference_is_a_binding_not_a_selection_slot() {
        let context = ExecutionContextRef::new("session").expect("valid binding");
        let source = RowSource::External {
            qualified_entity: QualifiedEntityKey::new("app", "Request"),
            capability: CapabilityName::from("request_query"),
            context: Some(context),
            parent_scope: ParentScope::Root,
            controls: InvocationControls::default(),
        };
        let rowset = ResolvedRowset {
            source,
            selection: BackendSelection::default(),
            transforms: Vec::new(),
            projection: Vec::new(),
            terminal: RowTerminal::Rows,
        };
        let json = serde_json::to_value(rowset).expect("serialize rowset");
        assert_eq!(
            json.pointer("/source/context").and_then(|v| v.as_str()),
            Some("session")
        );
        assert!(json
            .pointer("/selection")
            .and_then(|v| v.as_array())
            .is_some_and(Vec::is_empty));
    }

    #[test]
    fn ra5_context_reference_rejects_non_identifiers() {
        assert!(ExecutionContextRef::new("login.result").is_err());
        assert!(ExecutionContextRef::new("session token").is_err());
        assert!(ExecutionContextRef::new("").is_err());
    }
}
