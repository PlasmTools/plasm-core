//! Structurally disjoint capability-input lanes (execution, scope, selection, controls,
//! arguments, payload).
//!
//! Nested under [`crate::schema`] so lane types can reference [`super::InputSchema`] /
//! [`super::InputFieldSchema`] without a sibling-module cycle.

use super::{InputFieldSchema, InputSchema};
use crate::identity::{EntityFieldName, EntityName};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Execution-only inputs supplied from an explicitly named singleton context row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRequirement {
    /// Entity type required for the context binding.
    pub entity: EntityName,
    /// CML variable / capability slot → field on the context row.
    pub bindings: IndexMap<String, EntityFieldName>,
}

/// Capability execution requirements. Context is never inferred from predicates or scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityExecutionSchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextRequirement>,
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CapabilityInputs {
    #[serde(default, skip_serializing_if = "capability_execution_is_empty")]
    pub execution: CapabilityExecutionSchema,
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

fn capability_execution_is_empty(v: &CapabilityExecutionSchema) -> bool {
    v.context.is_none()
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
