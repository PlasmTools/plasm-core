//! Agent-scoped logical session identity: server-minted [`LogicalSessionId`] + explicit
//! **`session_mode`** (`new` / `extend`) on MCP `plasm_context`.
//!
//! **Intent does not select the session.** Per-turn `intent` strings accumulate on the logical
//! record for capability scoring only.
//!
//! ## Roles vs other MCP session state
//!
//! - **`LogicalSessionRegistry`** — mints [`LogicalSessionId`] on `session_mode: new`; lookup by id
//!   on `extend`. With `PLASM_MCP_TRANSPORT_REDIS_URL`, records mirror to Redis for multi-replica.
//! - **`PlasmHostState::logical_execute_bindings`** — host-wide latest `(prompt_hash, session_id)`
//!   per logical id for **`resources/read`** and reconnect hydration.
//! - **`McpTransportState::logical_by_id`** — per MCP transport cache; not the minting authority.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use crate::mcp_transport_store::RedisBackend;

const SESSION_KEY_PREFIX: &str = "mcp:logical:session:v2:";
const RECENT_SESSIONS_CAP: usize = 32;

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

/// MCP `plasm_context` session lifecycle verb.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlasmContextSessionMode {
    New,
    Extend,
}

impl PlasmContextSessionMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "new" => Some(Self::New),
            "extend" => Some(Self::Extend),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Extend => "extend",
        }
    }
}

/// Accumulated intent cap for scoring (Unicode scalar count).
const ACCUMULATED_INTENT_MAX_SCALARS: usize = 2048;

#[derive(Clone, Debug)]
pub struct LogicalSessionRecord {
    pub logical_session_id: LogicalSessionId,
    pub tenant_scope: String,
    pub discovery_pin: Option<crate::discovery_store::DiscoverySessionPin>,
    /// Append-only per `plasm_context` turn (`new` seeds the first turn).
    pub intent_provenance: crate::intent_provenance::IntentProvenance,
    /// Bounded display/scoring projection; never used as discovery ancestry.
    pub accumulated_intent: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedLogicalSession {
    logical_session_id: Uuid,
    tenant_scope: String,
    provenance: crate::intent_provenance::IntentProvenance,
    #[serde(deserialize_with = "Option::deserialize")]
    discovery_pin: Option<crate::discovery_store::DiscoverySessionPin>,
}

impl From<&LogicalSessionRecord> for PersistedLogicalSession {
    fn from(rec: &LogicalSessionRecord) -> Self {
        Self {
            logical_session_id: rec.logical_session_id.0,
            tenant_scope: rec.tenant_scope.clone(),
            provenance: rec.intent_provenance.clone(),
            discovery_pin: rec.discovery_pin.clone(),
        }
    }
}

impl From<PersistedLogicalSession> for LogicalSessionRecord {
    fn from(p: PersistedLogicalSession) -> Self {
        let intent_turns: Vec<_> = p.provenance.turns().map(str::to_owned).collect();
        Self {
            logical_session_id: LogicalSessionId(p.logical_session_id),
            tenant_scope: p.tenant_scope,
            accumulated_intent: normalize_accumulated_intent(&intent_turns),
            intent_provenance: p.provenance,
            discovery_pin: p.discovery_pin,
        }
    }
}

fn session_key(id: &Uuid) -> String {
    format!("{SESSION_KEY_PREFIX}{id}")
}

/// Trim, drop empty turns, join with newlines, cap length for scoring stability.
#[must_use]
pub fn normalize_accumulated_intent(turns: &[String]) -> String {
    let parts: Vec<String> = turns
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    let mut joined = parts.join("\n");
    if joined.chars().count() > ACCUMULATED_INTENT_MAX_SCALARS {
        let first = parts.first().cloned().unwrap_or_default();
        let tail: Vec<String> = parts.into_iter().rev().take(8).rev().collect();
        joined = if tail.first().map(String::as_str) == Some(first.as_str()) {
            tail.join("\n")
        } else {
            format!("{first}\n{}", tail.join("\n"))
        };
        if joined.chars().count() > ACCUMULATED_INTENT_MAX_SCALARS {
            joined = joined
                .chars()
                .take(ACCUMULATED_INTENT_MAX_SCALARS)
                .chain(Some('…'))
                .collect();
        }
    }
    joined
}

fn normalize_intent_turn(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() || t.contains('\0') {
        None
    } else {
        Some(t.to_string())
    }
}

struct RegistryInner {
    sessions: HashMap<Uuid, LogicalSessionRecord>,
    /// Recent logical session ids per tenant (newest last) for churn advisory.
    recent_by_tenant: HashMap<String, VecDeque<Uuid>>,
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
                sessions: HashMap::new(),
                recent_by_tenant: HashMap::new(),
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

