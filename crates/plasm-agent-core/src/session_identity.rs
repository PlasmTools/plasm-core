//! Agent-scoped logical session identity: stable host **`intent`** (MCP `plasm_context` JSON string) +
//! server-minted `LogicalSessionId`.
//!
//! ## Roles vs other MCP session state
//!
//! - **`LogicalSessionRegistry` (this module)** — sole **mint** for `LogicalSessionId` and
//!   **idempotent** lookup by `(tenant_scope, intent string)`; [`LogicalSessionRegistry::verify_tenant`]
//!   gates tool use. With `PLASM_MCP_TRANSPORT_REDIS_URL`, records mirror to Redis for multi-replica.
//! - **`PlasmHostState::logical_execute_bindings`** ([`crate::server_state::PlasmHostState`]) —
//!   host-wide **latest** `(prompt_hash, execute_session_id)` per logical id for **`resources/read`**
//!   and reconnect **hydration** without relying on this connection’s RAM.
//! - **`McpTransportState::logical_by_id`** ([`crate::mcp_server`]) — **per MCP transport**
//!   (`MCP-Session-Id`) cache for binding + stats + `_meta.plasm` index; **not** the minting authority.
//!   Logical session continuity uses stateless **`l_<token>`** wire refs ([`crate::mcp_logical_ref`]), not transport slots.

use std::collections::HashMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::mcp_transport_store::RedisBackend;

const SESSION_KEY_PREFIX: &str = "mcp:logical:session:";
const CLIENT_KEY_PREFIX: &str = "mcp:logical:client:";

/// Opaque client-supplied key (e.g. per-agent incrementing index), UTF-8 string.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ClientSessionKey(pub String);

impl ClientSessionKey {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Server-minted UUID identifying one Plasm logical session (prompt + execute + trace root).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct LogicalSessionId(pub Uuid);

impl LogicalSessionId {
    pub fn new_v4() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn parse(s: &str) -> Result<Self, uuid::Error> {
        Ok(Self(Uuid::parse_str(s)?))
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl std::fmt::Display for LogicalSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug)]
pub struct LogicalSessionRecord {
    pub logical_session_id: LogicalSessionId,
    /// Host-stable string from MCP `plasm_context` **`intent`** (same role as before the wire rename).
    pub intent: ClientSessionKey,
    pub tenant_scope: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedLogicalSession {
    logical_session_id: Uuid,
    intent: String,
    tenant_scope: String,
}

impl From<&LogicalSessionRecord> for PersistedLogicalSession {
    fn from(rec: &LogicalSessionRecord) -> Self {
        Self {
            logical_session_id: rec.logical_session_id.0,
            intent: rec.intent.as_str().to_string(),
            tenant_scope: rec.tenant_scope.clone(),
        }
    }
}

impl From<PersistedLogicalSession> for LogicalSessionRecord {
    fn from(p: PersistedLogicalSession) -> Self {
        Self {
            logical_session_id: LogicalSessionId(p.logical_session_id),
            intent: ClientSessionKey::new(p.intent),
            tenant_scope: p.tenant_scope,
        }
    }
}

fn session_key(id: &Uuid) -> String {
    format!("{SESSION_KEY_PREFIX}{id}")
}

fn client_index_key(tenant_scope: &str, intent: &str) -> String {
    let mut h = Sha256::new();
    h.update(tenant_scope.as_bytes());
    h.update([0u8]);
    h.update(intent.as_bytes());
    format!("{CLIENT_KEY_PREFIX}{:x}", h.finalize())
}

struct RegistryInner {
    /// `(tenant_scope, intent)` → logical id (idempotent init).
    client_index: HashMap<(String, String), Uuid>,
    /// All minted sessions (for lookup by id).
    sessions: HashMap<Uuid, LogicalSessionRecord>,
}

/// Logical session minting and lookup; optional Redis mirror for multi-replica `plasm-mcp`.
#[derive(Clone)]
pub struct LogicalSessionRegistry {
    inner: Arc<RwLock<RegistryInner>>,
    redis: Arc<RwLock<Option<Arc<RedisBackend>>>>,
}

impl Default for LogicalSessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LogicalSessionRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(RegistryInner {
                client_index: HashMap::new(),
                sessions: HashMap::new(),
            })),
            redis: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn attach_redis(&self, backend: Arc<RedisBackend>) {
        *self.redis.write().await = Some(backend);
    }

    async fn redis(&self) -> Option<Arc<RedisBackend>> {
        self.redis.read().await.clone()
    }

