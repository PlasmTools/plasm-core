//! Durable execute-session descriptors for cross-pod rehydration.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;

use crate::execute_session::{ExecuteSession, SessionReuseKey};

use super::persisted_operations::{merge_operation_patch, OperationPersistPatch};
use super::redis_backend::RedisBackend;

type TestJsonStore = Arc<RwLock<std::collections::HashMap<String, String>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecuteSessionPersistOutcome {
    InMemoryOnly,
    Durable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecuteSessionPersistError {
    MissingReuseKey,
    /// Hot session `domain_revision` is behind durable; caller must rehydrate before writing.
    HotBehindDurable {
        hot_revision: u32,
        durable_revision: u32,
    },
    /// Live execute row missing (catalog rotation / rehydrate failure) — do not use a caller Arc.
    SessionUnavailable,
}

impl std::fmt::Display for ExecuteSessionPersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingReuseKey => write!(
                f,
                "plan_commit_ref persistence failed: execute session reuse key unavailable — reopen `plasm_context` and dry-run again"
            ),
            Self::HotBehindDurable {
                hot_revision,
                durable_revision,
            } => write!(
                f,
                "execute session hot domain_revision={hot_revision} is behind durable={durable_revision} — retry after rehydrate"
            ),
            Self::SessionUnavailable => write!(
                f,
                "execute session unavailable — reopen `plasm_context` and dry-run again"
            ),
        }
    }
}

/// Outcome of merging durable plan commits into a hot execute row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeLiveOutcome {
    /// Plans/ops restored onto hot. Hot may be in-sync with or ahead of durable; only plans with
    /// `domain_revision <=` hot are attached (append-only).
    Merged,
    /// Durable exposure is ahead of hot — discard hot and full-rehydrate from durable.
    NeedsRehydrate,
}