    fn track_recent(inner: &mut RegistryInner, tenant_scope: &str, id: Uuid) {
        let q = inner
            .recent_by_tenant
            .entry(tenant_scope.to_string())
            .or_default();
        q.retain(|x| *x != id);
        q.push_back(id);
        while q.len() > RECENT_SESSIONS_CAP {
            q.pop_front();
        }
    }

    async fn cache_record(&self, rec: LogicalSessionRecord) {
        let id = rec.logical_session_id.0;
        let tenant = rec.tenant_scope.clone();
        let mut g = self.inner.write().await;
        g.sessions.insert(id, rec);
        Self::track_recent(&mut g, &tenant, id);
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
        }
    }

    async fn load_session_from_redis(&self, id: Uuid) -> Option<LogicalSessionRecord> {
        let redis = self.redis().await?;
        let persisted: PersistedLogicalSession = redis.get_json(&session_key(&id)).await?;
        let rec = LogicalSessionRecord::from(persisted);
        self.cache_record(rec.clone()).await;
        Some(rec)
    }

    /// Mint a **new** logical session (always fresh UUID). Seeds the first intent turn.
    pub async fn mint_session(
        &self,
        tenant_scope: &str,
        first_intent_turn: &str,
    ) -> Result<LogicalSessionRecord, String> {
        let turn = normalize_intent_turn(first_intent_turn)
            .ok_or("logical session requires valid nonempty intent")?;
        let intent_turns = vec![turn];
        let accumulated_intent = normalize_accumulated_intent(&intent_turns);
        let rec = LogicalSessionRecord {
            logical_session_id: LogicalSessionId::new_v4(),
            tenant_scope: tenant_scope.to_string(),
            intent_provenance: crate::intent_provenance::IntentProvenance::from_turns(intent_turns)
                .map_err(|e| e.to_string())?,
            accumulated_intent,
            discovery_pin: None,
        };
        self.persist_record(&rec).await;
        Ok(rec)
    }

