//! Durable execute-session descriptors for cross-pod rehydration.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;

use crate::execute_session::{ExecuteSession, SessionReuseKey};

use super::persisted_operations::{merge_operation_patch, OperationPersistPatch};
use super::redis_backend::RedisBackend;

type TestJsonStore = Arc<RwLock<std::collections::HashMap<String, String>>>;

const SESSION_KEY_PREFIX: &str = "mcp:execute:session:";

fn session_key(prompt_hash: &str, session_id: &str) -> String {
    format!("{SESSION_KEY_PREFIX}{prompt_hash}:{session_id}")
}

fn session_ttl_secs() -> u64 {
    crate::execute_session::session_ttl_secs()
}

fn expires_at_from_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_add(session_ttl_secs()))
        .unwrap_or(0)
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PersistedSessionReuseKey {
    pub tenant_scope: String,
    pub entry_id: String,
    pub catalog_cgs_hash: String,
    pub entities: Vec<String>,
    pub context_intent: Option<String>,
    pub ranked_capabilities: Option<Vec<String>>,
    pub principal: Option<String>,
    pub plugin_generation_id: Option<u64>,
    pub logical_session_id: Option<String>,
}

impl From<&SessionReuseKey> for PersistedSessionReuseKey {
    fn from(k: &SessionReuseKey) -> Self {
        Self {
            tenant_scope: k.tenant_scope.clone(),
            entry_id: k.entry_id.clone(),
            catalog_cgs_hash: k.catalog_cgs_hash.clone(),
            entities: k.entities.clone(),
            context_intent: k.context_intent.clone(),
            ranked_capabilities: k.ranked_capabilities.clone(),
            principal: k.principal.clone(),
            plugin_generation_id: k.plugin_generation_id,
            logical_session_id: k.logical_session_id.clone(),
        }
    }
}

