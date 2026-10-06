use thiserror::Error;

#[derive(Debug, Error)]
pub enum ViewPlanError {
    #[error("view `{view}` requires identity/scope field `{field}` (declared under views.scope)")]
    ScopeFieldRequired { view: String, field: String },
    #[error("view scope missing `{param}`")]
    ScopeMissing { param: String },
    #[error("view identity field expected scalar")]
    IdentityScalarRequired,
    #[error("view output missing identity field `{field}`")]
    OutputIdentityMissing { field: String },
    #[error("view Get node missing binding `{field}` for entity `{entity}`")]
    GetBindingMissing { entity: String, field: String },
    #[error("view node `{node}` expected exactly one entity (got {count})")]
    SingleRowCount { node: String, count: usize },
    #[error("view node `{node}` missing row")]
    NodeRowMissing { node: String },
    #[error("unknown composed view `{view}`")]
    UnknownView { view: String },
    #[error("view `{view}` targets entity `{expected}`, requested `{actual}`")]
    TargetMismatch {
        view: String,
        expected: String,
        actual: String,
    },
    #[error("view `{view}` requires a query predicate supplying scope parameters")]
    ScopePredicateRequired { view: String },
    #[error("view node `{node}`: unsupported capability kind {kind:?}")]
    UnsupportedNodeKind {
        node: String,
        kind: plasm_core::CapabilityKind,
    },
}
