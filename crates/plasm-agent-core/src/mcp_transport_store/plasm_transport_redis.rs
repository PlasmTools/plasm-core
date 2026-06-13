//! Redis mirror of per-transport Plasm logical session slot maps.

use std::sync::Arc;

use super::redis_backend::RedisBackend;
use super::types::PersistedPlasmTransportState;

const PLASM_TRANSPORT_KEY_PREFIX: &str = "mcp:plasm:transport:";

/// Optional Redis backing for MCP transport slot maps keyed by MCP transport session id.
#[derive(Clone)]
pub struct PlasmTransportRedisStore {
    backend: Arc<RedisBackend>,
}

impl PlasmTransportRedisStore {
    pub fn new(backend: Arc<RedisBackend>) -> Self {
        Self { backend }
    }

    fn key(session_id: &str) -> String {
        format!("{PLASM_TRANSPORT_KEY_PREFIX}{session_id}")
    }

    pub async fn load(&self, session_id: &str) -> Option<PersistedPlasmTransportState> {
        self.backend.get_json(&Self::key(session_id)).await
    }

    pub async fn save_snapshot(&self, session_id: &str, snapshot: &PersistedPlasmTransportState) {
        self.backend
            .set_json(&Self::key(session_id), snapshot)
            .await;
    }

    pub async fn touch(&self, session_id: &str) {
        self.backend.touch(&Self::key(session_id)).await;
    }
}
