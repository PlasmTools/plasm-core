//! Environment-driven MCP transport store configuration.

use std::time::Duration;

pub const DEFAULT_TRANSPORT_TTL_SECS: u64 = 3600; // aligned with execute session in-memory TTL

/// Redis-backed MCP transport store settings (`PLASM_MCP_TRANSPORT_REDIS_URL`).
#[derive(Clone, Debug)]
pub struct McpTransportStoreConfig {
    pub redis_url: String,
    pub ttl: Duration,
}

impl McpTransportStoreConfig {
    pub fn from_env() -> Option<Self> {
        let redis_url = std::env::var("PLASM_MCP_TRANSPORT_REDIS_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())?;
        let ttl_secs = std::env::var("PLASM_MCP_TRANSPORT_REDIS_TTL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(crate::execute_session::session_ttl_secs);
        Some(Self {
            redis_url,
            ttl: Duration::from_secs(ttl_secs.max(60)),
        })
    }
}
