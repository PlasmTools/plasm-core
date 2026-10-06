//! Semantic failures while opening or extending execute-session teaching state.

#[derive(Debug, thiserror::Error)]
pub enum SessionMutateError {
    #[error(transparent)]
    Persist(
        #[from] crate::mcp_transport_store::execute_session_registry::ExecuteSessionPersistError,
    ),
    #[error(transparent)]
    Teaching(#[from] plasm_core::prompt_render::python::PythonTeachingError),
    #[error(transparent)]
    Catalog(#[from] crate::catalog_runtime::CatalogRuntimeError),
    #[error(transparent)]
    CapabilitySurface(#[from] plasm_core::capability_exposure::CapabilityExposureError),
    #[error(transparent)]
    Materialize(#[from] crate::execute_session_materialize::MaterializeError),
    #[error(transparent)]
    Discovery(#[from] plasm_core::discovery::DiscoveryError),
    #[error(transparent)]
    SeedResolution(#[from] crate::http_execute::context::seed_resolve::SeedResolutionError),
    #[error(transparent)]
    Rehydrate(#[from] crate::execute_session_rehydrate::RehydrateError),
    #[error(transparent)]
    HttpBackend(#[from] crate::http_backend::ReplHttpOverrideError),
    #[error(transparent)]
    Binding(#[from] crate::binding_store::BindingLoadError),
    #[error(transparent)]
    Auth(#[from] plasm_runtime::AuthResolutionError),
    #[error("execute session has no entities")]
    EmptyEntities,
    #[error("execute session expansion has no seeds")]
    EmptySeeds,
    #[error("unknown entity `{entity}` in catalog `{entry_id}`")]
    UnknownEntity { entry_id: String, entity: String },
    #[error(
        "unknown entity `{entity}` in catalog `{entry_id}`; nearest entity names: {nearest:?}"
    )]
    UnknownSeedEntity {
        entry_id: String,
        entity: String,
        nearest: Vec<String>,
    },
    #[error("unknown catalog entry `{entry_id}` in the loaded session")]
    UnknownCatalogEntry { entry_id: String },
    #[error("execute session is unknown or expired")]
    UnknownOrExpiredSession,
    #[error("execute session already includes catalog entry `{entry_id}`")]
    CatalogAlreadyIncluded { entry_id: String },
    #[error("execute session has no incremental exposure state")]
    MissingExposureState,
    #[error("execute session tenant does not match the caller")]
    TenantMismatch,
    #[error("routed execute binding is unavailable")]
    RoutedBindingUnavailable,
    #[error("seed group for catalog `{entry_id}` is missing after grouping")]
    MissingSeedGroup { entry_id: String },
    #[error("routing selected no capabilities for a new context")]
    EmptyRoutedCapabilityPlan,
    #[error("routed execution session expired before it could be reused")]
    RoutedSessionExpired,
    #[error("routed execution session has no teaching exposure")]
    RoutedTeachingExposureMissing,
    #[error("capability exposure plan has no entries")]
    EmptyCapabilityExposurePlan,
    #[error("primary catalog `{entry_id}` has no entity seeds")]
    MissingPrimaryEntities { entry_id: String },
    #[error("prompt hash is invalid")]
    InvalidPromptHash,
    #[error("execute session id is invalid")]
    InvalidSessionId,
}
