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
    SchemaLoad(#[from] Box<plasm_core::loader::SchemaLoadError>),

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
    Execution(#[from] Box<plasm_runtime::RuntimeError>),

    #[error("Compilation error: {0}")]
    Compilation(#[from] plasm_compile::CompileError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Bootstrap(#[from] BootstrapSecretsError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<plasm_core::loader::SchemaLoadError> for AgentError {
    fn from(error: plasm_core::loader::SchemaLoadError) -> Self {
        Self::SchemaLoad(Box::new(error))
    }
}

impl From<plasm_runtime::RuntimeError> for AgentError {
    fn from(error: plasm_runtime::RuntimeError) -> Self {
        Self::Execution(Box::new(error))
    }
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
    #[error("Entity `{entity}` uses compound key {key_vars}; a GET capability is required to resolve CLI path variables", key_vars = .key_vars.join(", "))]
    CompoundKeyRequiresGet {
        entity: String,
        key_vars: Vec<String>,
    },
    #[error("Entity `{entity}` uses compound key {key_vars}; derived Gets have no CML path template for CLI key binding", key_vars = .key_vars.join(", "))]
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

#[cfg(test)]
mod footprint_tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn catalog_and_discovery_error_owners_have_bounded_footprints() {
        assert!(std::mem::size_of::<AgentError>() < 128);
        assert!(std::mem::size_of::<plasm_core::expr_parser::ParseError>() < 128);
        assert!(std::mem::size_of::<crate::compilation_error::CompilationError>() < 128);
        assert!(std::mem::size_of::<crate::plasm_render_compile::RenderFieldListError>() < 128);
        assert!(std::mem::size_of::<crate::map_body::MapBodyValidationError>() < 128);
        assert!(std::mem::size_of::<crate::plan_session_provisions::SessionProvisionError>() < 128);
        assert!(std::mem::size_of::<crate::catalog_data::CatalogLoadError>() < 128);
        assert!(std::mem::size_of::<crate::catalog_runtime::CatalogRuntimeError>() < 128);
        assert!(std::mem::size_of::<crate::discovery_store::DiscoveryStoreError>() < 128);
        assert!(std::mem::size_of::<crate::discovery_support::DiscoverySupportError>() < 128);
        assert!(std::mem::size_of::<crate::discovery_service::DiscoveryServiceError>() < 128);
        assert!(std::mem::size_of::<crate::execute_pipeline::DispatchError>() < 128);
    }

    #[test]
    fn boxed_catalog_imports_preserve_schema_source_chain() {
        let source = || {
            plasm_core::catalog_il::CatalogIlError::from(plasm_core::SchemaError::DuplicateEntity {
                name: "FixtureEntity".into(),
            })
        };
        let errors: [Box<dyn Error>; 3] = [
            Box::new(crate::catalog_data::CatalogLoadError::from(source())),
            Box::new(crate::catalog_runtime::CatalogRuntimeError::from(source())),
            Box::new(crate::discovery_store::DiscoveryStoreError::from(source())),
        ];
        for error in errors {
            assert_eq!(error.to_string(), "catalog schema validation failed");
            assert!(matches!(
                error.source().unwrap().downcast_ref::<plasm_core::SchemaError>(),
                Some(plasm_core::SchemaError::DuplicateEntity { name })
                    if name == "FixtureEntity"
            ));
        }
    }

    #[test]
    fn boxed_agent_imports_preserve_concrete_sources() {
        let schema = AgentError::from(plasm_core::loader::SchemaLoadError::from(
            plasm_core::SchemaError::DuplicateEntity {
                name: "FixtureEntity".into(),
            },
        ));
        assert!(matches!(
            schema.source().unwrap().downcast_ref::<plasm_core::SchemaError>(),
            Some(plasm_core::SchemaError::DuplicateEntity { name }) if name == "FixtureEntity"
        ));
        let runtime = AgentError::from(plasm_runtime::RuntimeError::Cancelled);
        assert!(matches!(
            runtime
                .source()
                .unwrap()
                .downcast_ref::<Box<plasm_runtime::RuntimeError>>()
                .map(Box::as_ref),
            Some(plasm_runtime::RuntimeError::Cancelled)
        ));
    }

    #[test]
    fn boxed_render_token_preserves_metadata_and_concrete_source() {
        let error = RenderFieldListError::TokenResolution {
            token: "binding.field".into(),
            source: Box::new(
                crate::plasm_plan_run::WireFieldTokenError::MissingBindingContext {
                    token: "binding.field".into(),
                },
            ),
        };
        assert!(matches!(
            error
                .source()
                .unwrap()
                .downcast_ref::<Box<crate::plasm_plan_run::WireFieldTokenError>>()
                .map(Box::as_ref),
            Some(crate::plasm_plan_run::WireFieldTokenError::MissingBindingContext { token })
                if token == "binding.field"
        ));
    }
}
