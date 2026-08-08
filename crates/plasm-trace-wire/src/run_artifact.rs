//! Stable archive identity for run snapshots (MCP `resources/read`, HTTP run artifacts).
//! Aligns with `RunArtifactDocument` (`run_id`, `prompt_hash`, `session_id`, optional
//! `resource_index`) for durable storage and future web deep-links.
//!
//! `run_id` is the on-the-wire prefixed-hex digest (not a UUID).
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunArtifactArchiveRef {
    pub prompt_hash: String,
    pub session_id: String,
    pub run_id: String,
    /// Present when the client read via short `plasm://session/.../r/{n}` (monotonic index).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_index: Option<u64>,
}
