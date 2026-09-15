//! Structurally disjoint capability-input lanes (scope, selection, controls,
//! arguments, payload).
//!
//! Nested under [`crate::schema`] so lane types can reference [`super::InputSchema`] /
//! [`super::InputFieldSchema`] without a sibling-module cycle.

use super::{InputFieldSchema, InputSchema};
use serde::{Deserialize, Serialize};

/// Semantic receiver of an operation, independent of its transport mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CapabilityReceiver {
    None,
    Entity { entity: crate::EntityName },
}

/// Parameters derived exclusively from a typed parent row.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParentScopeSchema(pub Vec<InputFieldSchema>);

/// Parameters accepted by source invocation braces to constrain backend row selection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BackendSelectionSchema(pub Vec<InputFieldSchema>);

/// Pagination, sorting, response-shape, and other non-predicate invocation controls.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InvocationControlsSchema(pub Vec<InputFieldSchema>);

/// Canonical, structurally disjoint capability-input algebra.
///
/// Built in Rust from [`crate::loader::DomainCapability`] (not deserialized from
/// `domain.yaml`). Catalog YAML rejects unknown keys such as abolished `execution:`
/// on [`crate::loader::DomainCapability`]'s `deny_unknown_fields`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CapabilityInputs {
    /// Omission follows operation-kind semantics: Get/Update/Delete use their
    /// domain entity; Query/Search/Create/Action have no receiver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<CapabilityReceiver>,
    #[serde(default, skip_serializing_if = "parent_scope_is_empty")]
    pub scope: ParentScopeSchema,
    #[serde(default, skip_serializing_if = "backend_selection_is_empty")]
    pub selection: BackendSelectionSchema,
    #[serde(default, skip_serializing_if = "invocation_controls_is_empty")]
    pub controls: InvocationControlsSchema,
    /// Named invocation arguments which are not an HTTP/body payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<InputSchema>,
    /// Create/update/action body payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<InputSchema>,
}

fn parent_scope_is_empty(v: &ParentScopeSchema) -> bool {
    v.0.is_empty()
}

fn backend_selection_is_empty(v: &BackendSelectionSchema) -> bool {
    v.0.is_empty()
}

fn invocation_controls_is_empty(v: &InvocationControlsSchema) -> bool {
    v.0.is_empty()
}
