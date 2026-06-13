//! Agent-scoped logical session identity: stable host **`intent`** (MCP `plasm_context` JSON string) +
//! server-minted `LogicalSessionId`.
//!
//! ## Roles vs other MCP session state (in-process)
//!
//! - **`LogicalSessionRegistry` (this module)** — sole **mint** for `LogicalSessionId` and
//!   **idempotent** lookup by `(tenant_scope, intent string)`; [`LogicalSessionRegistry::verify_tenant`]
//!   gates tool use.
//! - **`PlasmHostState::logical_execute_bindings`** ([`crate::server_state::PlasmHostState`]) —
//!   host-wide **latest** `(prompt_hash, execute_session_id)` per logical id for **`resources/read`**
//!   and reconnect **hydration** without relying on this connection’s RAM.
//! - **`McpTransportState::logical_by_id`** ([`crate::mcp_server`]) — **per MCP transport**
//!   (`MCP-Session-Id`) cache for binding + stats + `_meta.plasm` index; **not** the minting authority.
//!   Logical session continuity uses stateless **`l_<token>`** wire refs ([`crate::mcp_logical_ref`]), not transport slots.
//!
//! Used for idempotent `plasm_context` and for correlating MCP tools with a single execute
//! session without relying on MCP transport session ids. Durable cross-replica storage is a future
//! layer; this module holds the in-process registry.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

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

struct RegistryInner {
    /// `(tenant_scope, intent)` → logical id (idempotent init).
    client_index: HashMap<(String, String), Uuid>,
    /// All minted sessions (for lookup by id).
    sessions: HashMap<Uuid, LogicalSessionRecord>,
}

/// In-process registry for logical session minting and lookup.
#[derive(Clone)]
pub struct LogicalSessionRegistry {
    inner: Arc<RwLock<RegistryInner>>,
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
        }
    }

    /// Idempotent: same `(tenant_scope, intent)` returns the same [`LogicalSessionId`].
    pub async fn init_session(
        &self,
        tenant_scope: &str,
        intent: &ClientSessionKey,
    ) -> LogicalSessionRecord {
        let k = (tenant_scope.to_string(), intent.as_str().to_string());
        let mut g = self.inner.write().await;
        if let Some(id) = g.client_index.get(&k).copied() {
            return g
                .sessions
                .get(&id)
                .cloned()
                .expect("client_index without session");
        }
        let logical_session_id = LogicalSessionId::new_v4();
        let rec = LogicalSessionRecord {
            logical_session_id,
            intent: intent.clone(),
            tenant_scope: tenant_scope.to_string(),
        };
        g.client_index.insert(k, logical_session_id.0);
        g.sessions.insert(logical_session_id.0, rec.clone());
        rec
    }

    pub async fn get(&self, id: LogicalSessionId) -> Option<LogicalSessionRecord> {
        let g = self.inner.read().await;
        g.sessions.get(&id.0).cloned()
    }

    /// Verify the logical session exists and belongs to this tenant scope.
    pub async fn verify_tenant(&self, id: LogicalSessionId, tenant_scope: &str) -> bool {
        let g = self.inner.read().await;
        g.sessions
            .get(&id.0)
            .map(|r| r.tenant_scope == tenant_scope)
            .unwrap_or(false)
    }
}
