//! Durable execute-session descriptors for cross-pod rehydration.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;

use crate::execute_session::{ExecuteSession, SessionReuseKey};
use crate::server_state::PlasmHostState;

use super::persisted_operations::{merge_operation_patch, OperationPersistPatch};
use super::redis_backend::RedisBackend;

type TestJsonStore = Arc<RwLock<std::collections::HashMap<String, String>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecuteSessionPersistOutcome {
    InMemoryOnly,
    Durable,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ExecuteSessionPersistError {
    LearningReviewUnavailable,
    ExposureConflict,
    Encoding(#[source] Arc<serde_json::Error>),
    DurableWrite(#[source] Arc<redis::RedisError>),
    MissingReuseKey,
    /// Hot session `domain_revision` is behind durable; caller must rehydrate before writing.
    HotBehindDurable {
        hot_revision: u32,
        durable_revision: u32,
    },
    /// Live execute row missing (catalog rotation / rehydrate failure) — do not use a caller Arc.
    SessionUnavailable,
    /// Durable persist refused: execute row has no serializable symbol ledger.
    MissingSymbolLedger,
    Materialize(#[source] Arc<crate::execute_session_materialize::MaterializeError>),
    SymbolLedgerEncode(#[source] Arc<plasm_core::PersistedSymbolLedgerEncodeError>),
    LogicalLedgerWriteFailed(
        #[source] Arc<crate::mcp_transport_store::logical_symbol_ledger::SymbolLedgerUpsertError>,
    ),
}

impl std::fmt::Display for ExecuteSessionPersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LearningReviewUnavailable => write!(f, "workflow review cannot seal while evidence is incomplete or another review is active"),
            Self::ExposureConflict => write!(f, "execute exposure changed concurrently; refresh the session and retry"),
            Self::Encoding(error) => write!(f, "execute descriptor encoding failed: {error}"),
            Self::DurableWrite(error) => write!(f, "execute descriptor publication failed: {error}"),
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
            Self::MissingSymbolLedger => write!(
                f,
                "execute session persist refused: missing symbol ledger — reopen `plasm_context` with session_mode: \"new\""
            ),
            Self::Materialize(e) => write!(f, "execute session materialize failed: {e}"),
            Self::SymbolLedgerEncode(e) => write!(f, "symbol ledger encode failed: {e}"),
            Self::LogicalLedgerWriteFailed(source) => {
                write!(f, "logical symbol ledger durable write failed: {source}")
            }
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

impl From<crate::execute_session_materialize::MaterializeError> for ExecuteSessionPersistError {
    fn from(e: crate::execute_session_materialize::MaterializeError) -> Self {
        Self::Materialize(Arc::new(e))
    }
}

impl From<crate::mcp_transport_store::logical_symbol_ledger::SymbolLedgerUpsertError>
    for ExecuteSessionPersistError
{
    fn from(e: crate::mcp_transport_store::logical_symbol_ledger::SymbolLedgerUpsertError) -> Self {
        use crate::mcp_transport_store::logical_symbol_ledger::SymbolLedgerUpsertError;
        match e {
            SymbolLedgerUpsertError::Encode(err) => Self::SymbolLedgerEncode(Arc::new(err)),
            error @ SymbolLedgerUpsertError::RedisWriteFailed { .. } => {
                Self::LogicalLedgerWriteFailed(Arc::new(error))
            }
        }
    }
}

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

fn validate_exposure_successor(
    previous_revision: u32,
    previous_ledger: &[u8],
    incoming: &PersistedExecuteSessionDescriptor,
) -> Result<(), ExecuteSessionPersistError> {
    validate_exposure_version(
        previous_revision,
        previous_ledger,
        incoming.domain_revision,
        &incoming.symbol_ledger_bytes,
    )
}

fn validate_exposure_version(
    previous_revision: u32,
    previous_ledger: &[u8],
    incoming_revision: u32,
    incoming_ledger: &[u8],
) -> Result<(), ExecuteSessionPersistError> {
    if incoming_revision < previous_revision
        || (incoming_revision > previous_revision.saturating_add(1)
            && incoming_ledger != previous_ledger)
        || (incoming_revision == previous_revision && incoming_ledger != previous_ledger)
    {
        return Err(ExecuteSessionPersistError::ExposureConflict);
    }
    Ok(())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PersistedSessionReuseKey {
    pub tenant_scope: String,
    pub entry_id: String,
    pub catalog_cgs_hash: String,
    pub entities: Vec<String>,
    pub context_intent: Option<String>,
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
            principal: k.principal,
            logical_session_id: k.logical_session_id,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PersistedPlanCommitRecord {
    pub authorization: crate::plan_authorization::AuthorizationSnapshot,
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
    #[serde(default)]
    pub example_fetch_receipts:
        std::collections::BTreeMap<uuid::Uuid, crate::workflow_learning::serving::ServedExample>,
    #[serde(default)]
    pub discovery_pin: Option<crate::discovery_store::DiscoverySessionPin>,
    pub prompt_hash: String,
    pub session_id: String,
    pub prompt_text: String,
    /// Required: native-language sessions cannot rehydrate under Python teaching.
    pub python_teaching: plasm_core::prompt_render::python::PythonTeachingState,
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
    /// Append-only symbol ledger (`PLSL` + postcard); required for cross-pod rehydrate.
    #[serde(default)]
    pub symbol_ledger_bytes: Vec<u8>,
    /// Explicit prerequisite deployments (RA-17). Missing on older snapshots.
    #[serde(default)]
    pub prerequisite_deployments: plasm_core::prerequisites::DeploymentBindings,
}

fn default_expires_at_unix() -> u64 {
    expires_at_from_now()
}

pub struct PlanCommitPersistSnapshot {
    pub records: Vec<PersistedPlanCommitRecord>,
    pub next_sequence: u64,
}

impl PersistedExecuteSessionDescriptor {
    pub(crate) fn from_session_and_durable_snapshot(
        session: &ExecuteSession,
        session_id: &str,
        reuse_key: &SessionReuseKey,
        durable: &crate::execute_session_materialize::DurableExposureSnapshot,
    ) -> Self {
        let catalog_cgs_hashes_by_entry = crate::catalog_hash::effective_hash_map_to_strings(
            &durable.catalog_cgs_hashes_by_entry,
        );
        let outbound_hosted_kv_by_entry = session.materialized_outbound_hosted_kv_by_entry.clone();
        let plan_snapshot = session.snapshot_plan_commits_for_persist();
        let op_snapshot = session.snapshot_operations_for_persist();
        let entities = session.entities.clone();
        let entity_catalog_entry_ids = session
            .teaching_exposure
            .as_ref()
            .map(|exposure| {
                let mut search_from = 0usize;
                entities
                    .iter()
                    .map(|entity| {
                        let relative = exposure.entities[search_from..]
                            .iter()
                            .position(|candidate| candidate == entity);
                        if let Some(relative) = relative {
                            let index = search_from + relative;
                            search_from = index + 1;
                            exposure
                                .entity_catalog_entry_ids
                                .get(index)
                                .cloned()
                                .unwrap_or_else(|| session.entry_id.clone())
                        } else {
                            session.entry_id.clone()
                        }
                    })
                    .collect()
            })
            .unwrap_or_else(|| vec![session.entry_id.clone(); entities.len()]);
        Self {
            example_fetch_receipts: Default::default(),
            prompt_hash: session.prompt_hash.clone(),
            session_id: session_id.to_string(),
            prompt_text: session.prompt_text.clone(),
            python_teaching: session.python_teaching.clone(),
            entry_id: session.entry_id.clone(),
            context_entry_ids: session.contexts_by_entry.keys().cloned().collect(),
            entities,
            entity_catalog_entry_ids,
            tenant_scope: session.tenant_scope.clone(),
            principal_subject: session.principal_subject.clone(),
            http_backend: session.http_backend.clone(),
            principal: session.principal.clone(),
            catalog_cgs_hash: session.catalog_cgs_hash.clone(),
            context_intent: session.context_intent.clone(),
            domain_revision: session.domain_revision,
            reuse_key: PersistedSessionReuseKey::from(reuse_key),
            expires_at_unix: expires_at_from_now(),
            catalog_cgs_hashes_by_entry,
            discovery_pin: session.discovery_pin.clone(),
            registry_catalog_hashes_by_entry: session.registry_catalog_hashes_by_entry.clone(),
            outbound_hosted_kv_by_entry,
            bindings_by_entry: session.bindings_by_entry.clone(),
            plan_commits: plan_snapshot.records,
            plan_commit_next: plan_snapshot.next_sequence,
            operations: op_snapshot.operations,
            operation_handle_next: op_snapshot.operation_handle_next,
            symbol_ledger_bytes: durable.symbol_ledger_bytes.clone(),
            prerequisite_deployments: session.prerequisite_deployments.clone(),
        }
    }
}

#[derive(Clone, Default)]
pub struct ExecuteSessionRegistry {
    redis: Arc<RwLock<Option<Arc<RedisBackend>>>>,
    test_json: Arc<RwLock<Option<TestJsonStore>>>,
    memory_descriptors:
        Arc<RwLock<std::collections::HashMap<String, PersistedExecuteSessionDescriptor>>>,
    learning_watermarks: Arc<RwLock<std::collections::HashMap<String, LearningExecutionWatermark>>>,
}

#[derive(Default)]
struct LearningExecutionWatermark {
    issued: u64,
    completed: u64,
    completed_out_of_order: std::collections::BTreeSet<u64>,
    attempts: std::collections::HashMap<String, u64>,
    review: Option<LearningReviewLease>,
    seals: std::collections::HashMap<String, String>,
}

struct LearningReviewLease {
    token: String,
    issued: u64,
    until: std::time::Instant,
}

pub(crate) struct LearningReviewFence {
    token: String,
    issued: u64,
}

const LEARNING_REVIEW_LEASE_MS: u64 = 30_000;

impl ExecuteSessionRegistry {
    /// Must succeed before effects begin. There is deliberately no expiry: a
    /// missing completion remains unknown until the execute session is deleted.
    pub(crate) fn begin_learning_execution<'a>(
        &'a self,
        prompt_hash: &'a str,
        session_id: &'a str,
        attempt: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<u64, ExecuteSessionPersistError>> {
        Box::pin(async move {
            let key = session_key(prompt_hash, session_id);
            let test_backend = self.test_json().await;
            if let Some(redis) = self.redis().await.filter(|_| test_backend.is_none()) {
                return redis
                    .begin_learning_execution(&key, attempt)
                    .await
                    .map_err(|error| ExecuteSessionPersistError::DurableWrite(Arc::new(error)));
            }
            if self.load_json(&key).await.is_none() {
                return Err(ExecuteSessionPersistError::SessionUnavailable);
            }
            let mut watermarks = self.learning_watermarks.write().await;
            let watermark = watermarks.entry(key).or_default();
            if watermark
                .review
                .as_ref()
                .is_some_and(|review| review.until > std::time::Instant::now())
            {
                return Err(ExecuteSessionPersistError::LearningReviewUnavailable);
            }
            if let Some(token) = watermark.attempts.get(attempt) {
                return Ok(*token);
            }
            let token = watermark
                .issued
                .checked_add(1)
                .filter(|token| *token <= 9_007_199_254_740_991)
                .ok_or(ExecuteSessionPersistError::ExposureConflict)?;
            watermark.issued = token;
            watermark.attempts.insert(attempt.to_owned(), token);
            Ok(token)
        })
    }

    /// Retain this attempt's completion even when earlier attempts are pending.
    /// Return whether all issued attempts have completed; duplicate completion
    /// cannot acknowledge any later attempt.
    pub(crate) fn complete_learning_execution<'a>(
        &'a self,
        prompt_hash: &'a str,
        session_id: &'a str,
        token: u64,
    ) -> futures_util::future::BoxFuture<'a, Result<bool, ExecuteSessionPersistError>> {
        Box::pin(async move {
            let key = session_key(prompt_hash, session_id);
            let test_backend = self.test_json().await;
            if let Some(redis) = self.redis().await.filter(|_| test_backend.is_none()) {
                return redis
                    .complete_learning_execution(&key, token)
                    .await
                    .map_err(|error| ExecuteSessionPersistError::DurableWrite(Arc::new(error)));
            }
            let mut watermarks = self.learning_watermarks.write().await;
            let Some(watermark) = watermarks.get_mut(&key) else {
                return Ok(false);
            };
            if token == 0 || token > watermark.issued {
                return Ok(false);
            }
            if token > watermark.completed {
                watermark.completed_out_of_order.insert(token);
                while watermark.completed < watermark.issued
                    && watermark
                        .completed_out_of_order
                        .remove(&(watermark.completed + 1))
                {
                    watermark.completed += 1;
                }
            }
            Ok(watermark.completed == watermark.issued)
        })
    }

    #[cfg(test)]
    pub(crate) fn learning_evidence_complete<'a>(
        &'a self,
        prompt_hash: &'a str,
        session_id: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<bool, ExecuteSessionPersistError>> {
        Box::pin(async move {
            let key = session_key(prompt_hash, session_id);
            let test_backend = self.test_json().await;
            if let Some(redis) = self.redis().await.filter(|_| test_backend.is_none()) {
                return redis
                    .learning_evidence_complete(&key)
                    .await
                    .map_err(|error| ExecuteSessionPersistError::DurableWrite(Arc::new(error)));
            }
            Ok(self
                .learning_watermarks
                .read()
                .await
                .get(&key)
                .is_some_and(|watermark| {
                    watermark.issued > 0 && watermark.issued == watermark.completed
                }))
        })
    }

    pub(crate) fn begin_learning_review<'a>(
        &'a self,
        prompt_hash: &'a str,
        session_id: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<LearningReviewFence, ExecuteSessionPersistError>>
    {
        Box::pin(async move {
            let key = session_key(prompt_hash, session_id);
            let token = uuid::Uuid::new_v4().to_string();
            let test_backend = self.test_json().await;
            if let Some(redis) = self.redis().await.filter(|_| test_backend.is_none()) {
                let issued = redis
                    .begin_learning_review(&key, &token, LEARNING_REVIEW_LEASE_MS)
                    .await
                    .map_err(|error| ExecuteSessionPersistError::DurableWrite(Arc::new(error)))?
                    .ok_or(ExecuteSessionPersistError::LearningReviewUnavailable)?;
                return Ok(LearningReviewFence { token, issued });
            }
            if self.load_json(&key).await.is_none() {
                return Err(ExecuteSessionPersistError::SessionUnavailable);
            }
            let mut watermarks = self.learning_watermarks.write().await;
            let watermark = watermarks
                .get_mut(&key)
                .ok_or(ExecuteSessionPersistError::LearningReviewUnavailable)?;
            let now = std::time::Instant::now();
            if watermark.issued == 0
                || watermark.issued != watermark.completed
                || watermark
                    .review
                    .as_ref()
                    .is_some_and(|review| review.until > now)
            {
                return Err(ExecuteSessionPersistError::LearningReviewUnavailable);
            }
            let issued = watermark.issued;
            watermark.review = Some(LearningReviewLease {
                token: token.clone(),
                issued,
                until: now + std::time::Duration::from_millis(LEARNING_REVIEW_LEASE_MS),
            });
            Ok(LearningReviewFence { token, issued })
        })
    }

    pub(crate) fn seal_learning_review<'a>(
        &'a self,
        prompt_hash: &'a str,
        session_id: &'a str,
        fence: &'a LearningReviewFence,
        review_id: &'a str,
        digest: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<bool, ExecuteSessionPersistError>> {
        Box::pin(async move {
            let key = session_key(prompt_hash, session_id);
            let test_backend = self.test_json().await;
            if let Some(redis) = self.redis().await.filter(|_| test_backend.is_none()) {
                return redis
                    .seal_learning_review(&key, &fence.token, fence.issued, review_id, digest)
                    .await
                    .map_err(|error| ExecuteSessionPersistError::DurableWrite(Arc::new(error)));
            }
            let mut watermarks = self.learning_watermarks.write().await;
            let Some(watermark) = watermarks.get_mut(&key) else {
                return Ok(false);
            };
            if watermark.issued != fence.issued
                || watermark.completed != fence.issued
                || !watermark.review.as_ref().is_some_and(|review| {
                    review.token == fence.token
                        && review.issued == fence.issued
                        && review.until > std::time::Instant::now()
                })
            {
                return Ok(false);
            }
            watermark
                .seals
                .insert(review_id.to_owned(), format!("{digest}:{}", fence.issued));
            watermark.review = None;
            Ok(true)
        })
    }

    pub(crate) fn abort_learning_review<'a>(
        &'a self,
        prompt_hash: &'a str,
        session_id: &'a str,
        fence: &'a LearningReviewFence,
    ) -> futures_util::future::BoxFuture<'a, Result<(), ExecuteSessionPersistError>> {
        Box::pin(async move {
            let key = session_key(prompt_hash, session_id);
            let test_backend = self.test_json().await;
            if let Some(redis) = self.redis().await.filter(|_| test_backend.is_none()) {
                return redis
                    .abort_learning_review(&key, &fence.token)
                    .await
                    .map_err(|error| ExecuteSessionPersistError::DurableWrite(Arc::new(error)));
            }
            if let Some(watermark) = self.learning_watermarks.write().await.get_mut(&key) {
                if watermark
                    .review
                    .as_ref()
                    .is_some_and(|review| review.token == fence.token)
                {
                    watermark.review = None;
                }
            }
            Ok(())
        })
    }

    pub(crate) async fn commit_credential_record(
        &self,
        memory: &crate::session_credentials::CredentialMemory,
        operation_key: &str,
        reference_key: &str,
        record: &plasm_runtime::credentials::StoredCredential,
    ) -> Result<
        plasm_runtime::credentials::StoredCredential,
        crate::session_credentials::CredentialPersistenceError,
    > {
        use crate::session_credentials::CredentialPersistenceError;
        use plasm_runtime::credentials::StoredCredential;
        let payload = serde_json::to_string(record)
            .map_err(|source| CredentialPersistenceError::RecordEncoding { source })?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let test = self.test_json().await;
        let raw = if let Some(redis) = self.redis().await.filter(|_| test.is_none()) {
            redis
                .commit_credential_json(
                    operation_key,
                    reference_key,
                    &payload,
                    now,
                    record.expires_at_unix.saturating_sub(now).max(1),
                )
                .await
                .map_err(|source| CredentialPersistenceError::DurableCommit { source })?
        } else {
            let storage = match test.as_ref() {
                Some(test) => test.as_ref(),
                None => &memory.0,
            };
            let mut data = storage.write().await;
            let previous = data
                .get(operation_key)
                .map(|raw| {
                    serde_json::from_str::<StoredCredential>(raw)
                        .map_err(|source| CredentialPersistenceError::StoredOperation { source })
                })
                .transpose()?;
            if let Some(previous) = previous.filter(|record| record.expires_at_unix > now) {
                return Ok(previous);
            }
            data.insert(operation_key.into(), payload.clone());
            data.insert(reference_key.into(), payload.clone());
            payload
        };
        serde_json::from_str(&raw)
            .map_err(|source| CredentialPersistenceError::CommitAcknowledgement { source })
    }

    pub(crate) async fn load_credential_record(
        &self,
        memory: &crate::session_credentials::CredentialMemory,
        key: &str,
    ) -> Result<
        Option<plasm_runtime::credentials::StoredCredential>,
        crate::session_credentials::CredentialPersistenceError,
    > {
        use crate::session_credentials::CredentialPersistenceError;
        let test = self.test_json().await;
        if let Some(redis) = self.redis().await.filter(|_| test.is_none()) {
            let raw = redis
                .get_string_strict(key)
                .await
                .map_err(|source| CredentialPersistenceError::DurableLookup { source })?;
            return raw
                .map(|raw| {
                    serde_json::from_str(&raw)
                        .map_err(|source| CredentialPersistenceError::StoredRecord { source })
                })
                .transpose();
        }
        let storage = match test.as_ref() {
            Some(test) => test.as_ref(),
            None => &memory.0,
        };
        let result = storage
            .read()
            .await
            .get(key)
            .map(|raw| {
                serde_json::from_str(raw)
                    .map_err(|source| CredentialPersistenceError::StoredRecord { source })
            })
            .transpose();
        result
    }

    /// Shared in-memory JSON store for cross-pod rehydrate tests (no Redis required).
    pub fn with_test_json_store() -> (Self, TestJsonStore) {
        let map: TestJsonStore = Arc::new(RwLock::new(std::collections::HashMap::new()));
        (
            Self {
                redis: Default::default(),
                test_json: Arc::new(RwLock::new(Some(map.clone()))),
                memory_descriptors: Default::default(),
                learning_watermarks: Default::default(),
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

    pub(crate) async fn write_descriptor_from_session(
        &self,
        session: &ExecuteSession,
        session_id: &str,
        reuse_key: &SessionReuseKey,
        durable: &crate::execute_session_materialize::DurableExposureSnapshot,
    ) -> Result<ExecuteSessionPersistOutcome, ExecuteSessionPersistError> {
        let desc = PersistedExecuteSessionDescriptor::from_session_and_durable_snapshot(
            session, session_id, reuse_key, durable,
        );
        let key = session_key(&desc.prompt_hash, &desc.session_id);
        self.write_descriptor(&key, &desc).await
    }

    /// Build rematerialized exposure snapshot and write descriptor (bind-credentials upsert path).
    pub(crate) async fn build_exposure_and_write_descriptor(
        &self,
        st: &PlasmHostState,
        session: &ExecuteSession,
        session_id: &str,
        reuse_key: &SessionReuseKey,
    ) -> Result<ExecuteSessionPersistOutcome, ExecuteSessionPersistError> {
        let exposure =
            crate::execute_session_materialize::build_durable_exposure_snapshot(st, session)
                .await?;
        self.write_descriptor_from_session(session, session_id, reuse_key, &exposure)
            .await
    }

    pub(crate) async fn persist(
        &self,
        session: &ExecuteSession,
        session_id: &str,
        reuse_key: &SessionReuseKey,
        exposure: &crate::execute_session_materialize::DurableExposureSnapshot,
    ) -> Result<ExecuteSessionPersistOutcome, ExecuteSessionPersistError> {
        self.write_descriptor_from_session(session, session_id, reuse_key, exposure)
            .await
    }

    /// Refresh descriptor after in-session mutation (federate/expand/plan commit).
    /// When no durable row exists yet, `reuse_key_fallback` seeds the first upsert.
    pub(crate) async fn persist_or_update(
        &self,
        _st: &PlasmHostState,
        session: &ExecuteSession,
        session_id: &str,
        reuse_key_fallback: Option<&SessionReuseKey>,
        exposure: &crate::execute_session_materialize::DurableExposureSnapshot,
    ) -> Result<ExecuteSessionPersistOutcome, ExecuteSessionPersistError> {
        use crate::domain_revision::{compare_exposure, DomainRevision, ExposureSync};

        let durable_backend = self.durable_backend_configured().await;
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
        } else if durable_backend {
            return Err(ExecuteSessionPersistError::MissingReuseKey);
        } else {
            return Ok(ExecuteSessionPersistOutcome::InMemoryOnly);
        };
        self.write_descriptor_from_session(session, session_id, &reuse_key, exposure)
            .await
    }

    /// Patch only `plan_commits` / `plan_commit_next` on the durable row when exposure revisions
    /// match. When hot is ahead, [`ExposurePersistPolicy`] chooses plan-only vs reuse-pin sync vs
    /// full rematerialize.
    pub async fn patch_plan_commits_only(
        &self,
        st: &PlasmHostState,
        session: &ExecuteSession,
        session_id: &str,
        reuse_key_fallback: Option<&SessionReuseKey>,
    ) -> Result<ExecuteSessionPersistOutcome, ExecuteSessionPersistError> {
        use crate::domain_revision::{
            plan_commit_persist_policy, DomainRevision, ExposurePersistPolicy,
            PlanCommitMaterializationInputs,
        };

        let durable_backend = self.durable_backend_configured().await;
        let key = session_key(&session.prompt_hash, session_id);
        let snapshot = session.snapshot_plan_commits_for_persist();
        if let Some(existing) = self.load_json(&key).await {
            let context_ids: Vec<String> = session.contexts_by_entry.keys().cloned().collect();
            let policy = plan_commit_persist_policy(
                DomainRevision::new(session.domain_revision),
                DomainRevision::new(existing.domain_revision),
                PlanCommitMaterializationInputs {
                    session_context_entry_ids: &context_ids,
                    durable_context_entry_ids: &existing.context_entry_ids,
                    session_outbound: &session.materialized_outbound_hosted_kv_by_entry,
                    durable_outbound: &existing.outbound_hosted_kv_by_entry,
                    session_bindings_len: session.bindings_by_entry.len(),
                    durable_bindings_len: existing.bindings_by_entry.len(),
                },
            );
            return match policy {
                ExposurePersistPolicy::RefuseHotBehind {
                    hot_revision,
                    durable_revision,
                } => Err(ExecuteSessionPersistError::HotBehindDurable {
                    hot_revision,
                    durable_revision,
                }),
                ExposurePersistPolicy::PatchMetadataOnly => {
                    let mut existing = existing;
                    existing.plan_commits = snapshot.records;
                    existing.plan_commit_next = snapshot.next_sequence;
                    existing.expires_at_unix = expires_at_from_now();
                    self.write_descriptor(&key, &existing).await
                }
                ExposurePersistPolicy::SyncHotExposureReusePins => {
                    let pins = crate::catalog_hash::effective_hashes_from_string_map(
                        &existing.catalog_cgs_hashes_by_entry,
                    );
                    let exposure =
                        crate::execute_session_materialize::build_durable_exposure_snapshot_reusing_pins(
                            session, pins,
                        )
                        .await?;
                    let reuse_key = existing.reuse_key.into();
                    self.write_descriptor_from_session(session, session_id, &reuse_key, &exposure)
                        .await
                }
                ExposurePersistPolicy::FullPersistMaterialize => {
                    let exposure =
                        crate::execute_session_materialize::build_durable_exposure_snapshot(
                            st, session,
                        )
                        .await?;
                    self.persist_or_update(st, session, session_id, reuse_key_fallback, &exposure)
                        .await
                }
            };
        }
        if !durable_backend {
            return Ok(ExecuteSessionPersistOutcome::InMemoryOnly);
        }
        let Some(fallback) = reuse_key_fallback else {
            return Err(ExecuteSessionPersistError::MissingReuseKey);
        };
        self.build_exposure_and_write_descriptor(st, session, session_id, fallback)
            .await
    }

    fn write_descriptor<'a>(
        &'a self,
        key: &'a str,
        desc: &'a PersistedExecuteSessionDescriptor,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<ExecuteSessionPersistOutcome, ExecuteSessionPersistError>,
    > {
        Box::pin(async move {
            if desc.symbol_ledger_bytes.is_empty() {
                return Err(ExecuteSessionPersistError::MissingSymbolLedger);
            }
            let payload = serde_json::to_string(desc)
                .map_err(|error| ExecuteSessionPersistError::Encoding(Arc::new(error)))?;
            if let Some(map) = self.test_json().await {
                let mut map = map.write().await;
                if let Some(previous) = map.get(key) {
                    let previous: PersistedExecuteSessionDescriptor =
                        serde_json::from_str(previous).map_err(|error| {
                            ExecuteSessionPersistError::Encoding(Arc::new(error))
                        })?;
                    validate_exposure_successor(
                        previous.domain_revision,
                        &previous.symbol_ledger_bytes,
                        desc,
                    )?;
                    let mut merged = desc.clone();
                    for (id, receipt) in previous.example_fetch_receipts {
                        merged.example_fetch_receipts.entry(id).or_insert(receipt);
                    }
                    let payload = serde_json::to_string(&merged)
                        .map_err(|error| ExecuteSessionPersistError::Encoding(Arc::new(error)))?;
                    map.insert(key.to_string(), payload);
                    return Ok(ExecuteSessionPersistOutcome::Durable);
                }
                map.insert(key.to_string(), payload);
                return Ok(ExecuteSessionPersistOutcome::Durable);
            }
            let Some(redis) = self.redis().await else {
                let mut descriptors = self.memory_descriptors.write().await;
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|time| time.as_secs())
                    .unwrap_or(0);
                descriptors.retain(|_, descriptor| descriptor.expires_at_unix > now);
                let mut merged = desc.clone();
                if let Some(previous) = descriptors.get(key) {
                    validate_exposure_successor(
                        previous.domain_revision,
                        &previous.symbol_ledger_bytes,
                        desc,
                    )?;
                    for (id, receipt) in &previous.example_fetch_receipts {
                        merged
                            .example_fetch_receipts
                            .entry(*id)
                            .or_insert_with(|| receipt.clone());
                    }
                }
                descriptors.insert(key.to_owned(), merged);
                return Ok(ExecuteSessionPersistOutcome::InMemoryOnly);
            };
            let ledger_key = desc
                .reuse_key
                .logical_session_id
                .as_deref()
                .map(|id| format!("mcp:execute:symbol_ledger:{id}"));
            if !redis
                .commit_session_exposure(
                    key,
                    ledger_key.as_deref(),
                    &payload,
                    &desc.symbol_ledger_bytes,
                )
                .await
                .map_err(|error| ExecuteSessionPersistError::DurableWrite(Arc::new(error)))?
            {
                return Err(ExecuteSessionPersistError::ExposureConflict);
            }
            Ok(ExecuteSessionPersistOutcome::Durable)
        })
    }

    async fn load_json(&self, key: &str) -> Option<PersistedExecuteSessionDescriptor> {
        if let Some(map) = self.test_json().await {
            let raw = map.read().await.get(key).cloned()?;
            return serde_json::from_str(&raw).ok();
        }
        if let Some(redis) = self.redis().await {
            return redis.get_json(key).await;
        }
        self.memory_descriptors.read().await.get(key).cloned()
    }

    pub(crate) async fn example_receipt(
        &self,
        prompt_hash: &str,
        session_id: &str,
        id: uuid::Uuid,
    ) -> Result<Option<crate::workflow_learning::serving::ServedExample>, ExecuteSessionPersistError>
    {
        let key = session_key(prompt_hash, session_id);
        if let Some(redis) = self.redis().await {
            let Some(raw) = redis
                .get_string_strict(&format!("{key}:example_receipts"))
                .await
                .map_err(|error| ExecuteSessionPersistError::DurableWrite(Arc::new(error)))?
            else {
                return Ok(None);
            };
            let receipts: std::collections::BTreeMap<uuid::Uuid, String> =
                serde_json::from_str(&raw)
                    .map_err(|error| ExecuteSessionPersistError::Encoding(Arc::new(error)))?;
            return receipts
                .get(&id)
                .map(|receipt| {
                    serde_json::from_str(receipt)
                        .map_err(|error| ExecuteSessionPersistError::Encoding(Arc::new(error)))
                })
                .transpose();
        }
        if let Some(descriptor) = self.load_json(&key).await {
            return Ok(descriptor.example_fetch_receipts.get(&id).cloned());
        }
        Ok(None)
    }

    /// Strict metadata-only read for optional example fetch. Never hydrate a
    /// catalogue or interpret an unavailable Redis backend as a missing row.
    pub(crate) async fn load_for_example(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Result<Option<PersistedExecuteSessionDescriptor>, ExecuteSessionPersistError> {
        let key = session_key(prompt_hash, session_id);
        let raw = if let Some(map) = self.test_json().await {
            map.read().await.get(&key).cloned()
        } else if let Some(redis) = self.redis().await {
            redis
                .get_string_strict(&key)
                .await
                .map_err(|error| ExecuteSessionPersistError::DurableWrite(Arc::new(error)))?
        } else {
            return Ok(self.memory_descriptors.read().await.get(&key).cloned());
        };
        raw.map(|raw| {
            serde_json::from_str(&raw)
                .map_err(|error| ExecuteSessionPersistError::Encoding(Arc::new(error)))
        })
        .transpose()
    }

    pub(crate) async fn persist_example_fetch(
        &self,
        session: &ExecuteSession,
        session_id: &str,
        reuse_key: &SessionReuseKey,
        exposure: &crate::execute_session_materialize::DurableExposureSnapshot,
        receipt: crate::workflow_learning::serving::ServedExample,
    ) -> Result<(), ExecuteSessionPersistError> {
        let mut descriptor = PersistedExecuteSessionDescriptor::from_session_and_durable_snapshot(
            session, session_id, reuse_key, exposure,
        );
        descriptor
            .example_fetch_receipts
            .insert(receipt.example_id, receipt);
        self.write_descriptor(&session_key(&session.prompt_hash, session_id), &descriptor)
            .await?;
        Ok(())
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
                redis.touch(&format!("{key}:example_receipts")).await;
                if let Some(logical_id) = &desc.reuse_key.logical_session_id {
                    redis
                        .touch(&format!("mcp:execute:symbol_ledger:{logical_id}"))
                        .await;
                }
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
        MergeLiveOutcome::Merged
    }

    pub(crate) async fn delete(&self, prompt_hash: &str, session_id: &str) {
        let key = session_key(prompt_hash, session_id);
        self.memory_descriptors.write().await.remove(&key);
        if let Some(map) = self.test_json().await {
            map.write().await.remove(&key);
        }
        self.learning_watermarks.write().await.remove(&key);
        if let Some(redis) = self.redis().await {
            if let Err(error) = redis.delete_session_publication(&key).await {
                tracing::warn!(%error, "execute descriptor and evidence watermark deletion failed atomically");
            }
        }
    }

    /// Drop all cross-pod execute session descriptors (e.g. after catalog-dir reload).
    pub async fn purge_redis(&self) -> u64 {
        self.memory_descriptors.write().await.clear();
        if let Some(map) = self.test_json().await {
            map.write().await.clear();
        }
        self.learning_watermarks.write().await.clear();
        if let Some(redis) = self.redis().await {
            redis.purge_session_publications(SESSION_KEY_PREFIX).await
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
        if let Err(error) = self.write_descriptor(&key, &desc).await {
            tracing::warn!(%error, "operation metadata publication conflicted with session exposure");
        }
    }
}

#[cfg(test)]
mod exposure_publication_tests {
    use super::validate_exposure_version;

    #[test]
    fn competing_allocations_at_same_revision_are_rejected() {
        assert!(validate_exposure_version(4, b"first allocation", 4, b"other allocation").is_err());
        assert!(validate_exposure_version(4, b"first allocation", 4, b"first allocation").is_ok());
    }

    #[test]
    fn exposure_publication_requires_direct_successor() {
        assert!(validate_exposure_version(4, b"old", 3, b"stale").is_err());
        assert!(validate_exposure_version(4, b"old", 6, b"skipped").is_err());
        assert!(validate_exposure_version(4, b"old", 6, b"old").is_ok());
        assert!(validate_exposure_version(4, b"old", 5, b"new").is_ok());
    }

    #[tokio::test]
    async fn redis_publication_is_atomic_and_rejects_competing_allocations() {
        let Ok(url) = std::env::var("PLASM_TEST_REDIS_URL") else {
            return;
        };
        let redis = super::RedisBackend::connect(&url, std::time::Duration::from_secs(60))
            .await
            .expect("redis");
        let suffix = uuid::Uuid::new_v4();
        let session = format!("test:workflow:session:{suffix}");
        let ledger = format!("test:workflow:ledger:{suffix}");
        assert!(redis
            .commit_session_exposure(
                &session,
                Some(&ledger),
                r#"{"domain_revision":1,"symbol_ledger_bytes":[1]}"#,
                &[1]
            )
            .await
            .expect("first"));
        assert!(!redis
            .commit_session_exposure(
                &session,
                Some(&ledger),
                r#"{"domain_revision":1,"symbol_ledger_bytes":[2]}"#,
                &[2]
            )
            .await
            .expect("conflict"));
        assert_eq!(redis.get_bytes(&ledger).await, Some(vec![1]));
        assert!(redis
            .commit_session_exposure(
                &session,
                Some(&ledger),
                r#"{"domain_revision":2,"symbol_ledger_bytes":[2]}"#,
                &[2]
            )
            .await
            .expect("successor"));
        assert_eq!(redis.get_bytes(&ledger).await, Some(vec![2]));
        redis.delete(&session).await;
        redis.delete(&format!("{session}:example_receipts")).await;
        redis.delete(&ledger).await;
    }

    #[tokio::test]
    async fn memory_learning_watermark_tracks_both_completion_orders_and_missing_attempts() {
        for order in [[1, 2], [2, 1]] {
            let (registry, descriptors) = super::ExecuteSessionRegistry::with_test_json_store();
            let descriptor = serde_json::json!({
                "prompt_hash":"workflow", "session_id":"session", "prompt_text":"",
                "python_teaching":plasm_core::prompt_render::python::PythonTeachingState::default(),
                "entry_id":"matrix", "context_entry_ids":[], "entities":[],
                "tenant_scope":"tenant", "principal_subject":"principal",
                "catalog_cgs_hash":"fixture", "domain_revision":1,
                "reuse_key":{"tenant_scope":"tenant", "entry_id":"matrix",
                    "catalog_cgs_hash":"fixture", "entities":[]}
            });
            descriptors.write().await.insert(
                super::session_key("workflow", "session"),
                descriptor.to_string(),
            );
            assert_eq!(
                registry
                    .begin_learning_execution("workflow", "session", "first")
                    .await
                    .unwrap(),
                1
            );
            assert_eq!(
                registry
                    .begin_learning_execution("workflow", "session", "second")
                    .await
                    .unwrap(),
                2
            );
            assert!(!registry
                .complete_learning_execution("workflow", "session", 0)
                .await
                .unwrap());
            assert!(!registry
                .complete_learning_execution("workflow", "session", 3)
                .await
                .unwrap());
            assert!(!registry
                .complete_learning_execution("workflow", "session", order[0])
                .await
                .unwrap());
            assert!(!registry
                .learning_evidence_complete("workflow", "session")
                .await
                .unwrap());
            assert!(registry
                .complete_learning_execution("workflow", "session", order[1])
                .await
                .unwrap());
            assert!(registry
                .learning_evidence_complete("workflow", "session")
                .await
                .unwrap());
            let fence = registry
                .begin_learning_review("workflow", "session")
                .await
                .unwrap();
            assert!(registry
                .begin_learning_execution("workflow", "session", "blocked")
                .await
                .is_err());
            assert!(registry
                .seal_learning_review("workflow", "session", &fence, "review", "digest")
                .await
                .unwrap());
            assert_eq!(
                registry
                    .begin_learning_execution("workflow", "session", "third")
                    .await
                    .unwrap(),
                3
            );
            assert!(!registry
                .complete_learning_execution("workflow", "session", 1)
                .await
                .unwrap());
            assert!(!registry
                .learning_evidence_complete("workflow", "session")
                .await
                .unwrap());
            assert!(registry
                .complete_learning_execution("workflow", "session", 3)
                .await
                .unwrap());
        }
    }

    #[tokio::test]
    async fn redis_learning_watermark_tracks_both_completion_orders_and_review_gate() {
        let Ok(url) = std::env::var("PLASM_TEST_REDIS_URL") else {
            return;
        };
        let backend = super::RedisBackend::connect(&url, std::time::Duration::from_secs(60))
            .await
            .unwrap();
        for reverse in [false, true] {
            let key = format!("learning-overlap-{}", uuid::Uuid::new_v4());
            backend
                .commit_session_exposure(
                    &key,
                    None,
                    r#"{"domain_revision":1,"symbol_ledger_bytes":[1]}"#,
                    &[1],
                )
                .await
                .unwrap();
            let first = backend
                .begin_learning_execution(&key, "first")
                .await
                .unwrap();
            let second = backend
                .begin_learning_execution(&key, "second")
                .await
                .unwrap();
            assert_eq!(
                backend
                    .begin_learning_execution(&key, "first")
                    .await
                    .unwrap(),
                first
            );
            let order = if reverse {
                [second, first]
            } else {
                [first, second]
            };
            assert!(!backend.complete_learning_execution(&key, 0).await.unwrap());
            assert!(!backend
                .complete_learning_execution(&key, second + 1)
                .await
                .unwrap());
            assert!(!backend
                .complete_learning_execution(&key, order[0])
                .await
                .unwrap());
            assert!(backend
                .begin_learning_review(&key, "incomplete", 30_000)
                .await
                .unwrap()
                .is_none());
            assert!(backend
                .complete_learning_execution(&key, order[1])
                .await
                .unwrap());
            assert_eq!(
                backend
                    .begin_learning_review(&key, "review", 30_000)
                    .await
                    .unwrap(),
                Some(second)
            );
            assert!(backend
                .begin_learning_execution(&key, "blocked")
                .await
                .is_err());
            assert!(backend
                .seal_learning_review(&key, "review", second, "review-id", "digest")
                .await
                .unwrap());
            let third = backend
                .begin_learning_execution(&key, "third")
                .await
                .unwrap();
            assert!(!backend
                .complete_learning_execution(&key, first)
                .await
                .unwrap());
            assert!(backend
                .begin_learning_review(&key, "still-incomplete", 30_000)
                .await
                .unwrap()
                .is_none());
            assert!(backend
                .complete_learning_execution(&key, third)
                .await
                .unwrap());
            backend.delete_session_publication(&key).await.unwrap();
        }
    }

    #[tokio::test]
    async fn redis_learning_watermark_blocks_other_replica_after_missing_completion() {
        let Ok(url) = std::env::var("PLASM_TEST_REDIS_URL") else {
            return;
        };
        let backend = std::sync::Arc::new(
            super::RedisBackend::connect(&url, std::time::Duration::from_secs(60))
                .await
                .expect("redis"),
        );
        let first = super::ExecuteSessionRegistry::new_in_memory();
        first.attach_redis(backend.clone()).await;
        let second = super::ExecuteSessionRegistry::new_in_memory();
        second
            .attach_redis(std::sync::Arc::new(
                super::RedisBackend::connect(&url, std::time::Duration::from_secs(60))
                    .await
                    .expect("other replica"),
            ))
            .await;
        let ph = format!("watermark-{}", uuid::Uuid::new_v4());
        let sid = uuid::Uuid::new_v4().to_string();
        let key = super::session_key(&ph, &sid);
        backend
            .commit_session_exposure(
                &key,
                None,
                r#"{"domain_revision":1,"symbol_ledger_bytes":[1]}"#,
                &[1],
            )
            .await
            .expect("session");
        assert!(!second
            .learning_evidence_complete(&ph, &sid)
            .await
            .expect("missing"));
        let token = first
            .begin_learning_execution(&ph, &sid, "verified")
            .await
            .expect("begin");
        assert_eq!(
            first
                .begin_learning_execution(&ph, &sid, "verified")
                .await
                .expect("retry"),
            token
        );
        assert!(first
            .complete_learning_execution(&ph, &sid, token)
            .await
            .expect("complete"));
        assert!(second
            .learning_evidence_complete(&ph, &sid)
            .await
            .expect("verified"));
        let fence = first.begin_learning_review(&ph, &sid).await.expect("gate");
        assert!(second
            .begin_learning_execution(&ph, &sid, "blocked")
            .await
            .is_err());
        first
            .abort_learning_review(&ph, &sid, &fence)
            .await
            .expect("abort");
        let newer = second
            .begin_learning_review(&ph, &sid)
            .await
            .expect("new fence");
        assert!(!first
            .seal_learning_review(&ph, &sid, &fence, "old", "digest")
            .await
            .expect("stale fence"));
        assert!(second
            .seal_learning_review(&ph, &sid, &newer, "sealed", "digest")
            .await
            .expect("seal"));
        assert!(backend
            .begin_learning_review(&key, "expires", 1)
            .await
            .expect("short lease")
            .is_some());
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        assert!(!backend
            .seal_learning_review(&key, "expires", token, "expired", "digest")
            .await
            .expect("expired fence"));
        first
            .begin_learning_execution(&ph, &sid, "journal-unavailable")
            .await
            .expect("begin before effects");
        // The failed PostgreSQL journal never completes its durable watermark.
        assert!(!second
            .learning_evidence_complete(&ph, &sid)
            .await
            .expect("other replica blocked"));
        let later = second
            .begin_learning_execution(&ph, &sid, "later-success")
            .await
            .expect("later begin");
        assert!(!second
            .complete_learning_execution(&ph, &sid, later)
            .await
            .expect("gap remains"));
        assert!(!first
            .learning_evidence_complete(&ph, &sid)
            .await
            .expect("still incomplete"));
        first.delete(&ph, &sid).await;
    }
}