    /// Commit the identity already pinned by successful discovery. Identical retries are idempotent.
    pub async fn register_routed_session(
        &self,
        id: LogicalSessionId,
        tenant_scope: &str,
        provenance: &crate::intent_provenance::IntentProvenance,
        discovery_pin: Option<crate::discovery_store::DiscoverySessionPin>,
    ) -> Result<LogicalSessionRecord, String> {
        if let Some(existing) = self.get(id).await {
            if existing.tenant_scope != tenant_scope {
                return Err("routing session belongs to another scope".into());
            }
            if !provenance
                .turns()
                .take(existing.intent_provenance.turns().count())
                .eq(existing.intent_provenance.turns())
            {
                return Err("routing intent rewrites session ancestry".into());
            }
        }
        let intent_turns: Vec<_> = provenance.turns().map(str::to_owned).collect();
        let rec = LogicalSessionRecord {
            logical_session_id: id,
            tenant_scope: tenant_scope.to_owned(),
            accumulated_intent: normalize_accumulated_intent(&intent_turns),
            intent_provenance: provenance.clone(),
            discovery_pin,
        };
        self.persist_record(&rec).await;
        Ok(rec)
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

    /// Recent logical sessions for this tenant (newest last), excluding `except`.
    pub async fn recent_sessions_for_tenant(
        &self,
        tenant_scope: &str,
        except: Option<LogicalSessionId>,
    ) -> Vec<LogicalSessionRecord> {
        let ids: Vec<Uuid> = {
            let g = self.inner.read().await;
            g.recent_by_tenant
                .get(tenant_scope)
                .map(|q| q.iter().copied().collect())
                .unwrap_or_default()
        };
        let except_uuid = except.map(|x| x.0);
        let mut out = Vec::new();
        for id in ids {
            if except_uuid == Some(id) {
                continue;
            }
            if let Some(rec) = self.get(LogicalSessionId(id)).await {
                out.push(rec);
            }
        }
        out
    }

    /// Verify the logical session exists and belongs to this tenant scope.
    pub async fn verify_tenant(&self, id: LogicalSessionId, tenant_scope: &str) -> bool {
        self.get(id)
            .await
            .map(|r| r.tenant_scope == tenant_scope)
            .unwrap_or(false)
    }
}

/// Preview of accumulated intent for `_meta.plasm` (truncated).
#[must_use]
pub fn accumulated_intent_meta_preview(accumulated: &str, max_chars: usize) -> String {
    if accumulated.chars().count() <= max_chars {
        accumulated.to_string()
    } else {
        accumulated
            .chars()
            .take(max_chars)
            .chain(Some('…'))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn normalize_accumulated_intent_joins_turns() {
        let turns = vec!["first".into(), "second".into()];
        assert_eq!(normalize_accumulated_intent(&turns), "first\nsecond");
    }

    #[tokio::test]
    async fn mint_session_always_fresh() {
        let reg = LogicalSessionRegistry::new();
        let a = reg.mint_session("tenant", "goal-a").await.unwrap();
        let b = reg.mint_session("tenant", "goal-a").await.unwrap();
        assert_ne!(a.logical_session_id, b.logical_session_id);
        assert_eq!(
            a.intent_provenance.turns().collect::<Vec<_>>(),
            vec!["goal-a"]
        );
        assert!(reg.verify_tenant(a.logical_session_id, "tenant").await);
        assert!(!reg.verify_tenant(a.logical_session_id, "other").await);
    }

    #[tokio::test]
    async fn append_intent_turn_accumulates() {
        let reg = LogicalSessionRegistry::new();
        let rec = reg.mint_session("tenant", "turn-one").await.unwrap();
        let id = rec.logical_session_id;
        let extended = reg
            .register_routed_session(
                id,
                "tenant",
                &rec.intent_provenance.derived("turn-two".into()).unwrap(),
                None,
            )
            .await
            .expect("extend");
        assert_eq!(extended.intent_provenance.turns().count(), 2);
        assert_eq!(extended.accumulated_intent, "turn-one\nturn-two");
    }

    #[tokio::test]
    async fn pending_discovery_roundtrip_preserves_pin_and_ancestry() {
        let registry = LogicalSessionRegistry::new();
        let id = LogicalSessionId::new_v4();
        let provenance = crate::intent_provenance::IntentProvenance::from_turns([
            "  Only selected records.  ".into(),
        ])
        .unwrap();
        let pin = crate::discovery_store::DiscoverySessionPin {
            pin_id: id.to_string(),
            generation: "matrix-generation".into(),
            authorization: crate::discovery_store::DiscoveryAuthorization::catalogs(
                ["matrix".into()].into(),
            ),
        };
        let pending = registry
            .register_routed_session(id, "tenant", &provenance, Some(pin.clone()))
            .await
            .unwrap();
        let wire = serde_json::to_string(&PersistedLogicalSession::from(&pending)).unwrap();
        let restored = LogicalSessionRecord::from(
            serde_json::from_str::<PersistedLogicalSession>(&wire).unwrap(),
        );
        assert_eq!(restored.discovery_pin, Some(pin.clone()));
        assert_eq!(restored.intent_provenance, provenance);
        let child = provenance
            .derived("Read the related records.".into())
            .unwrap();
        let extended = registry
            .register_routed_session(id, "tenant", &child, Some(pin.clone()))
            .await
            .unwrap();
        assert_eq!(extended.logical_session_id, id);
        assert_eq!(extended.discovery_pin, Some(pin));
        assert_eq!(extended.intent_provenance, child);
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
            .mint_session("tenant-smoke", "cross-pod")
            .await
            .unwrap();
        assert!(
            reg_b
                .verify_tenant(rec.logical_session_id, "tenant-smoke")
                .await,
            "pod B must verify logical session minted on pod A"
        );
    }
}