impl From<PersistedSessionReuseKey> for SessionReuseKey {
    fn from(k: PersistedSessionReuseKey) -> Self {
        Self {
            tenant_scope: k.tenant_scope,
            entry_id: k.entry_id,
            catalog_cgs_hash: k.catalog_cgs_hash,
            entities: k.entities,
            context_intent: k.context_intent,
            ranked_capabilities: k.ranked_capabilities,
            principal: k.principal,
            plugin_generation_id: k.plugin_generation_id,
            logical_session_id: k.logical_session_id,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PersistedPlanCommitRecord {
    pub commit_ref: String,
    pub commit_id_hex: String,
    #[serde(default)]
    pub comp: Option<serde_json::Value>,
    #[serde(default)]
    pub program: String,
    pub dry_review: crate::plan_dry_display::PlanDryReview,
    pub verdict: crate::plan_dry_display::PlanDryVerdict,
    pub expires_at_unix: u64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PersistedExecuteSessionDescriptor {
    pub prompt_hash: String,
    pub session_id: String,
    pub prompt_text: String,
    pub entry_id: String,
    pub context_entry_ids: Vec<String>,
    pub entities: Vec<String>,
    /// Parallel catalog `entry_id` for each row in [`Self::entities`] (federated symbol map rehydrate).
    #[serde(default)]
    pub entity_catalog_entry_ids: Vec<String>,
    pub tenant_scope: String,
    pub principal_subject: String,
    pub http_backend: Option<String>,
    pub principal: Option<String>,
    pub catalog_cgs_hash: String,
    pub context_intent: Option<String>,
    pub ranked_capabilities: Option<Vec<String>>,
    pub plugin_generation_id: Option<u64>,
    pub domain_revision: u32,
    pub reuse_key: PersistedSessionReuseKey,
    /// Unix seconds after which rehydrate must reject (aligned with in-memory session TTL).
    #[serde(default = "default_expires_at_unix")]
    pub expires_at_unix: u64,
    #[serde(default)]
    pub catalog_cgs_hashes_by_entry: std::collections::HashMap<String, String>,
    /// Registry-base digests at open (before tenant materialization); used for rotation detection.
    #[serde(default)]
    pub registry_catalog_hashes_by_entry: std::collections::HashMap<String, String>,
    /// Tenant outbound hosted_kv keys per entry (cross-pod rehydrate materialization).
    #[serde(default)]
    pub outbound_hosted_kv_by_entry: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub bindings_by_entry: indexmap::IndexMap<String, crate::binding_slots::SessionBindingMap>,
    #[serde(default)]
    pub plan_commits: Vec<PersistedPlanCommitRecord>,
    #[serde(default)]
    pub plan_commit_next: u64,
    #[serde(default)]
    pub operations: Vec<super::persisted_operations::PersistedOperationDescriptor>,
    #[serde(default)]
    pub operation_handle_next: u64,
}

fn default_expires_at_unix() -> u64 {
    expires_at_from_now()
}

pub struct PlanCommitPersistSnapshot {
    pub records: Vec<PersistedPlanCommitRecord>,
    pub next_sequence: u64,
}

impl PersistedExecuteSessionDescriptor {
    pub fn from_session_and_reuse(
        session: &ExecuteSession,
        session_id: &str,
        reuse_key: &SessionReuseKey,
    ) -> Self {
        let mut catalog_cgs_hashes_by_entry = std::collections::HashMap::new();
        let mut outbound_hosted_kv_by_entry = std::collections::HashMap::new();
        for (eid, ctx) in &session.contexts_by_entry {
            catalog_cgs_hashes_by_entry
                .insert(eid.clone(), ctx.cgs.effective_catalog_cgs_hash_hex());
            if let Some(kv) =
                crate::execute_session_materialize::outbound_hosted_kv_from_cgs(ctx.cgs.as_ref())
            {
                outbound_hosted_kv_by_entry.insert(eid.clone(), kv);
            }
        }
        let plan_snapshot = session.snapshot_plan_commits_for_persist();
        let op_snapshot = session.snapshot_operations_for_persist();
        let entity_catalog_entry_ids = session
            .teaching_exposure
            .as_ref()
            .map(|e| e.entity_catalog_entry_ids.clone())
            .unwrap_or_else(|| vec![session.entry_id.clone(); session.entities.len()]);
        Self {
            prompt_hash: session.prompt_hash.clone(),
            session_id: session_id.to_string(),
            prompt_text: session.prompt_text.clone(),
            entry_id: session.entry_id.clone(),
            context_entry_ids: session.contexts_by_entry.keys().cloned().collect(),
            entities: session.entities.clone(),
            entity_catalog_entry_ids,
            tenant_scope: session.tenant_scope.clone(),
            principal_subject: session.principal_subject.clone(),
            http_backend: session.http_backend.clone(),
            principal: session.principal.clone(),
            catalog_cgs_hash: session.catalog_cgs_hash.clone(),
            context_intent: session.context_intent.clone(),
            ranked_capabilities: session.ranked_capabilities.clone(),
            plugin_generation_id: session.plugin_generation.as_ref().map(|g| g.id),
            domain_revision: session.domain_revision,
            reuse_key: PersistedSessionReuseKey::from(reuse_key),
            expires_at_unix: expires_at_from_now(),
            catalog_cgs_hashes_by_entry,
            registry_catalog_hashes_by_entry: session.registry_catalog_hashes_by_entry.clone(),
            outbound_hosted_kv_by_entry,
            bindings_by_entry: session.bindings_by_entry.clone(),
            plan_commits: plan_snapshot.records,
            plan_commit_next: plan_snapshot.next_sequence,
            operations: op_snapshot.operations,
            operation_handle_next: op_snapshot.operation_handle_next,
        }
    }
}

#[derive(Clone, Default)]
pub struct ExecuteSessionRegistry {
    redis: Arc<RwLock<Option<Arc<RedisBackend>>>>,
    test_json: Arc<RwLock<Option<TestJsonStore>>>,
}

impl ExecuteSessionRegistry {
    /// Shared in-memory JSON store for cross-pod rehydrate tests (no Redis required).
    pub fn with_test_json_store() -> (Self, TestJsonStore) {
        let map: TestJsonStore = Arc::new(RwLock::new(std::collections::HashMap::new()));
        (
            Self {
                redis: Default::default(),
                test_json: Arc::new(RwLock::new(Some(map.clone()))),
            },
            map,
        )
    }

    pub fn new_in_memory() -> Self {
        Self::default()
    }

    pub async fn attach_redis(&self, backend: Arc<RedisBackend>) {
        *self.redis.write().await = Some(backend);
    }

    async fn redis(&self) -> Option<Arc<RedisBackend>> {
        self.redis.read().await.clone()
    }

    async fn test_json(&self) -> Option<TestJsonStore> {
        self.test_json.read().await.clone()
    }

    pub async fn persist(
        &self,
        session: &ExecuteSession,
        session_id: &str,
        reuse_key: &SessionReuseKey,
    ) {
        let desc = PersistedExecuteSessionDescriptor::from_session_and_reuse(
            session, session_id, reuse_key,
        );
        let key = session_key(&desc.prompt_hash, &desc.session_id);
        if let Some(map) = self.test_json().await {
            if let Ok(payload) = serde_json::to_string(&desc) {
                map.write().await.insert(key, payload);
            }
            return;
        }
        let Some(redis) = self.redis().await else {
            return;
        };
        redis.set_json(&key, &desc).await;
    }

    /// Refresh descriptor after in-session mutation (federate/expand/plan commit).
    /// When no durable row exists yet, `reuse_key_fallback` seeds the first upsert.
    pub async fn persist_or_update(
        &self,
        session: &ExecuteSession,
        session_id: &str,
        reuse_key_fallback: Option<&SessionReuseKey>,
    ) {
        let key = session_key(&session.prompt_hash, session_id);
        let reuse_key = if let Some(existing) = self.load_json(&key).await {
            existing.reuse_key.into()
        } else if let Some(fallback) = reuse_key_fallback {
            fallback.clone()
        } else {
            return;
        };
        let desc = PersistedExecuteSessionDescriptor::from_session_and_reuse(
            session, session_id, &reuse_key,
        );
        if let Some(map) = self.test_json().await {
            if let Ok(payload) = serde_json::to_string(&desc) {
                map.write().await.insert(key, payload);
            }
            return;
        }
        let Some(redis) = self.redis().await else {
            return;
        };
        redis.set_json(&key, &desc).await;
    }

    async fn load_json(&self, key: &str) -> Option<PersistedExecuteSessionDescriptor> {
        if let Some(map) = self.test_json().await {
            let raw = map.read().await.get(key).cloned()?;
            return serde_json::from_str(&raw).ok();
        }
        let redis = self.redis().await?;
        redis.get_json(key).await
    }

    pub async fn load(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Option<PersistedExecuteSessionDescriptor> {
        let key = session_key(prompt_hash, session_id);
        let desc = self.load_json(&key).await?;
        if self.redis().await.is_some() {
            if let Some(redis) = self.redis().await {
                redis.touch(&key).await;
            }
        }
        Some(desc)
    }

    pub(crate) async fn delete(&self, prompt_hash: &str, session_id: &str) {
        let key = session_key(prompt_hash, session_id);
        if let Some(map) = self.test_json().await {
            map.write().await.remove(&key);
        }
        if let Some(redis) = self.redis().await {
            redis.delete(&key).await;
        }
    }

    /// Drop all cross-pod execute session descriptors (e.g. after plugin catalog reload).
    pub async fn purge_redis(&self) -> u64 {
        if let Some(redis) = self.redis().await {
            redis.delete_keys_matching_prefix(SESSION_KEY_PREFIX).await
        } else {
            0
        }
    }

    /// Merge one operation patch into the durable session descriptor (cross-pod wait/cancel metadata).
    pub async fn patch_session_operations(
        &self,
        prompt_hash: &str,
        session_id: &str,
        patch: OperationPersistPatch,
    ) {
        let key = session_key(prompt_hash, session_id);
        let Some(mut desc) = self.load_json(&key).await else {
            return;
        };
        merge_operation_patch(&mut desc.operations, &mut desc.operation_handle_next, patch);
        desc.expires_at_unix = expires_at_from_now();
        if let Some(map) = self.test_json().await {
            if let Ok(payload) = serde_json::to_string(&desc) {
                map.write().await.insert(key, payload);
            }
            return;
        }
        let Some(redis) = self.redis().await else {
            return;
        };
        redis.set_json(&key, &desc).await;
    }
}
