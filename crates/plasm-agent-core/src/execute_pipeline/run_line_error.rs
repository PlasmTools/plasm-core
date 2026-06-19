//! Shared execute-line error surface for HTTP/MCP ingress and graph branch paths.

use plasm_runtime::RuntimeError;

use crate::execute_session::GraphEpoch;

/// Stable user-facing copy when optimistic graph commit loses the epoch race.
pub const STALE_GRAPH_EPOCH_USER_MESSAGE: &str =
    "session graph changed during concurrent execute; retry the request";

#[must_use]
pub fn stale_graph_epoch_user_message() -> String {
    STALE_GRAPH_EPOCH_USER_MESSAGE.to_string()
}

#[derive(Debug)]
pub enum RunLineError {
    Parse(String),
    Normalize(String),
    /// [`ExecutionEngine::auto_resolve_projection`] failed; surface to clients instead of silent degradation.
    Projection(String),
    /// Runtime failure after successful parse; second field is the **source** line for logging and MCP.
    Runtime(RuntimeError, String),
    ArtifactSerialization(serde_json::Error),
    /// Durable run snapshot write failed (object store / memory backend).
    ArtifactPersist(String),
    /// Optimistic graph commit lost the epoch race after bounded retries.
    StaleGraphEpoch {
        expected: GraphEpoch,
        found: GraphEpoch,
        attempts: u32,
    },
    /// Async operation continuation (`wait` / `cancel`) — success payload via `Err` channel for unified ingress.
    Operation(Box<crate::plasm_plan_run::PlasmPlanRunResult>),
}

/// User-facing message for HTTP/MCP/plan fan-out execute paths.
#[must_use]
pub fn display_run_line_error(e: RunLineError) -> String {
    match e {
        RunLineError::Parse(d) | RunLineError::Normalize(d) | RunLineError::Projection(d) => d,
        RunLineError::Runtime(err, src) => format!("{err}\nsource expression: {src}"),
        RunLineError::ArtifactSerialization(err) => {
            format!("artifact serialization failed: {err}")
        }
        RunLineError::ArtifactPersist(d) => format!("run artifact persist failed: {d}"),
        RunLineError::StaleGraphEpoch { .. } => stale_graph_epoch_user_message(),
        RunLineError::Operation(_) => {
            "operation continuation is not valid inside a plan surface node".to_string()
        }
    }
}
