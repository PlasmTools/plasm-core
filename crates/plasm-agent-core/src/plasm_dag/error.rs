//! One error algebra for the mutually recursive DAG compilation passes.
//!
//! Passes propagate this type directly. Sources belong to lower-level parsers,
//! contracts, and schema validators; no variant wraps a compiler pass envelope.
//!
//! Dependency direction: admission envelopes -> this algebra -> leaf errors.
//! Do not add admission or pass-specific envelopes as sources here: recursive
//! passes share this type precisely to avoid error-type backedges.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DagCompilationError {
    #[error(transparent)]
    MembershipRhs(#[from] plasm_core::RowMembershipParseError),
    #[error(transparent)]
    BooleanFilter(#[from] plasm_core::BooleanFilterError),
    #[error(transparent)]
    PassthroughSchema(#[from] super::schema_validate::PassthroughSchemaError),
    #[error(transparent)]
    RowSuffix(#[from] super::RowSuffixLoweringError),
    #[error(transparent)]
    CollectMeta(#[from] plasm_core::expr_parser::CollectMetaError),
    #[error(transparent)]
    ExprNode(#[from] plasm_core::expr_parser::ExprNodeParseError),
    #[error(transparent)]
    SurfaceParse(#[from] crate::plasm_plan_run::ProgramSurfaceParseError),
    #[error(transparent)]
    TemplateSyntax(#[from] plasm_core::program_string_template::TemplateSyntaxError),
    #[error(transparent)]
    SurfaceGuard(#[from] crate::plasm_dag_surface_guards::SurfaceGuardError),
    #[error(transparent)]
    ViewEmbed(#[from] super::view_embed_proof::ViewEmbedProofError),
    #[error(transparent)]
    Prerequisite(#[from] plasm_core::prerequisites::PrerequisiteError),
    #[error(transparent)]
    Pipe(#[from] plasm_core::expr_parser::PipeParseError),
    #[error(transparent)]
    Iteration(#[from] plasm_core::expr_parser::IterateUntilError),
    #[error(transparent)]
    PlanValue(#[from] super::plan_serialize::PlanValueExpressionError),
    #[error(transparent)]
    Atom(#[from] plasm_core::plasm_monad::PlanAtomError),
    #[error(transparent)]
    PlanValidation(#[from] crate::plasm_plan::PlanValidationError),
    #[error(transparent)]
    CatalogOwnership(#[from] crate::catalog_ownership::CatalogOwnershipError),
    #[error(transparent)]
    Relation(#[from] super::relation::RelationLoweringError),
    #[error(transparent)]
    SchemaPath(#[from] super::schema_validate::SchemaPathValidationError),
    #[error(transparent)]
    SchemaCatalog(#[from] super::schema_validate::SchemaCatalogError),
    #[error(transparent)]
    SurfaceSyntax(#[from] plasm_core::expr_parser::SurfaceSyntaxError),
    #[error(transparent)]
    RenderColumns(#[from] crate::plasm_plan_run::RenderColumnsError),
    #[error(transparent)]
    TemplateNames(#[from] crate::plasm_render_compile::TemplateNameClassificationError),
    #[error(transparent)]
    FixtureSerialization(#[from] serde_json::Error),
    #[error(transparent)]
    RowPredicate(#[from] plasm_core::RowPredicateError),
    #[error(transparent)]
    RowPredicateLowering(#[from] crate::row_predicate_lower::RowPredicateLoweringError),
    #[error(transparent)]
    Type(#[from] plasm_core::TypeError),
    #[error(transparent)]
    BindingInvariant(#[from] crate::program_rejection::PythonLoweringInvariantError),
    #[error(transparent)]
    RenderColumnInference(#[from] super::schema_validate::RenderColumnInferenceError),
    #[error("row suffix stream received no suffixes")]
    EmptyStream,
    #[error("membership RHS `{inner}` has an invalid expression: {source}")]
    MembershipExpression {
        inner: String,
        #[source]
        source: plasm_core::expr_parser::ExprNodeParseError,
    },
    #[error("membership RHS pipeline cannot have a row applicator")]
    MembershipApplicator,
    #[error("union RHS `{inner}` has an invalid expression: {source}")]
    UnionExpression {
        inner: String,
        #[source]
        source: plasm_core::expr_parser::ExprNodeParseError,
    },
    #[error("union RHS pipeline cannot have a row applicator")]
    UnionApplicator,
    #[error("unknown Plasm program binding `{binding}`")]
    UnknownBinding { binding: String },
    #[error("program final roots list is empty")]
    EmptyRoots,
    #[error("program is empty")]
    EmptyProgram,
    #[error("invalid program binding label `{label}`")]
    InvalidBindingLabel { label: String },
    #[error("Remove `return` — write bare roots on the last line")]
    ReturnKeyword,
    #[error("program has no final roots")]
    MissingRoots,
    #[error("binding `{binding}` does not retain entity identity for a method receiver")]
    ReceiverIdentityMissing { binding: String },
    #[error("receiver catalog is unavailable for `{binding}`")]
    ReceiverCatalogMissing { binding: String },
    #[error("receiver entity is unavailable for `{binding}`")]
    ReceiverEntityMissing { binding: String },
    #[error("catalog `{entry_id}` is not loaded for entity `{entity}`")]
    CatalogMissing { entry_id: String, entity: String },
    #[error("program `{id}`: bind relation rows before applying an explicit continuation")]
    ExplicitRelationApplication { id: String },
    #[error("program `{id}` row application must be a catalog read or operation expression")]
    InvalidRowApplication { id: String },
    #[error("program `{id}` iterate step must be a write/side-effect expression")]
    InvalidIterationStep { id: String },
    #[error("until predicate binding `{binding}` is plural; select exactly one row")]
    PluralUntilBinding { binding: String },
    #[error("program `{id}`: iterate cannot be staged as an apply left-hand; bind it separately")]
    IterationApplySource { id: String },
    #[error("program `{id}`: surface `{expression}` is multi-node ({actual} nodes); use compile_surface_nodes")]
    SurfaceNodeCount {
        id: String,
        expression: String,
        actual: usize,
    },
    #[error("PLP-4: program `{id}`: `{binding}` is not a Plasm expression anchor")]
    TerminalContinuation { id: String, binding: String },
    #[error("PLP-4: program `{id}`: binding `{binding}` cannot extend with `{tail}`")]
    UnsupportedContinuation {
        id: String,
        binding: String,
        tail: String,
    },
    #[error("program `{id}`: relation continuation `{binding}.{tail}` requires one segment")]
    RelationSegmentRequired {
        id: String,
        binding: String,
        tail: String,
    },
    #[error("program `{id}`: method receiver `{binding}` requires an identity-preserving singleton; apply plural receivers with => _.m#(args)")]
    MethodReceiverCardinality { id: String, binding: String },
    #[error("program `{id}`: relation continuation `{binding}` requires a singleton binding")]
    RelationReceiverCardinality { id: String, binding: String },
    #[error("PLP-4: program `{id}`: binding `{binding}` lacks entity continuation evidence")]
    RelationEvidenceMissing { id: String, binding: String },
    #[error("program `{id}`: `{binding}.{segment}` did not lower to a relation chain")]
    RelationChainRequired {
        id: String,
        binding: String,
        segment: String,
    },
    #[error("program `{id}`: `{segment}` is not a field or relation on `{binding}`")]
    RelationSegmentUnknown {
        id: String,
        binding: String,
        segment: String,
    },
    #[error("program `{id}`: relation wire mismatch for `{binding}.{segment}`")]
    RelationWireMismatch {
        id: String,
        binding: String,
        segment: String,
    },
    #[error("program `{id}`: binding `{binding}` has no continuation anchor for `{tail}`")]
    ContinuationAnchorMissing {
        id: String,
        binding: String,
        tail: String,
    },
    #[error("program `{id}`: continuation of `{binding}` expands to a non-relation expression")]
    NonRelationContinuation { id: String, binding: String },
    #[error("program `{id}`: unknown dotted row transform `{tail}`; use postfix row algebra")]
    UnknownRowTransform { id: String, tail: String },
    #[error("page_size requires a closing `)`")]
    PageSizeClosingDelimiter,
    #[error("page_size requires a positive integer, got `{value}`")]
    InvalidPageSize { value: String },
    #[error("collect-meta tails must chain as `.page_size(N).singleton()`")]
    InvalidCollectMetaChain,
    #[error("program `{id}`: collect-meta continuation `{expression}` produced no nodes")]
    EmptyContinuation { id: String, expression: String },
    #[error("program `{id}`: per-row render requires exactly one source; got {actual}")]
    RenderSourceCount { id: String, actual: usize },
    #[error("program `{id}`: render source `{binding}` is not in scope")]
    RenderSourceMissing { id: String, binding: String },
    #[error("program `{id}`: render chain produced no nodes")]
    EmptyRenderChain { id: String },
    #[error("program `{id}`: `{expression}` field-dot requires an identity-preserving singleton; use `select {wire}` to retain rows")]
    PluralFieldExtract {
        id: String,
        expression: String,
        wire: String,
    },
    #[error("PLP-1: `{entity}.{wire}` Get identity must be a literal or binding, got {actual}")]
    InvalidGetIdentity {
        entity: String,
        wire: String,
        actual: &'static str,
    },
    #[error("relation chains must be lowered before surface contract inference")]
    UnloweredRelationChain,
    #[error("page handle `{handle}` is not registered in this session")]
    UnknownPageHandle { handle: String },
    #[error("page continuation requires catalog ownership from session e# or binding")]
    PageOwnershipMissing,
    #[error("teaching values cannot appear in execution plans")]
    TeachingValueInPlan,
    #[error("wait/cancel are host continuations and cannot appear in plan surfaces")]
    HostContinuationInPlan,
    #[error("PLP-4: program `{id}`: param `{param}` expects a scalar cell, but `{binding}` does not denote one scalar cell")]
    NonScalarParameter {
        id: String,
        param: String,
        binding: String,
    },
    #[error("program `{id}`: param `{param}` expects a scalar, but `{binding}.{path}` is not an identity-preserving singleton field extract")]
    PluralParameterField {
        id: String,
        param: String,
        binding: String,
        path: String,
    },
}

#[cfg(test)]
mod tests {
    use super::DagCompilationError;
    use plasm_core::expr_parser::SurfaceSyntaxError;

    #[test]
    fn syntax_fault_keeps_its_semantic_variant() {
        let error = DagCompilationError::from(SurfaceSyntaxError::InvalidBindingLabel {
            label: "p1".into(),
        });
        assert!(matches!(
            error,
            DagCompilationError::SurfaceSyntax(SurfaceSyntaxError::InvalidBindingLabel { label })
                if label == "p1"
        ));
    }
}