    async fn cache_record(&self, rec: LogicalSessionRecord) {
        let mut g = self.inner.write().await;
        let k = (rec.tenant_scope.clone(), rec.intent.as_str().to_string());
        g.client_index.insert(k, rec.logical_session_id.0);
        g.sessions.insert(rec.logical_session_id.0, rec);
    }

    async fn persist_record(&self, rec: &LogicalSessionRecord) {
        self.cache_record(rec.clone()).await;
        if let Some(redis) = self.redis().await.as_ref() {
            redis
                .set_json(
                    &session_key(&rec.logical_session_id.0),
                    &PersistedLogicalSession::from(rec),
                )
                .await;
            redis
                .set_json(
                    &client_index_key(&rec.tenant_scope, rec.intent.as_str()),
                    &rec.logical_session_id.0.to_string(),
                )
                .await;
        }
    }

    async fn load_session_from_redis(&self, id: Uuid) -> Option<LogicalSessionRecord> {
        let redis = self.redis().await?;
        let persisted: PersistedLogicalSession = redis.get_json(&session_key(&id)).await?;
        let rec = LogicalSessionRecord::from(persisted);
        self.cache_record(rec.clone()).await;
        Some(rec)
    }

    async fn load_by_client_index_from_redis(
        &self,
        tenant_scope: &str,
        intent: &str,
    ) -> Option<LogicalSessionRecord> {
        let redis = self.redis().await?;
        let id_str: String = redis
            .get_json(&client_index_key(tenant_scope, intent))
            .await?;
        let id = id_str.parse().ok()?;
        self.load_session_from_redis(id).await
    }

    /// Idempotent: same `(tenant_scope, intent)` returns the same [`LogicalSessionId`].
    pub async fn init_session(
        &self,
        tenant_scope: &str,
        intent: &ClientSessionKey,
    ) -> LogicalSessionRecord {
        let k = (tenant_scope.to_string(), intent.as_str().to_string());
        {
            let g = self.inner.read().await;
            if let Some(id) = g.client_index.get(&k).copied() {
                if let Some(rec) = g.sessions.get(&id) {
                    return rec.clone();
                }
            }
        }
        if let Some(rec) = self
            .load_by_client_index_from_redis(tenant_scope, intent.as_str())
            .await
        {
            return rec;
        }
        let logical_session_id = LogicalSessionId::new_v4();
        let rec = LogicalSessionRecord {
            logical_session_id,
            intent: intent.clone(),
            tenant_scope: tenant_scope.to_string(),
        };
        self.persist_record(&rec).await;
        rec
    }

    pub async fn get(&self, id: LogicalSessionId) -> Option<LogicalSessionRecord> {
        {
            let g = self.inner.read().await;
            if let Some(rec) = g.sessions.get(&id.0) {
                if let Some(redis) = self.redis().await.as_ref() {
                    redis.touch(&session_key(&id.0)).await;
                }
                return Some(rec.clone());
            }
        }
        self.load_session_from_redis(id.0).await
    }

    /// Verify the logical session exists and belongs to this tenant scope.
    pub async fn verify_tenant(&self, id: LogicalSessionId, tenant_scope: &str) -> bool {
        self.get(id)
            .await
            .map(|r| r.tenant_scope == tenant_scope)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn init_session_idempotent_in_memory() {
        let reg = LogicalSessionRegistry::new();
        let intent = ClientSessionKey::new("goal-a");
        let a = reg.init_session("tenant", &intent).await;
        let b = reg.init_session("tenant", &intent).await;
        assert_eq!(a.logical_session_id, b.logical_session_id);
        assert!(reg.verify_tenant(a.logical_session_id, "tenant").await);
        assert!(!reg.verify_tenant(a.logical_session_id, "other").await);
    }

    /// Requires `PLASM_TEST_REDIS_URL` (same as other agent-core Redis integration tests).
    #[tokio::test]
    async fn verify_tenant_hydrates_from_redis_across_registry_instances() {
        let Some(url) = std::env::var("PLASM_TEST_REDIS_URL")
            .ok()
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        let backend = Arc::new(
            RedisBackend::connect(&url, Duration::from_secs(120))
                .await
                .expect("redis connect"),
        );
        let reg_a = LogicalSessionRegistry::new();
        reg_a.attach_redis(Arc::clone(&backend)).await;
        let reg_b = LogicalSessionRegistry::new();
        reg_b.attach_redis(backend).await;

        let rec = reg_a
            .init_session("tenant-smoke", &ClientSessionKey::new("cross-pod-intent"))
            .await;
        assert!(
            reg_b
                .verify_tenant(rec.logical_session_id, "tenant-smoke")
                .await,
            "pod B must verify logical session minted on pod A"
        );
    }
}
