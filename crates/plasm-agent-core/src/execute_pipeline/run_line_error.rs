//! Shared execute-line error surface for HTTP/MCP ingress and graph branch paths.

use plasm_runtime::RuntimeError;

use crate::execute_session::GraphEpoch;

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
