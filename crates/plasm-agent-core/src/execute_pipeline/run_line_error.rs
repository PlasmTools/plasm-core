//! Shared execute-line error surface for HTTP/MCP ingress and graph branch paths.

use plasm_runtime::{RuntimeError, WriteConflictDetails};

/// Stable user-facing copy when optimistic materialization commit loses a concurrent race.
pub const GRAPH_WRITE_CONFLICT_USER_MESSAGE: &str =
    "session materialization changed during concurrent execute; retry the request";

#[must_use]
pub fn graph_write_conflict_user_message() -> String {
    GRAPH_WRITE_CONFLICT_USER_MESSAGE.to_string()
}

#[derive(Debug)]
pub enum RunLineError {
    Parse(String),
    OperationFailed(crate::operation_error::OperationError),
    Normalize(String),
    /// [`ExecutionEngine::auto_resolve_projection`] failed; surface to clients instead of silent degradation.
    Projection(String),
    /// Runtime failure after successful admission. Authored source is not diagnostic payload.
    Runtime(RuntimeError),
    ArtifactSerialization(serde_json::Error),
    /// Durable run snapshot write failed (object store / memory backend).
    ArtifactPersist(String),
    /// Optimistic branch commit lost a per-store write race after bounded retries.
    GraphWriteConflict {
        details: WriteConflictDetails,
        attempts: u32,
    },
    /// Async operation continuation (`wait` / `cancel`) — success payload via `Err` channel for unified ingress.
    Operation(Box<crate::plasm_plan_run::PlasmPlanRunResult>),
}

impl From<RunLineError> for plasm_runtime::ExecutionFailure {
    fn from(error: RunLineError) -> Self {
        use plasm_runtime::{ExecutionFailure, FailureCause};
        match error {
            RunLineError::OperationFailed(error) => error.into(),
            RunLineError::Runtime(error) => ExecutionFailure::from(error),
            RunLineError::Parse(detail) | RunLineError::Normalize(detail) => {
                ExecutionFailure::new(FailureCause::Program, "program_admission", detail)
            }
            RunLineError::Projection(detail) | RunLineError::ArtifactPersist(detail) => {
                ExecutionFailure::from(detail)
            }
            RunLineError::ArtifactSerialization(error) => ExecutionFailure::from(error.to_string()),
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
