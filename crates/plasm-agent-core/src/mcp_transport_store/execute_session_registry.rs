//! Durable execute-session descriptors for cross-pod rehydration.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;

use crate::execute_session::{ExecuteSession, SessionReuseKey};

use super::redis_backend::RedisBackend;

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
    #[serde(default)]
    pub bindings_by_entry: indexmap::IndexMap<String, crate::binding_slots::SessionBindingMap>,
    #[serde(default)]
    pub plan_commits: Vec<PersistedPlanCommitRecord>,
    #[serde(default)]
    pub plan_commit_next: u64,
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
        for (eid, ctx) in &session.contexts_by_entry {
            catalog_cgs_hashes_by_entry
                .insert(eid.clone(), ctx.cgs.effective_catalog_cgs_hash_hex());
        }
        let plan_snapshot = session.snapshot_plan_commits_for_persist();
        Self {
            prompt_hash: session.prompt_hash.clone(),
            session_id: session_id.to_string(),
            prompt_text: session.prompt_text.clone(),
            entry_id: session.entry_id.clone(),
            context_entry_ids: session.contexts_by_entry.keys().cloned().collect(),
            entities: session.entities.clone(),
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
            bindings_by_entry: session.bindings_by_entry.clone(),
            plan_commits: plan_snapshot.records,
            plan_commit_next: plan_snapshot.next_sequence,
        }
    }
}

#[derive(Clone, Default)]
pub struct ExecuteSessionRegistry {
    redis: Arc<RwLock<Option<Arc<RedisBackend>>>>,
}

impl ExecuteSessionRegistry {
    pub fn new_in_memory() -> Self {
        Self::default()
    }

    pub async fn attach_redis(&self, backend: Arc<RedisBackend>) {
        *self.redis.write().await = Some(backend);
    }

    async fn redis(&self) -> Option<Arc<RedisBackend>> {
        self.redis.read().await.clone()
    }

    pub async fn persist(
        &self,
        session: &ExecuteSession,
        session_id: &str,
        reuse_key: &SessionReuseKey,
    ) {
        let Some(redis) = self.redis().await else {
            return;
        };
        let desc = PersistedExecuteSessionDescriptor::from_session_and_reuse(
            session, session_id, reuse_key,
        );
        redis
            .set_json(&session_key(&desc.prompt_hash, &desc.session_id), &desc)
            .await;
    }

    /// Refresh descriptor after in-session mutation (federate/expand); keeps stored reuse key when present.
    pub async fn persist_or_update(&self, session: &ExecuteSession, session_id: &str) {
        let Some(redis) = self.redis().await else {
            return;
        };
        let key = session_key(&session.prompt_hash, session_id);
        let reuse_key = if let Some(existing) = redis
            .get_json::<PersistedExecuteSessionDescriptor>(&key)
            .await
        {
            existing.reuse_key
        } else {
            return;
        };
        let reuse_key: SessionReuseKey = reuse_key.into();
        let desc = PersistedExecuteSessionDescriptor::from_session_and_reuse(
            session, session_id, &reuse_key,
        );
        redis.set_json(&key, &desc).await;
    }

    pub async fn load(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Option<PersistedExecuteSessionDescriptor> {
        let redis = self.redis().await?;
        let key = session_key(prompt_hash, session_id);
        let desc: PersistedExecuteSessionDescriptor = redis.get_json(&key).await?;
        redis.touch(&key).await;
        Some(desc)
    }
}
