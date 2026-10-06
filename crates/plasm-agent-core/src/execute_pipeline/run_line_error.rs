//! Shared execute-line error surface for HTTP/MCP ingress and graph branch paths.

use plasm_runtime::{RuntimeError, WriteConflictDetails};

/// Stable user-facing copy when optimistic materialization commit loses a concurrent race.
pub const GRAPH_WRITE_CONFLICT_USER_MESSAGE: &str =
    "session materialization changed during concurrent execute; retry the request";

#[must_use]
pub fn graph_write_conflict_user_message() -> String {
    GRAPH_WRITE_CONFLICT_USER_MESSAGE.to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum RunLineError {
    #[error("{correction}")]
    Parse {
        #[source]
        source: Box<plasm_core::expr_parser::ParseError>,
        /// Already-rendered ingress correction; never substitutes for the cause.
        correction: String,
    },
    #[error("{0}")]
    Admission(#[source] Box<crate::program_diagnostic::ProgramStageError>),
    #[error("{0}")]
    WireField(#[source] crate::plasm_plan_run::WireFieldTokenError),
    #[error("page expression has no pagination snapshot")]
    MissingPageResume,
    #[error(transparent)]
    CatalogOwnership(crate::catalog_ownership::CatalogOwnershipError),
    #[error("{0}")]
    PageHandle(crate::http_execute::PagingHandleFault),
    #[error(transparent)]
    OperationFailed(crate::operation_error::OperationError),
    #[error("{0}")]
    Normalize(#[source] plasm_core::QueryCapabilityResolveError),
    /// [`ExecutionEngine::auto_resolve_projection`] failed; surface to clients instead of silent degradation.
    #[error("{0}")]
    Projection(#[source] Box<RuntimeError>),
    /// Runtime failure after successful admission. Authored source is not diagnostic payload.
    #[error(transparent)]
    Runtime(Box<RuntimeError>),
    #[error(transparent)]
    ArtifactSerialization(serde_json::Error),
    /// Durable run snapshot write failed (object store / memory backend).
    #[error("{0}")]
    ArtifactPersist(#[source] crate::run_artifacts::RunArtifactError),
    #[error("{0}")]
    ArtifactDigest(#[source] plasm_evidence::CanonicalError),
    #[error("{0}")]
    ArtifactRehydration(#[source] crate::error::GraphRehydrateError),
    /// Optimistic branch commit lost a per-store write race after bounded retries.
    #[error("session materialization changed during concurrent execute")]
    GraphWriteConflict {
        details: WriteConflictDetails,
        attempts: u32,
    },
    /// Async operation continuation (`wait` / `cancel`) — success payload via `Err` channel for unified ingress.
    #[error("operation continuation")]
    Operation(Box<crate::plasm_plan_run::PlasmPlanRunResult>),
}

impl From<RuntimeError> for RunLineError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(Box::new(error))
    }
}

impl From<crate::program_diagnostic::ProgramStageError> for RunLineError {
    fn from(error: crate::program_diagnostic::ProgramStageError) -> Self {
        Self::Admission(Box::new(error))
    }
}

impl From<RunLineError> for plasm_runtime::ExecutionFailure {
    fn from(error: RunLineError) -> Self {
        use plasm_runtime::{ExecutionFailure, FailureCause};
        match error {
            RunLineError::PageHandle(fault) => fault.into(),
            RunLineError::CatalogOwnership(error) => ExecutionFailure::new(
                FailureCause::Program,
                "catalog_ownership_failed",
                error.to_string(),
            ),
            RunLineError::OperationFailed(error) => error.into(),
            RunLineError::Runtime(error) => ExecutionFailure::from(*error),
            RunLineError::Parse { correction, .. } => {
                ExecutionFailure::new(FailureCause::Program, "program_admission", correction)
            }
            RunLineError::Admission(error) => (*error).into(),
            RunLineError::Normalize(error) => ExecutionFailure::new(
                FailureCause::Program,
                "program_admission",
                error.to_string(),
            ),
            RunLineError::WireField(error) => ExecutionFailure::new(
                FailureCause::Program,
                "program_admission",
                error.to_string(),
            ),
            RunLineError::MissingPageResume => ExecutionFailure::new(
                FailureCause::Runtime,
                "page_resume_missing",
                "page expression has no pagination snapshot",
            ),
            RunLineError::ArtifactDigest(error) => ExecutionFailure::new(
                FailureCause::Program,
                "program_admission",
                error.to_string(),
            ),
            RunLineError::Projection(detail) => ExecutionFailure::new(
                FailureCause::Program,
                "projection_failed",
                detail.to_string(),
            ),
            RunLineError::ArtifactPersist(detail) => ExecutionFailure::new(
                FailureCause::Runtime,
                "artifact_persist_failed",
                detail.to_string(),
            ),
            RunLineError::ArtifactRehydration(error) => ExecutionFailure::new(
                FailureCause::Runtime,
                "artifact_persist_failed",
                error.to_string(),
            ),
            RunLineError::ArtifactSerialization(error) => ExecutionFailure::new(
                FailureCause::Runtime,
                "artifact_serialization_failed",
                error.to_string(),
            ),
            RunLineError::GraphWriteConflict { .. } => ExecutionFailure::new(
                FailureCause::Runtime,
                "graph_write_conflict",
                "concurrent graph conflict",
            ),
            RunLineError::Operation(_) => ExecutionFailure::new(
                FailureCause::Runtime,
                "invalid_operation_continuation",
                "invalid continuation",
            ),
        }
    }
}

/// Presentation only; execution paths must retain the typed failure.
pub fn display_run_line_error(error: RunLineError) -> String {
    plasm_runtime::ExecutionFailure::from(error).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::PagingHandle;
    use plasm_runtime::{FailureCause, RecoveryDisposition};

    #[test]
    fn boxed_parse_keeps_source_and_bounds_error_footprint() {
        use std::error::Error;
        let bytes = std::mem::size_of::<RunLineError>();
        assert!(bytes < 128, "RunLineError is {bytes} bytes; expected <128");
        let error = RunLineError::Parse {
            source: Box::new(plasm_core::expr_parser::ParseError {
                kind: plasm_core::expr_parser::ParseErrorKind::ExpectedIdentifier,
                offset: 7,
            }),
            correction: "expected identifier".into(),
        };
        let source = error.source().unwrap();
        assert_eq!(
            source
                .downcast_ref::<Box<plasm_core::expr_parser::ParseError>>()
                .unwrap()
                .as_ref()
                .offset,
            7
        );
        assert_eq!(error.to_string(), "expected identifier");
    }

    #[test]
    fn normalization_keeps_query_resolution_cause() {
        use std::error::Error;
        let error = RunLineError::Normalize(
            plasm_core::QueryCapabilityResolveError::CapabilityNotFound {
                capability: "missing".into(),
                entity: "item".into(),
            },
        );
        assert!(matches!(
                error.source().and_then(
                    |source| source.downcast_ref::<plasm_core::QueryCapabilityResolveError>()
                ),
                Some(plasm_core::QueryCapabilityResolveError::CapabilityNotFound { .. })
            ));
    }

    #[test]
    fn nested_boxed_parse_keeps_symbol_cause() {
        use plasm_core::expr_parser::{ParseError, ParseErrorKind};
        use plasm_core::SymbolResolveError;
        use std::error::Error;
        let error = RunLineError::Parse {
            source: Box::new(ParseError {
                offset: 7,
                kind: ParseErrorKind::SymbolResolution {
                    source: Box::new(SymbolResolveError::UnknownEntityPSym {
                        catalog_entry_id: "fixture".into(),
                        entity: "Item".into(),
                        token: "p1".into(),
                    }),
                },
            }),
            correction: "symbol rejection".into(),
        };
        let parse = error
            .source()
            .unwrap()
            .downcast_ref::<Box<ParseError>>()
            .unwrap()
            .as_ref();
        assert_eq!(parse.offset, 7);
        let symbol = parse
            .source()
            .unwrap()
            .downcast_ref::<Box<SymbolResolveError>>()
            .unwrap()
            .as_ref();
        assert!(matches!(symbol,
            SymbolResolveError::UnknownEntityPSym { catalog_entry_id, entity, token }
                if catalog_entry_id == "fixture" && entity == "Item" && token == "p1"
        ));
        assert_eq!(error.to_string(), "symbol rejection");
    }

    #[test]
    fn artifact_persistence_keeps_filesystem_cause() {
        use std::error::Error;
        let error =
            RunLineError::ArtifactPersist(crate::run_artifacts::RunArtifactError::Filesystem(
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ));
        let source = error.source().expect("artifact source");
        assert!(matches!(
            source.downcast_ref::<crate::run_artifacts::RunArtifactError>(),
            Some(crate::run_artifacts::RunArtifactError::Filesystem(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn stale_page_handle_stays_typed_through_http_error_boundary() {
        let handle = PagingHandle::parse("pg1").unwrap();
        let failure: plasm_runtime::ExecutionFailure =
            RunLineError::PageHandle(crate::http_execute::PagingHandleFault::Unavailable {
                handle,
            })
            .into();
        assert_eq!(failure.cause, FailureCause::Program);
        assert_eq!(failure.code, "page_handle_unavailable");
        assert_eq!(failure.recovery, RecoveryDisposition::RepairProgram);
        assert!(!failure.effects_unresolved);
        assert!(failure.effects.is_empty());
        assert!(failure.dispatches.is_empty());
    }
}
