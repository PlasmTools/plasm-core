//! Shared MCP transport / execute binding types (no handler dependency).

use std::collections::HashMap;

/// Per logical session: Plasm execute `prompt_hash` + `session` ids (same as HTTP paths).
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlasmExecBinding {
    pub prompt_hash: String,
    pub session_id: String,
}

/// Serializable slot-map snapshot for Redis (per MCP transport session id).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PersistedPlasmTransportState {
    pub logical_bindings: HashMap<String, PlasmExecBinding>,
}
