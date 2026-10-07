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

/// Admission class for a named Python query/search source-call argument.
/// Scope and selection bind the source; controls have a separate invocation lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuerySourceInputLane {
    Scope,
    Selection,
    Control,
}

/// Canonical, structurally disjoint capability-input algebra.
///
/// Built in Rust from [`crate::loader::DomainCapability`] (not deserialized from
/// `domain.yaml`). Catalog YAML rejects unknown keys such as abolished `execution:`
/// on [`crate::loader::DomainCapability`]'s `deny_unknown_fields`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CapabilityInputs {
    /// Cross-field source contracts, checked before a query/search is dispatched.
    #[serde(default, skip_serializing_if = "source_validation_is_empty")]
    pub input_validation: super::InputValidation,
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

fn source_validation_is_empty(v: &super::InputValidation) -> bool {
    !v.allow_null && v.cross_field_rules.is_empty()
}

impl CapabilityInputs {
    /// Exactly the named inputs that a Python query/search source call can accept.
    pub fn query_source_fields(&self) -> impl Iterator<Item = &InputFieldSchema> {
        self.scope.0.iter().chain(&self.selection.0)
    }

    /// Classify against the same typed lanes used to render source-call signatures.
    pub fn query_source_lane(&self, name: &str) -> Option<QuerySourceInputLane> {
        if self.scope.0.iter().any(|field| field.name == name) {
            Some(QuerySourceInputLane::Scope)
        } else if self.selection.0.iter().any(|field| field.name == name) {
            Some(QuerySourceInputLane::Selection)
        } else if self.controls.0.iter().any(|field| field.name == name) {
            Some(QuerySourceInputLane::Control)
        } else {
            None
        }
    }
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
