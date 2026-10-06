use thiserror::Error;

use crate::bootstrap_secrets::BootstrapSecretsError;
pub use crate::plasm_plan_run::SessionCatalogNotLoaded;

// Public source types carried by the host's semantic error envelopes. Keep the
// implementation modules private; callers can name and inspect causes here.
pub use crate::catalog_ownership::CatalogOwnershipError;
pub use crate::graph_rehydrate::GraphRehydrateError;
pub use crate::map_body::{
    CatalogOwnershipError as ScopeCatalogOwnershipError, MapBodyValidationError, MapSchemaError,
    PythonFieldError, ScopeContractError,
};
pub use crate::map_body_schema::{refinements::RefinementError, MapBodySchemaError};
pub use crate::plan_session_provisions::SessionProvisionError;
pub use crate::plan_surface_policy::SurfaceQualifiedEntityPolicyError;
pub use crate::plasm_comp_bundle::PlasmCompBundleError;
pub use crate::plasm_comp_lift::PlasmCompLiftError;
pub use crate::plasm_dag::row_suffix::RowSuffixLoweringError;
pub use crate::plasm_dag::schema_validate::catalog::RowContractFieldError;
pub use crate::plasm_dag::schema_validate::compute_schema::{
    PassthroughSchemaError, RenderColumnInferenceError,
};
pub use crate::plasm_dag::{
    error::DagCompilationError,
    plan_serialize::parse_helpers::{
        AggregateSpecError, GroupByError, PlanValueExpressionError, SortSpecError,
    },
    relation::RelationLoweringError,
    row_suffix::reductions::ReductionLoweringError,
    schema_validate::{catalog::SchemaCatalogError, path_validate::SchemaPathValidationError},
    view_embed_proof::ViewEmbedProofError,
};
pub use crate::plasm_dag_surface_guards::SurfaceGuardError;
pub use crate::plasm_plan::validate::{
    compute::{PlanDataInputError, PlanExpressionError},
    value::{ValueValidationError, ValueValidationKind},
};
pub use crate::plasm_render_compile::{RenderFieldListError, TemplateNameClassificationError};
pub use crate::plasm_step_convert::StepPayloadLiftError;
pub use crate::python_compute::ComputeInputBudgetError;
pub use crate::python_compute::{
    arguments::PythonArgumentError,
    inference::{DeclarationError, InferenceError, InferenceGraphError, ReturnContractError},
    schema::PythonSchemaError,
};
pub use crate::row_predicate_lower::RowPredicateLoweringError;

#[derive(Error, Debug)]
pub enum AgentError {
    #[error(transparent)]
    CatalogTemplate(#[from] plasm_compile::CatalogTemplateError),
    #[error(transparent)]
    SchemaLoad(#[from] plasm_core::loader::SchemaLoadError),

    #[error("--schema and --catalog-dir cannot be used together")]
    SchemaAndCatalogDir,

    #[error("catalog directory contains no catalog entries")]
    EmptyCatalogDirectory,

    #[error("server mode requires --schema <path> or --catalog-dir <dir>")]
    ServerCatalogInputRequired,

    #[error("non-server mode requires --schema <path>")]
    NonServerSchemaRequired,

    #[error(transparent)]
    CatalogLoad(#[from] crate::catalog_data::CatalogLoadError),

    #[error(transparent)]
    TemplateValidation(#[from] plasm_compile::CmlError),

    #[error(transparent)]
    HttpBackend(#[from] crate::http_backend::ReplHttpOverrideError),

    #[error("No entity '{0}' in schema")]
    EntityNotFound(String),

    #[error("No capability '{kind}' for entity '{entity}'")]
    CapabilityNotFound { entity: String, kind: String },

    #[error(transparent)]
    Argument(#[from] AgentArgumentError),

    #[error("Execution error: {0}")]
    Execution(#[from] plasm_runtime::RuntimeError),

    #[error("Compilation error: {0}")]
    Compilation(#[from] plasm_compile::CompileError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Bootstrap(#[from] BootstrapSecretsError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum AgentArgumentError {
    #[error("No entity command specified")]
    MissingEntityCommand,
    #[error("'{command}' requires an ID. Usage: {entity} <ID> {command}")]
    MissingNodeId { command: String, entity: String },
    #[error("Provide an ID or use 'query'. Usage: {entity} query [--filters] | {entity} <ID> [--key flags] | {entity} <ID> <relation>")]
    MissingEntityIdOrQuery { entity: String },
    #[error("Relation '{relation}' not found")]
    RelationNotFound { relation: String },
    #[error("Unknown materialize capability '{capability}' (relation '{relation}')")]
    MaterializeCapabilityNotFound {
        capability: String,
        relation: String,
    },
    #[error("Unknown target entity '{entity}'")]
    TargetEntityNotFound { entity: String },
    #[error("Missing required path flag --{flag} (CML path variable `{variable}`)")]
    MissingPathFlag { flag: String, variable: String },
    #[error("Entity `{entity}` uses compound key {key_vars:?}; a GET capability is required to resolve CLI path variables")]
    CompoundKeyRequiresGet {
        entity: String,
        key_vars: Vec<String>,
    },
    #[error("Entity `{entity}` uses compound key {key_vars:?}; derived Gets have no CML path template for CLI key binding")]
    CompoundKeyGetRequiresPathTemplate {
        entity: String,
        key_vars: Vec<String>,
    },
    #[error("Missing compound key part `{part}` for entity `{entity}` (use --{flag} or the positional id for the last URL segment, per SCHEMA `key_vars`)")]
    MissingCompoundKeyPart {
        part: String,
        entity: String,
        flag: String,
    },
    #[error("scoped get binding `{field}` is missing for entity `{entity}`")]
    MissingScopedGetBinding { field: String, entity: String },
    #[error("preflight CLI arguments are invalid")]
    PreflightCliArguments(#[source] clap::Error),
    #[error("catalog CLI arguments are invalid")]
    CatalogCliArguments(#[source] clap::Error),
    #[error("full CLI arguments are invalid")]
    FullCliArguments(#[source] clap::Error),
}