impl std::error::Error for ExecuteSessionPersistError {}

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
            logical_session_id: k.logical_session_id,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PersistedPlanCommitRecord {
    pub commit_ref: String,
    pub commit_id_hex: String,
    #[serde(default)]
    pub domain_revision: u32,
    #[serde(default)]
    pub policy_revision: u64,
    #[serde(default)]
    pub comp: Option<serde_json::Value>,
    #[serde(default)]
    pub program: String,
    pub dry_review: crate::plan_dry_display::PlanDryReview,
    pub verdict: crate::plan_dry_display::PlanDryVerdict,
    pub expires_at_unix: u64,
    #[serde(default)]
    pub dry_cache: crate::operation::PlanCommitDryCache,
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
    /// Share-link token bound via session-effect capabilities (e.g. Proof `document_share_bind`).
    #[serde(default)]
    pub session_share_token: Option<String>,
    /// Proof `baseToken` from the latest successful `editor_state_get`.
    #[serde(default)]
    pub session_proof_base_token: Option<String>,
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
        bind_credentials: crate::execute_session::SessionBindCredentialsSnapshot,
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
            session_share_token: bind_credentials.session_share_token,
            session_proof_base_token: bind_credentials.session_proof_base_token,
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

    pub async fn durable_backend_configured(&self) -> bool {
        self.test_json().await.is_some() || self.redis().await.is_some()
    }

    pub async fn persist(
        &self,
        session: &ExecuteSession,
        session_id: &str,
        reuse_key: &SessionReuseKey,
    ) {
        let bind_credentials = session.snapshot_bind_credentials().await;
        let desc = PersistedExecuteSessionDescriptor::from_session_and_reuse(
            session,
            session_id,
            reuse_key,
            bind_credentials,
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
    ) -> Result<ExecuteSessionPersistOutcome, ExecuteSessionPersistError> {
        use crate::domain_revision::{compare_exposure, DomainRevision, ExposureSync};

        let durable = self.durable_backend_configured().await;
        let key = session_key(&session.prompt_hash, session_id);
        let reuse_key = if let Some(existing) = self.load_json(&key).await {
            match compare_exposure(
                DomainRevision::new(session.domain_revision),
                DomainRevision::new(existing.domain_revision),
            ) {
                ExposureSync::HotBehind => {
                    return Err(ExecuteSessionPersistError::HotBehindDurable {
                        hot_revision: session.domain_revision,
                        durable_revision: existing.domain_revision,
                    });
                }
                ExposureSync::InSync | ExposureSync::HotAhead => {}
            }
            existing.reuse_key.into()
        } else if let Some(fallback) = reuse_key_fallback {
            fallback.clone()
        } else if durable {
            return Err(ExecuteSessionPersistError::MissingReuseKey);
        } else {
            return Ok(ExecuteSessionPersistOutcome::InMemoryOnly);
        };
        let desc = PersistedExecuteSessionDescriptor::from_session_and_reuse(
            session,
            session_id,
            &reuse_key,
            session.snapshot_bind_credentials().await,
        );
        self.write_descriptor(&key, &desc).await
    }

    /// Patch durable bind credentials after session-effect bind or proof base-token refresh.
    pub async fn patch_bind_credentials(
        &self,
        session: &ExecuteSession,
        session_id: &str,
        reuse_key_fallback: Option<&SessionReuseKey>,
    ) -> Result<ExecuteSessionPersistOutcome, ExecuteSessionPersistError> {
        let durable = self.durable_backend_configured().await;
        if !durable {
            return Ok(ExecuteSessionPersistOutcome::InMemoryOnly);
        }
        let key = session_key(&session.prompt_hash, session_id);
        let creds = session.snapshot_bind_credentials().await;
        if let Some(mut existing) = self.load_json(&key).await {
            existing.session_share_token = creds.session_share_token;
            existing.session_proof_base_token = creds.session_proof_base_token;
            existing.expires_at_unix = expires_at_from_now();
            return self.write_descriptor(&key, &existing).await;
        }
        let Some(reuse_key) = reuse_key_fallback else {
            return Err(ExecuteSessionPersistError::MissingReuseKey);
        };
        self.persist_or_update(session, session_id, Some(reuse_key))
            .await
    }

    /// Patch only `plan_commits` / `plan_commit_next` on the durable row when exposure revisions
    /// match. If hot is ahead of durable, full-persist hot (authoritative). If hot is behind,
    /// refuse so callers rehydrate first.
    pub async fn patch_plan_commits_only(
        &self,
        session: &ExecuteSession,
        session_id: &str,
        reuse_key_fallback: Option<&SessionReuseKey>,
    ) -> Result<ExecuteSessionPersistOutcome, ExecuteSessionPersistError> {
        let durable = self.durable_backend_configured().await;
        let key = session_key(&session.prompt_hash, session_id);
        let snapshot = session.snapshot_plan_commits_for_persist();
        if let Some(mut existing) = self.load_json(&key).await {
            use crate::domain_revision::{compare_exposure, DomainRevision, ExposureSync};
            match compare_exposure(
                DomainRevision::new(session.domain_revision),
                DomainRevision::new(existing.domain_revision),
            ) {
                ExposureSync::HotBehind => {
                    return Err(ExecuteSessionPersistError::HotBehindDurable {
                        hot_revision: session.domain_revision,
                        durable_revision: existing.domain_revision,
                    });
                }
                ExposureSync::HotAhead => {
                    // Hot exposure advanced (e.g. prior durable write failed) — reconcile fully.
                    return self
                        .persist_or_update(session, session_id, reuse_key_fallback)
                        .await;
                }
                ExposureSync::InSync => {
                    existing.plan_commits = snapshot.records;
                    existing.plan_commit_next = snapshot.next_sequence;
                    existing.expires_at_unix = expires_at_from_now();
                    return self.write_descriptor(&key, &existing).await;
                }
            }
        }
        if !durable {
            return Ok(ExecuteSessionPersistOutcome::InMemoryOnly);
        }
        // No durable row yet: first upsert still needs a full descriptor (open path).
        let Some(fallback) = reuse_key_fallback else {
            return Err(ExecuteSessionPersistError::MissingReuseKey);
        };
        let desc = PersistedExecuteSessionDescriptor::from_session_and_reuse(
            session,
            session_id,
            fallback,
            session.snapshot_bind_credentials().await,
        );
        self.write_descriptor(&key, &desc).await
    }

    async fn write_descriptor(
        &self,
        key: &str,
        desc: &PersistedExecuteSessionDescriptor,
    ) -> Result<ExecuteSessionPersistOutcome, ExecuteSessionPersistError> {
        if let Some(map) = self.test_json().await {
            if let Ok(payload) = serde_json::to_string(desc) {
                map.write().await.insert(key.to_string(), payload);
            }
            return Ok(ExecuteSessionPersistOutcome::Durable);
        }
        let Some(redis) = self.redis().await else {
            return Ok(ExecuteSessionPersistOutcome::InMemoryOnly);
        };
        redis.set_json(key, desc).await;
        Ok(ExecuteSessionPersistOutcome::Durable)
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

    /// Merge cross-pod durable plan commits / ops into a hot row.
    ///
    /// When durable exposure is **ahead** of hot, returns [`MergeLiveOutcome::NeedsRehydrate`].
    /// Restores plan commits pinned at `domain_revision <=` hot (append-only: older plans stay
    /// valid after extend). Never attaches plans pinned *ahead* of the hot row.
    pub async fn merge_into_live_session(
        &self,
        session: &ExecuteSession,
        prompt_hash: &str,
        session_id: &str,
    ) -> MergeLiveOutcome {
        use crate::domain_revision::{
            compare_exposure, plan_compatible_with_session, DomainRevision, ExposureSync,
        };

        if !self.durable_backend_configured().await {
            return MergeLiveOutcome::Merged;
        }
        let Some(desc) = self.load(prompt_hash, session_id).await else {
            return MergeLiveOutcome::Merged;
        };
        let session_rev = DomainRevision::new(session.domain_revision);
        if compare_exposure(session_rev, DomainRevision::new(desc.domain_revision))
            == ExposureSync::HotBehind
        {
            return MergeLiveOutcome::NeedsRehydrate;
        }
        let plan_commit_next = desc.plan_commit_next;
        let ops = super::OperationPersistSnapshot {
            operations: desc.operations,
            operation_handle_next: desc.operation_handle_next,
        };
        let plans: Vec<_> = desc
            .plan_commits
            .into_iter()
            .filter(|p| {
                plan_compatible_with_session(DomainRevision::new(p.domain_revision), session_rev)
            })
            .collect();
        session.restore_persisted_plan_commits(&plans, plan_commit_next);
        session.restore_persisted_operations(&ops);
        session
            .restore_bind_credentials(&crate::execute_session::SessionBindCredentialsSnapshot {
                session_share_token: desc.session_share_token.clone(),
                session_proof_base_token: desc.session_proof_base_token.clone(),
            })
            .await;
        MergeLiveOutcome::Merged
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

    /// Drop all cross-pod execute session descriptors (e.g. after catalog-dir reload).
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
