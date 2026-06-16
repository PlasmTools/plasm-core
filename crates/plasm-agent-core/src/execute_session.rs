//! In-memory execute sessions: prompt text + CGS + entity seeds, keyed by `(prompt_hash, session_id)`.
//! Plasm instructions text is built incrementally via [`plasm_core::TeachingExposureSession`] (monotonic `e#`/`m#`/`p#`/`r#`).

pub use crate::graph_cache_guard::GraphCacheGuard;

use indexmap::IndexMap;
use plasm_core::CgsContext;
use plasm_core::FederationDispatch;
use plasm_core::OperationHandle;
use plasm_core::PagingHandle;
use plasm_core::PlanCommitRef;
use plasm_core::TeachingExposureSession;
use plasm_core::CGS;
use plasm_plugin_host::LoadedPluginGeneration;
use plasm_runtime::{
    CachedEntity, GraphCache, MutexGraphCacheSession, QueryPaginationResumeData, ViewAmbientContext,
};
use std::collections::{HashMap, VecDeque};
use std::env;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::{sleep, Duration as TokioDuration};

use crate::execute_path_ids::{ExecuteSessionId, PromptHashHex};
use crate::run_artifacts::{ArtifactPayload, RunArtifactId, RunArtifactStore};
use crate::session_graph_persistence::SessionGraphPersistence;
use serde::Serialize;

#[path = "execute_session_operations.rs"]
mod operations;

/// Default cap on concurrent `Running` async operations per execute session.
pub const DEFAULT_MAX_RUNNING_OPS_PER_SESSION: usize = 16;

/// `PLASM_MAX_RUNNING_OPS_PER_SESSION` — max concurrent async ops per session (default 16).
pub fn max_running_ops_per_session() -> usize {
    env::var("PLASM_MAX_RUNNING_OPS_PER_SESSION")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_RUNNING_OPS_PER_SESSION)
}

fn running_handles_from_map(
    map: &HashMap<OperationHandle, crate::operation::OperationState>,
) -> Vec<OperationHandle> {
    operations::running_handles_from_map(map)
}

fn format_too_many_operations_error(handles: &[OperationHandle], cap: usize) -> String {
    operations::format_too_many_operations_error(handles, cap)
}

/// Default time-to-live for a session (lazy expiry on lookup).
const SESSION_TTL: Duration = Duration::from_secs(3600);

/// Shared in-memory + Redis descriptor expiry (seconds).
pub fn session_ttl_secs() -> u64 {
    SESSION_TTL.as_secs()
}

/// Environment key: max run snapshots retained per session in RAM after archive write (default 256).
pub const ENV_RUN_ARTIFACT_HOT_CACHE_MAX_RUNS: &str = "PLASM_RUN_ARTIFACT_HOT_CACHE_MAX_RUNS";
/// Environment key: optional byte budget for the per-session hot cache (0 = use run count only).
pub const ENV_RUN_ARTIFACT_HOT_CACHE_MAX_BYTES: &str = "PLASM_RUN_ARTIFACT_HOT_CACHE_MAX_BYTES";

/// Bounds for the in-process FIFO working set of run snapshots (after each run is persisted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunArtifactHotCacheBounds {
    pub max_runs: usize,
    /// When > 0, evict until `approx_bytes <= max_bytes` (keeps at least one entry when possible).
    pub max_bytes: usize,
}

impl Default for RunArtifactHotCacheBounds {
    fn default() -> Self {
        Self {
            max_runs: 256,
            max_bytes: 0,
        }
    }
}

impl RunArtifactHotCacheBounds {
    /// Merge optional environment variables onto [`RunArtifactHotCacheBounds::default`].
    pub fn from_env() -> Self {
        let mut b = Self::default();
        if let Some(n) = positive_env_usize(ENV_RUN_ARTIFACT_HOT_CACHE_MAX_RUNS) {
            b.max_runs = n.max(1);
        }
        if let Some(n) = env::var(ENV_RUN_ARTIFACT_HOT_CACHE_MAX_BYTES)
            .ok()
            .and_then(|raw| {
                let t = raw.trim();
                if t.is_empty() {
                    return None;
                }
                t.parse::<usize>().ok()
            })
        {
            b.max_bytes = n;
        }
        b
    }
}

fn positive_env_usize(key: &str) -> Option<usize> {
    env::var(key).ok().and_then(|raw| {
        let t = raw.trim();
        if t.is_empty() {
            return None;
        }
        match t.parse::<usize>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => None,
        }
    })
}

fn run_artifact_hot_bounds() -> RunArtifactHotCacheBounds {
    static B: OnceLock<RunArtifactHotCacheBounds> = OnceLock::new();
    *B.get_or_init(RunArtifactHotCacheBounds::from_env)
}

/// FIFO-bounded working set for [`SessionCore`]: newest runs stay; oldest evicted first.
#[derive(Debug)]
struct RunArtifactHotCache {
    bounds: RunArtifactHotCacheBounds,
    order: VecDeque<RunArtifactId>,
    map: HashMap<RunArtifactId, Arc<SessionRunArtifact>>,
    approx_bytes: usize,
}

impl RunArtifactHotCache {
    fn new(bounds: RunArtifactHotCacheBounds) -> Self {
        Self {
            bounds,
            order: VecDeque::new(),
            map: HashMap::new(),
            approx_bytes: 0,
        }
    }

    fn insert(
        &mut self,
        run_id: RunArtifactId,
        epoch: GraphEpoch,
        resource_index: u64,
        seq: DeltaSeq,
        payload: ArtifactPayload,
    ) -> (Arc<SessionRunArtifact>, u64) {
        let item = Arc::new(SessionRunArtifact {
            run_id,
            resource_index,
            seq,
            epoch,
            payload,
        });
        let add_bytes = item.payload.bytes.len();
        self.map.insert(run_id, item.clone());
        self.order.push_back(run_id);
        self.approx_bytes = self.approx_bytes.saturating_add(add_bytes);
        let evicted = self.evict_for_limits();
        (item, evicted)
    }

    fn evict_for_limits(&mut self) -> u64 {
        let mut evicted = 0u64;
        while self.map.len() > self.bounds.max_runs {
            evicted = evicted.saturating_add(self.evict_one());
        }
        if self.bounds.max_bytes > 0 {
            while self.approx_bytes > self.bounds.max_bytes && self.map.len() > 1 {
                evicted = evicted.saturating_add(self.evict_one());
            }
        }
        evicted
    }

    fn evict_one(&mut self) -> u64 {
        let Some(oldest) = self.order.pop_front() else {
            return 0;
        };
        let Some(removed) = self.map.remove(&oldest) else {
            return 0;
        };
        self.approx_bytes = self
            .approx_bytes
            .saturating_sub(removed.payload.bytes.len());
        1
    }

    fn get(&self, run_id: RunArtifactId) -> Option<Arc<SessionRunArtifact>> {
        self.map.get(&run_id).cloned()
    }

    fn get_by_resource_index(&self, resource_index: u64) -> Option<Arc<SessionRunArtifact>> {
        self.map
            .values()
            .find(|a| a.resource_index == resource_index)
            .cloned()
    }

    fn drain(&mut self) -> Vec<Arc<SessionRunArtifact>> {
        let mut out = Vec::with_capacity(self.map.len());
        for id in self.order.drain(..) {
            if let Some(a) = self.map.remove(&id) {
                out.push(a);
            }
        }
        self.approx_bytes = 0;
        out
    }

    fn requeue(&mut self, artifacts: Vec<Arc<SessionRunArtifact>>) {
        for item in artifacts {
            let id = item.run_id;
            let bytes = item.payload.bytes.len();
            self.map.insert(id, item);
            self.order.push_back(id);
            self.approx_bytes = self.approx_bytes.saturating_add(bytes);
            let _ = self.evict_for_limits();
        }
    }

    fn list_ordered_summaries(&self) -> Vec<SessionRunSummary> {
        self.order
            .iter()
            .filter_map(|id| {
                self.map.get(id).map(|a| SessionRunSummary {
                    run_id: a.run_id,
                    resource_index: a.resource_index,
                    delta_seq: a.seq.0,
                })
            })
            .collect()
    }
}

/// Key for deduplicating execute sessions: same registry `entry_id`, same entity seed set, and
/// (in delegated auth mode) the same [`ExecuteSession::principal`].
///
/// When set, [`Self::logical_session_id`] scopes reuse to one MCP agent logical session (distinct
/// from MCP transport `MCP-Session-Id`).
///
/// [`Self::context_intent`] participates in reuse when set (MCP `plasm_context`): distinct intents
/// must not share an execute row whose teaching symbols were filtered for a different wording.
///
/// When [`Self::context_intent`] is set, [`Self::ranked_capabilities`] participates in reuse: distinct
/// ranked gate lists must not share a session row filtered for different mutation picks.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct SessionReuseKey {
    /// Tenant scope from incoming auth (empty string when anonymous / auth off).
    pub tenant_scope: String,
    pub entry_id: String,
    /// Canonical digest of the pinned CGS (see [`plasm_core::schema::CGS::catalog_cgs_hash_hex`]).
    pub catalog_cgs_hash: String,
    /// Sorted, deduplicated entity names (same convention as HTTP/MCP bodies).
    pub entities: Vec<String>,
    /// Normalized first-open `plasm_context` intent when capability-scoped teaching table is active.
    pub context_intent: Option<String>,
    /// Sorted deduped capability wire names for ranked mutation gating when intent-scoped teaching table is active.
    pub ranked_capabilities: Option<Vec<String>>,
    /// Set when `PLASM_AUTH_RESOLUTION=delegated` so distinct users do not share a session.
    pub principal: Option<String>,
    /// Pinned compile-plugin generation when [`ExecuteSession::plugin_generation`] is set.
    pub plugin_generation_id: Option<u64>,
    /// MCP logical session UUID string (canonical); `None` for HTTP-only execute without a logical id.
    pub logical_session_id: Option<String>,
}

/// Monotonic sequence for per-session append-only run deltas.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct DeltaSeq(pub u64);

/// Coarse graph epoch marker for snapshot boundaries (mirrors `GraphCache` stats.version).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct GraphEpoch(pub u64);

#[derive(Clone, Debug)]
pub struct SessionRunArtifact {
    pub run_id: RunArtifactId,
    /// Monotonic per execute session; matches `RunArtifactDocument.resource_index` and `plasm://r/{n}`.
    pub resource_index: u64,
    pub seq: DeltaSeq,
    pub epoch: GraphEpoch,
    pub payload: ArtifactPayload,
}

/// In-memory run index row (`GET /execute/.../runs` hot cache; evicted runs may still exist in [`RunArtifactStore`]).
#[derive(Debug, Clone, Serialize)]
pub struct SessionRunSummary {
    pub run_id: RunArtifactId,
    pub resource_index: u64,
    pub delta_seq: u64,
}

#[derive(Clone, Debug)]
pub struct SyntheticPageCursor {
    pub node_id: String,
    pub entity_type: String,
    pub rows: Vec<CachedEntity>,
    pub offset: usize,
    pub page_size: usize,
    pub request_fingerprints: Vec<String>,
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum PagingResume {
    Query(QueryPaginationResumeData),
    Synthetic(SyntheticPageCursor),
}

#[derive(Debug)]
struct SessionCoreState {
    seq: DeltaSeq,
    run_artifacts: RunArtifactHotCache,
}

/// Shared active-session materialization core: graph + run artifacts + monotonic sequence.
pub struct SessionCore {
    graph_cache: Arc<MutexGraphCacheSession>,
    state: Mutex<SessionCoreState>,
}

impl SessionCore {
    pub fn new() -> Self {
        Self {
            graph_cache: Arc::new(MutexGraphCacheSession::new(GraphCache::new())),
            state: Mutex::new(SessionCoreState {
                seq: DeltaSeq::default(),
                run_artifacts: RunArtifactHotCache::new(run_artifact_hot_bounds()),
            }),
        }
    }
}

impl Default for SessionCore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionCore {
    pub fn graph_cache(&self) -> Arc<MutexGraphCacheSession> {
        self.graph_cache.clone()
    }

    pub async fn alloc_delta_seq(&self) -> DeltaSeq {
        let mut g = self.state.lock().await;
        g.seq.0 += 1;
        g.seq
    }

    pub async fn append_run_artifact(
        &self,
        run_id: RunArtifactId,
        epoch: GraphEpoch,
        resource_index: u64,
        payload: ArtifactPayload,
    ) -> Arc<SessionRunArtifact> {
        let seq = self.alloc_delta_seq().await;
        let mut g = self.state.lock().await;
        let (item, evicted) = g
            .run_artifacts
            .insert(run_id, epoch, resource_index, seq, payload);
        if evicted > 0 {
            crate::metrics::record_run_artifact_hot_cache_evictions(evicted);
        }
        item
    }

    pub async fn get_run_artifact(&self, run_id: RunArtifactId) -> Option<Arc<SessionRunArtifact>> {
        let g = self.state.lock().await;
        g.run_artifacts.get(run_id)
    }

    pub async fn get_run_artifact_by_resource_index(
        &self,
        resource_index: u64,
    ) -> Option<Arc<SessionRunArtifact>> {
        let g = self.state.lock().await;
        g.run_artifacts.get_by_resource_index(resource_index)
    }

    pub async fn drain_run_artifacts(&self) -> Vec<Arc<SessionRunArtifact>> {
        let mut g = self.state.lock().await;
        g.run_artifacts.drain()
    }

    pub async fn requeue_run_artifacts(&self, artifacts: Vec<Arc<SessionRunArtifact>>) {
        if artifacts.is_empty() {
            return;
        }
        let mut g = self.state.lock().await;
        g.run_artifacts.requeue(artifacts);
    }

    pub async fn tip_seq(&self) -> DeltaSeq {
        let g = self.state.lock().await;
        g.seq
    }

    /// Ordered run snapshots currently retained in the session hot cache (newest last).
    pub async fn list_run_summaries(&self) -> Vec<SessionRunSummary> {
        let g = self.state.lock().await;
        g.run_artifacts.list_ordered_summaries()
    }
}

#[derive(Clone)]
pub struct ExecuteSession {
    pub prompt_hash: String,
    pub prompt_text: String,
    pub cgs: Arc<CGS>,
    /// Loaded registry contexts keyed by `entry_id` (single entry for non-federated sessions).
    pub contexts_by_entry: IndexMap<String, Arc<CgsContext>>,
    pub entry_id: String,
    /// Incoming-auth tenant scope (empty when anonymous).
    pub tenant_scope: String,
    /// Principal subject from incoming auth (empty when anonymous).
    #[allow(dead_code)] // surfaced for future audit/logging and SaaS dashboards
    pub principal_subject: String,
    /// When set (registry entry `backend:`), HTTP execution uses this origin instead of global `--backend`.
    pub http_backend: Option<String>,
    /// Entity names exposed in this session (sorted at open; **cumulative** after incremental expand waves
    /// via [`crate::http_execute::expand_execute_teaching_session`], matching [`Self::teaching_exposure`].entities).
    pub entities: Vec<String>,
    /// Monotonic symbol map for incremental exposure + expression expand (exact seeds, expanded in waves).
    pub teaching_exposure: Option<TeachingExposureSession>,
    /// Increments on each successful [`expand_execute_teaching_session`] wave.
    pub domain_revision: u32,
    /// End-user / tenant id when using delegated credential resolution (`PLASM_AUTH_RESOLUTION=delegated`).
    pub principal: Option<String>,
    /// Pins [`plasm_plugin_host::LoadedPluginGeneration`] for compile overrides (hot-swap safe).
    pub plugin_generation: Option<Arc<LoadedPluginGeneration>>,
    /// Canonical digest of the pinned primary CGS at session open (effective, post-overlay).
    pub catalog_cgs_hash: String,
    /// Registry-base `catalog_cgs_hash_hex` per entry at open (before tenant/http/overlay patches).
    pub(crate) registry_catalog_hashes_by_entry: HashMap<String, String>,
    /// Normalized MCP `plasm_context` intent when teaching table uses intent-scoped capability exposure (`None` = legacy full closure).
    pub context_intent: Option<String>,
    /// Optional ranked capability-name gate for mutators (aligned with [`SessionReuseKey::ranked_capabilities`]).
    pub ranked_capabilities: Option<Vec<String>>,
    /// Share-link / instance token bound once per execute session (Bearer + optional `share_token` CML env).
    pub session_share_token: Arc<RwLock<Option<String>>>,
    /// Proof: `baseToken` from the latest successful `editor_state_get`; merged as `base_token` CML env for `/ops`.
    pub session_proof_base_token: Arc<RwLock<Option<String>>>,
    /// Per-session materialized graph; isolated from other execute sessions.
    pub graph_cache: Arc<MutexGraphCacheSession>,
    /// Unified in-session graph/artifact state.
    pub core: Arc<SessionCore>,
    /// Next `plasm://r/{n}` index for this execute session (1-based after first mint).
    run_resource_next: Arc<AtomicU64>,
    /// Opaque `pg#` handles → query or synthetic pagination resume snapshots for [`plasm_core::Expr::Page`].
    paging_resume_by_handle: Arc<StdMutex<HashMap<PagingHandle, PagingResume>>>,
    paging_handle_next: Arc<AtomicU64>,
    /// Serializes `page(pg#)` peek → execute → upsert so concurrent clients cannot corrupt continuation state.
    pub(crate) paging_op_lock: Arc<tokio::sync::Mutex<()>>,
    /// Opaque `sN_oM` handles → in-flight or terminal async plan runs ([`plasm_core::Expr::Wait`] / [`Cancel`](plasm_core::Expr::Cancel)).
    operation_by_handle: Arc<StdMutex<HashMap<OperationHandle, crate::operation::OperationState>>>,
    operation_handle_next: Arc<AtomicU64>,
    /// Cross-pod operation persist routing (`session_id`, host weak ref, started_at map).
    operation_wire: Arc<StdMutex<crate::operation_persist::OperationWireBinding>>,
    /// Dry-run plan acceptance tokens (`pcN`) for soft-gate live execute.
    plan_commits: Arc<StdMutex<HashMap<PlanCommitRef, crate::operation::PlanCommitRecord>>>,
    plan_commit_next: Arc<AtomicU64>,
    /// Hash-chained evidence for intent→comp→execute (`PLASM_EVIDENCE_CHAIN=1`); lazy slot.
    pub(crate) evidence_chain: crate::evidence_chain::EvidenceChainSlot,
    /// True while a synchronous (blocking) live plan run holds the execute session.
    sync_live_run_inflight: Arc<AtomicBool>,
    /// Per-catalog session binding maps (MCP connect / REPL `--backend`).
    pub bindings_by_entry: indexmap::IndexMap<String, crate::binding_slots::SessionBindingMap>,
}

impl ExecuteSession {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        prompt_hash: String,
        prompt_text: String,
        cgs: Arc<CGS>,
        contexts_by_entry: IndexMap<String, Arc<CgsContext>>,
        entry_id: String,
        tenant_scope: String,
        principal_subject: String,
        http_backend: Option<String>,
        entities: Vec<String>,
        teaching_exposure: Option<TeachingExposureSession>,
        principal: Option<String>,
        plugin_generation: Option<Arc<LoadedPluginGeneration>>,
        catalog_cgs_hash: String,
        context_intent: Option<String>,
        ranked_capabilities: Option<Vec<String>>,
    ) -> Self {
        Self::new_with_bindings(
            prompt_hash,
            prompt_text,
            cgs,
            contexts_by_entry,
            entry_id,
            tenant_scope,
            principal_subject,
            http_backend,
            entities,
            teaching_exposure,
            principal,
            plugin_generation,
            catalog_cgs_hash,
            context_intent,
            ranked_capabilities,
            IndexMap::new(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_bindings(
        prompt_hash: String,
        prompt_text: String,
        cgs: Arc<CGS>,
        contexts_by_entry: IndexMap<String, Arc<CgsContext>>,
        entry_id: String,
        tenant_scope: String,
        principal_subject: String,
        http_backend: Option<String>,
        entities: Vec<String>,
        teaching_exposure: Option<TeachingExposureSession>,
        principal: Option<String>,
        plugin_generation: Option<Arc<LoadedPluginGeneration>>,
        catalog_cgs_hash: String,
        context_intent: Option<String>,
        ranked_capabilities: Option<Vec<String>>,
        bindings_by_entry: indexmap::IndexMap<String, crate::binding_slots::SessionBindingMap>,
    ) -> Self {
        let core = Arc::new(SessionCore::new());
        Self {
            prompt_hash,
            prompt_text,
            cgs,
            contexts_by_entry,
            entry_id,
            tenant_scope,
            principal_subject,
            http_backend,
            entities,
            teaching_exposure,
            domain_revision: 0,
            principal,
            plugin_generation,
            catalog_cgs_hash,
            registry_catalog_hashes_by_entry: HashMap::new(),
            context_intent,
            ranked_capabilities,
            session_share_token: Arc::new(RwLock::new(None)),
            session_proof_base_token: Arc::new(RwLock::new(None)),
            graph_cache: core.graph_cache(),
            core,
            run_resource_next: Arc::new(AtomicU64::new(0)),
            paging_resume_by_handle: Arc::new(StdMutex::new(HashMap::new())),
            paging_handle_next: Arc::new(AtomicU64::new(0)),
            paging_op_lock: Arc::new(tokio::sync::Mutex::new(())),
            operation_by_handle: Arc::new(StdMutex::new(HashMap::new())),
            operation_handle_next: Arc::new(AtomicU64::new(0)),
            operation_wire: Arc::new(StdMutex::new(
                crate::operation_persist::OperationWireBinding::default(),
            )),
            plan_commits: Arc::new(StdMutex::new(HashMap::new())),
            plan_commit_next: Arc::new(AtomicU64::new(0)),
            evidence_chain: crate::evidence_chain::new_evidence_chain_slot(),
            sync_live_run_inflight: Arc::new(AtomicBool::new(false)),
            bindings_by_entry,
        }
    }

    /// Session binding wire values for a catalog `entry_id`.
    pub fn session_bindings_for_entry(
        &self,
        entry_id: &str,
    ) -> Option<&crate::binding_slots::SessionBindingMap> {
        self.bindings_by_entry.get(entry_id)
    }

    /// Allocate the next monotonic `resource_index` for this execute session (used for `plasm://r/{n}`).
    pub fn mint_run_resource_index(&self) -> u64 {
        self.run_resource_next.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Mint a paging handle and store `resume` for subsequent `page(...)` expressions.
    /// - `logical_session_ref: None` — plain `pgN` (HTTP execute).
    /// - `Some("l_<token>")` — namespaced `l_<token>_pgN` (MCP `plasm` with `logical_session_ref` on the trace).
    pub fn register_paging_continuation(
        &self,
        resume: QueryPaginationResumeData,
        logical_session_ref: Option<&str>,
    ) -> PagingHandle {
        let n = self.paging_handle_next.fetch_add(1, Ordering::Relaxed) + 1;
        let handle = match logical_session_ref {
            Some(r) => PagingHandle::mint_namespaced(r, n),
            None => PagingHandle::mint_monotonic(n),
        };
        self.paging_resume_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle.clone(), PagingResume::Query(resume));
        handle
    }

    pub fn peek_paging_resume(&self, handle: &PagingHandle) -> Option<QueryPaginationResumeData> {
        self.paging_resume_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(handle)
            .and_then(|resume| match resume {
                PagingResume::Query(query) => Some(query.clone()),
                PagingResume::Synthetic(_) => None,
            })
    }

    pub fn upsert_paging_resume(&self, handle: &PagingHandle, resume: QueryPaginationResumeData) {
        self.paging_resume_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle.clone(), PagingResume::Query(resume));
    }

    pub fn register_synthetic_paging_continuation(
        &self,
        resume: SyntheticPageCursor,
        logical_session_ref: Option<&str>,
    ) -> PagingHandle {
        let n = self.paging_handle_next.fetch_add(1, Ordering::Relaxed) + 1;
        let handle = match logical_session_ref {
            Some(r) => PagingHandle::mint_namespaced(r, n),
            None => PagingHandle::mint_monotonic(n),
        };
        self.paging_resume_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle.clone(), PagingResume::Synthetic(resume));
        handle
    }

    pub fn peek_synthetic_paging_resume(
        &self,
        handle: &PagingHandle,
    ) -> Option<SyntheticPageCursor> {
        self.paging_resume_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(handle)
            .and_then(|resume| match resume {
                PagingResume::Query(_) => None,
                PagingResume::Synthetic(cursor) => Some(cursor.clone()),
            })
    }

    pub fn upsert_synthetic_paging_resume(
        &self,
        handle: &PagingHandle,
        resume: SyntheticPageCursor,
    ) {
        self.paging_resume_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle.clone(), PagingResume::Synthetic(resume));
    }

    pub fn remove_paging_resume(&self, handle: &PagingHandle) {
        self.paging_resume_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(handle);
    }

    /// Mint the next monotonic operation handle for MCP logical session `l_<token>`.
    pub fn mint_operation_handle(&self, logical_session_ref: &str) -> OperationHandle {
        let n = self.operation_handle_next.fetch_add(1, Ordering::Relaxed) + 1;
        OperationHandle::mint_namespaced(logical_session_ref, n)
    }

    /// Mint plain `oN` for HTTP execute (no MCP logical session).
    pub fn mint_operation_handle_plain(&self) -> OperationHandle {
        let n = self.operation_handle_next.fetch_add(1, Ordering::Relaxed) + 1;
        OperationHandle::mint_monotonic(n)
    }

    pub fn register_operation(
        &self,
        handle: OperationHandle,
        state: crate::operation::OperationState,
    ) {
        self.operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle, state);
    }

    /// Reject nested synchronous live runs on the same execute session.
    pub fn begin_sync_live_run(&self) -> Result<(), String> {
        if self
            .sync_live_run_inflight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(
                "operation_in_flight: a synchronous live run is in progress on this execute session"
                    .to_string(),
            );
        }
        Ok(())
    }

    pub fn end_sync_live_run(&self) {
        self.sync_live_run_inflight.store(false, Ordering::Release);
    }

    /// Register an async operation; rejects when running-op cap is reached.
    pub fn try_begin_async_operation(
        &self,
        handle: OperationHandle,
        cancel: plasm_runtime::CancelSignal,
        accept: crate::operation::OpAcceptContext,
    ) -> Result<(), String> {
        let cap = max_running_ops_per_session();
        let mut map = self
            .operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let running = running_handles_from_map(&map);
        if running.len() >= cap {
            return Err(format_too_many_operations_error(&running, cap));
        }
        let (progress_tx, _) = tokio::sync::broadcast::channel(64);
        map.insert(
            handle.clone(),
            crate::operation::OperationState {
                phase: crate::operation::OperationPhase::Running,
                cancel,
                started_at: Instant::now(),
                progress: crate::operation::OperationProgress::default(),
                result: None,
                error: None,
                live_executor: true,
                run_artifact_id: None,
                agent_emit: crate::operation_progress::OperationAgentEmitState::default(),
                display_map: accept.display_map,
                plan_commit_ref: accept.plan_commit_ref,
                dry_verdict: accept.dry_verdict,
                auto_async: accept.auto_async,
                mcp_transport_key: accept.mcp_transport_key,
                progress_host: accept.host,
                progress_tx,
                comp: accept.comp,
                plan_ux_reflection: accept.plan_ux_reflection,
                step_order: accept.step_order,
            },
        );
        if let Ok(mut wire) = self.operation_wire.lock() {
            wire.started_at_unix_by_handle.insert(
                handle.as_str().to_string(),
                crate::operation_persist::unix_now(),
            );
        }
        Ok(())
    }

    pub fn get_operation(
        &self,
        handle: &OperationHandle,
    ) -> Option<crate::operation::OperationState> {
        self.operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(handle)
            .cloned()
    }

    /// Running async operations with a live executor on this session (for stale-handle diagnostics).
    pub fn open_live_operation_handles(&self) -> Vec<OperationHandle> {
        let map = self
            .operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        running_handles_from_map(&map)
    }

    pub fn get_operation_poll_snapshot(
        &self,
        handle: &OperationHandle,
    ) -> Option<crate::operation::OperationPollSnapshot> {
        self.operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(handle)
            .map(|op| match op.phase {
                crate::operation::OperationPhase::Running => {
                    crate::operation::OperationPollSnapshot::Running(op.progress.clone())
                }
                crate::operation::OperationPhase::Succeeded => {
                    if let Some(result) = op.result.clone() {
                        crate::operation::OperationPollSnapshot::Succeeded(result)
                    } else {
                        crate::operation::OperationPollSnapshot::Failed(
                            "operation succeeded; poll again after cross-pod artifact hydrate"
                                .to_string(),
                        )
                    }
                }
                crate::operation::OperationPhase::Failed => {
                    crate::operation::OperationPollSnapshot::Failed(
                        op.error
                            .clone()
                            .unwrap_or_else(|| "operation failed".to_string()),
                    )
                }
                crate::operation::OperationPhase::Cancelled => {
                    crate::operation::OperationPollSnapshot::Cancelled(op.progress.clone())
                }
            })
    }

    pub fn get_operation_progress(
        &self,
        handle: &OperationHandle,
    ) -> Option<crate::operation::OperationProgress> {
        self.operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(handle)
            .map(|op| op.progress.clone())
    }

    pub fn update_operation_progress(
        &self,
        handle: &OperationHandle,
        mut progress: crate::operation::OperationProgress,
    ) {
        let mut map = self
            .operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(op) = map.get_mut(handle) else {
            return;
        };
        if let Some(ref node_id) = progress.label {
            if let Some(display) = op.display_map.get(node_id) {
                progress.label = Some(display.clone());
            }
        }
        op.progress = progress;
        let host = op.progress_host.as_ref().and_then(|w| w.upgrade());
        drop(map);
        let st_ref = host.as_deref();
        if self.try_emit_op_running(handle, st_ref) {
            self.persist_operation_state(
                handle,
                crate::operation_persist::PersistUrgency::Coalesced,
            );
        }
    }

    fn fanout_op_line(
        &self,
        handle: &OperationHandle,
        line: &str,
        seq: u64,
        terminal: bool,
        st: Option<&crate::server_state::PlasmHostState>,
    ) {
        if let Some(op) = self.get_operation(handle) {
            let _ = op
                .progress_tx
                .send(crate::operation_progress::OpProgressEvent {
                    seq,
                    line: line.to_string(),
                    terminal,
                });
            if let (Some(st), Some(tk)) = (st, op.mcp_transport_key.as_deref()) {
                st.op_progress_hub
                    .queue_mcp_notify(tk, line, seq, op.plan_commit_ref.as_ref());
            }
        }
    }

    pub fn emit_op_accept(
        &self,
        handle: &OperationHandle,
        st: &crate::server_state::PlasmHostState,
    ) -> Result<(), String> {
        let mut map = self
            .operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(op) = map.get_mut(handle) else {
            return Err(format!("unknown operation handle `{}`", handle.as_str()));
        };
        op.agent_emit.seq = 1;
        let line = crate::operation_progress::render_op_wire_line(
            handle,
            crate::operation_progress::OpWireSig::Accept,
            None,
            op.plan_commit_ref.as_ref(),
            op.dry_verdict,
            None,
        );
        op.agent_emit.last_line = line.clone();
        op.agent_emit.last_emit_at = Instant::now();
        drop(map);
        self.fanout_op_line(handle, &line, 1, false, Some(st));
        self.persist_operation_state(handle, crate::operation_persist::PersistUrgency::Immediate);
        Ok(())
    }

    fn try_emit_op_running(
        &self,
        handle: &OperationHandle,
        st: Option<&crate::server_state::PlasmHostState>,
    ) -> bool {
        let mut map = self
            .operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(op) = map.get_mut(handle) else {
            return false;
        };
        if op.phase != crate::operation::OperationPhase::Running {
            return false;
        }
        let snapshot = crate::operation_progress::OpAgentSnapshot::from_running(&op.progress);
        if !crate::operation_progress::should_emit_agent_progress(
            op.agent_emit.last_emitted,
            snapshot,
            op.agent_emit.last_emit_at,
        ) {
            return false;
        }
        op.agent_emit.seq = op.agent_emit.seq.saturating_add(1);
        op.agent_emit.last_emitted = snapshot;
        op.agent_emit.last_emit_at = Instant::now();
        let line = crate::operation_progress::render_op_wire_line(
            handle,
            crate::operation_progress::OpWireSig::Running,
            Some(&op.progress),
            None,
            None,
            None,
        );
        op.agent_emit.last_line = line.clone();
        let seq = op.agent_emit.seq;
        drop(map);
        self.fanout_op_line(handle, &line, seq, false, st);
        true
    }

    fn emit_op_terminal(
        &self,
        handle: &OperationHandle,
        phase: crate::operation::OperationPhase,
        st: Option<&crate::server_state::PlasmHostState>,
    ) {
        let mut map = self
            .operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(op) = map.get_mut(handle) else {
            return;
        };
        op.agent_emit.seq = op.agent_emit.seq.saturating_add(1);
        let sig = match phase {
            crate::operation::OperationPhase::Succeeded => {
                crate::operation_progress::OpWireSig::Done
            }
            crate::operation::OperationPhase::Cancelled => {
                crate::operation_progress::OpWireSig::Cancelled
            }
            crate::operation::OperationPhase::Failed => {
                crate::operation_progress::OpWireSig::Failed
            }
            crate::operation::OperationPhase::Running => return,
        };
        let line = crate::operation_progress::render_op_wire_line(
            handle,
            sig,
            Some(&op.progress),
            None,
            None,
            op.error.as_deref(),
        );
        op.agent_emit.last_line = line.clone();
        let seq = op.agent_emit.seq;
        drop(map);
        self.fanout_op_line(handle, &line, seq, true, st);
    }

    pub fn operation_progress_subscribe(
        &self,
        handle: &OperationHandle,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::operation_progress::OpProgressEvent>> {
        self.operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(handle)
            .map(|op| op.progress_tx.subscribe())
    }

    pub fn operation_progress_snapshot_line(
        &self,
        handle: &OperationHandle,
    ) -> Option<(u64, String)> {
        self.operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(handle)
            .map(|op| (op.agent_emit.seq, op.agent_emit.last_line.clone()))
    }

    pub fn operation_poll_parts(
        &self,
        handle: &OperationHandle,
    ) -> Option<(String, serde_json::Map<String, serde_json::Value>, bool)> {
        let map = self
            .operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let op = map.get(handle)?;
        if op.phase != crate::operation::OperationPhase::Running {
            return None;
        }
        let snapshot = crate::operation_progress::OpAgentSnapshot::from_running(&op.progress);
        let unchanged = snapshot == op.agent_emit.last_emitted && op.agent_emit.seq > 0;
        let sig = if unchanged {
            crate::operation_progress::OpWireSig::Unchanged
        } else {
            crate::operation_progress::OpWireSig::Running
        };
        let markdown = if unchanged {
            crate::operation_progress::render_op_wire_markdown(
                &crate::operation_progress::render_op_wire_line(
                    handle,
                    sig,
                    Some(&op.progress),
                    None,
                    None,
                    None,
                ),
            )
        } else {
            crate::operation::operation_running_markdown(handle, &op.progress, false)
        };
        let meta = if unchanged {
            crate::operation_progress::op_poll_unchanged_meta(op.agent_emit.seq, Some(&op.progress))
        } else {
            crate::operation::operation_meta_object(
                handle,
                crate::operation_progress::OpWireSig::Running,
                op.agent_emit.seq,
                Some(&op.progress),
                op.plan_commit_ref.as_ref(),
            )
        };
        let mut plasm = meta;
        crate::run_explorer_meta::merge_run_explorer_fields_into_plasm(
            &mut plasm,
            op,
            Some(&op.progress),
        );
        Some((markdown, plasm, unchanged))
    }

    pub fn finalize_operation_succeeded(
        &self,
        handle: &OperationHandle,
        result: crate::plasm_plan_run::PlasmPlanRunResult,
        st: Option<&crate::server_state::PlasmHostState>,
    ) {
        if let Some(op) = self
            .operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(handle)
        {
            if op.phase != crate::operation::OperationPhase::Running {
                return;
            }
            op.phase = crate::operation::OperationPhase::Succeeded;
            op.result = Some(Arc::new(result));
        }
        self.emit_op_terminal(handle, crate::operation::OperationPhase::Succeeded, st);
        self.persist_operation_state(handle, crate::operation_persist::PersistUrgency::Immediate);
    }

    pub fn finalize_operation_succeeded_with_artifact(
        &self,
        handle: &OperationHandle,
        result: crate::plasm_plan_run::PlasmPlanRunResult,
        run_artifact_id: Option<String>,
        st: Option<&crate::server_state::PlasmHostState>,
    ) {
        if let Some(id) = run_artifact_id {
            self.set_operation_run_artifact_id(handle, id);
        }
        self.finalize_operation_succeeded(handle, result, st);
    }

    pub fn finalize_operation_failed(
        &self,
        handle: &OperationHandle,
        error: String,
        st: Option<&crate::server_state::PlasmHostState>,
    ) {
        if let Some(op) = self
            .operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(handle)
        {
            if op.phase != crate::operation::OperationPhase::Running {
                return;
            }
            op.phase = crate::operation::OperationPhase::Failed;
            op.error = Some(error);
        }
        self.emit_op_terminal(handle, crate::operation::OperationPhase::Failed, st);
        self.persist_operation_state(handle, crate::operation_persist::PersistUrgency::Immediate);
    }

    pub fn cancel_operation(
        &self,
        handle: &OperationHandle,
        st: Option<&crate::server_state::PlasmHostState>,
    ) -> bool {
        let mut map = self
            .operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(op) = map.get_mut(handle) else {
            return false;
        };
        if op.phase != crate::operation::OperationPhase::Running {
            return true;
        }
        op.cancel.cancel();
        op.phase = crate::operation::OperationPhase::Cancelled;
        drop(map);
        self.emit_op_terminal(handle, crate::operation::OperationPhase::Cancelled, st);
        self.persist_operation_state(handle, crate::operation_persist::PersistUrgency::Immediate);
        true
    }

    pub fn mint_plan_commit_ref(&self) -> PlanCommitRef {
        let n = self.plan_commit_next.fetch_add(1, Ordering::Relaxed);
        PlanCommitRef::mint(n)
    }

    pub fn register_plan_commit(&self, record: crate::operation::PlanCommitRecord) {
        self.plan_commits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(record.commit_ref.clone(), record);
    }

    pub fn get_plan_commit(
        &self,
        commit_ref: &PlanCommitRef,
    ) -> Option<crate::operation::PlanCommitRecord> {
        self.plan_commits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(commit_ref)
            .cloned()
    }

    pub(crate) fn snapshot_plan_commits_for_persist(
        &self,
    ) -> crate::mcp_transport_store::execute_session_registry::PlanCommitPersistSnapshot {
        use crate::mcp_transport_store::execute_session_registry::PersistedPlanCommitRecord;
        use std::time::{SystemTime, UNIX_EPOCH};
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let map = self.plan_commits.lock().unwrap_or_else(|e| e.into_inner());
        let mut records = Vec::new();
        let mut max_seq = self.plan_commit_next.load(Ordering::Relaxed);
        for record in map.values() {
            if record.is_expired() {
                continue;
            }
            let expires_at_unix = now_unix.saturating_add(
                record
                    .expires_at
                    .checked_duration_since(std::time::Instant::now())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
            if let Some(n) = record
                .commit_ref
                .as_str()
                .strip_prefix("pc")
                .and_then(|s| s.parse::<u64>().ok())
            {
                max_seq = max_seq.max(n.saturating_add(1));
            }
            records.push(PersistedPlanCommitRecord {
                commit_ref: record.commit_ref.as_str().to_string(),
                commit_id_hex: record.commit_id.to_string(),
                dry_review: record.dry_review.clone(),
                verdict: record.verdict,
                expires_at_unix,
            });
        }
        crate::mcp_transport_store::execute_session_registry::PlanCommitPersistSnapshot {
            records,
            next_sequence: max_seq,
        }
    }

    pub(crate) fn restore_persisted_plan_commits(
        &self,
        records: &[crate::mcp_transport_store::execute_session_registry::PersistedPlanCommitRecord],
        next_sequence: u64,
    ) {
        use plasm_core::PlanCommitId;
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut map = self.plan_commits.lock().unwrap_or_else(|e| e.into_inner());
        for persisted in records {
            let Some(commit_ref) = PlanCommitRef::parse(&persisted.commit_ref) else {
                continue;
            };
            if persisted.expires_at_unix <= now_unix {
                continue;
            }
            let Ok(bytes) = hex::decode(persisted.commit_id_hex.as_str()) else {
                continue;
            };
            if bytes.len() != 32 {
                continue;
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            let ttl_secs = persisted.expires_at_unix.saturating_sub(now_unix);
            map.insert(
                commit_ref.clone(),
                crate::operation::PlanCommitRecord {
                    commit_ref,
                    commit_id: PlanCommitId::from_canonical_bytes(arr),
                    dry_review: persisted.dry_review.clone(),
                    verdict: persisted.verdict,
                    expires_at: std::time::Instant::now() + Duration::from_secs(ttl_secs),
                },
            );
        }
        let current = self.plan_commit_next.load(Ordering::Relaxed);
        if next_sequence > current {
            self.plan_commit_next
                .store(next_sequence, Ordering::Relaxed);
        }
    }

    /// View scope injection context for composed `views:` preflight and live execute.
    pub fn view_ambient(&self) -> ViewAmbientContext {
        ViewAmbientContext::from_http_backend(self.http_backend.as_deref())
    }

    /// Multi-catalog dispatch for execute (HTTP backend + auth per owning graph).
    pub fn federation_dispatch(&self) -> Option<Arc<FederationDispatch>> {
        if self.contexts_by_entry.len() <= 1 {
            return None;
        }
        Some(Arc::new(crate::catalog_ownership::federation_for_session(
            self,
        )))
    }

    /// Exclusive graph-cache access for the full execute / rehydrate await chain.
    pub(crate) async fn lock_graph_cache(&self) -> GraphCacheGuard<'_> {
        GraphCacheGuard::from_guard(self.graph_cache.lock().await)
    }

    async fn finalize_run_artifacts(&self, session_id: &str, store: &RunArtifactStore) {
        let artifacts = self.core.drain_run_artifacts().await;
        if artifacts.is_empty() {
            return;
        }
        let mut failed = Vec::new();
        for a in artifacts {
            if let Err(err) = store
                .insert_payload(
                    self.prompt_hash.as_str(),
                    session_id,
                    a.run_id,
                    Some(a.resource_index),
                    &a.payload,
                )
                .await
            {
                tracing::warn!(
                    error = %err,
                    prompt_hash = %self.prompt_hash,
                    session_id = %session_id,
                    run_id = %a.run_id.to_wire(),
                    "failed to flush session run artifact"
                );
                failed.push(a.clone());
            }
        }
        self.core.requeue_run_artifacts(failed).await;
    }
}

#[derive(Clone)]
pub struct ExecuteSessionStore {
    inner: Arc<RwLock<HashMap<ExecuteSessionKey, SessionRecord>>>,
    /// Maps `(entry_id, entities)` → `(prompt_hash, session_id)` for reuse without re-rendering Plasm instructions.
    reuse_index: Arc<RwLock<HashMap<SessionReuseKey, (String, String)>>>,
    finalize_tx: mpsc::Sender<(Arc<ExecuteSession>, String)>,
    /// Shared [`plasm_core::SymbolMapCrossRequestCache`] across HTTP/MCP execute sessions (`PLASM_SYMBOL_MAP_LRU_CAP`).
    symbol_map_cross_cache: Arc<plasm_core::SymbolMapCrossRequestCache>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct ExecuteSessionKey {
    prompt_hash: String,
    session_id: String,
}

impl ExecuteSessionKey {
    fn new(prompt_hash: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            prompt_hash: prompt_hash.into(),
            session_id: session_id.into(),
        }
    }
}

impl Default for ExecuteSessionStore {
    fn default() -> Self {
        Self::new(Arc::new(RunArtifactStore::memory()), None)
    }
}

struct SessionRecord {
    session: Arc<ExecuteSession>,
    expires_at: StdMutex<Instant>,
}

impl SessionRecord {
    fn new(session: Arc<ExecuteSession>) -> Self {
        Self {
            session,
            expires_at: StdMutex::new(Instant::now() + SESSION_TTL),
        }
    }
    fn touch(&self) {
        if let Ok(mut g) = self.expires_at.lock() {
            *g = Instant::now() + SESSION_TTL;
        }
    }
    fn is_expired(&self) -> bool {
        if let Ok(g) = self.expires_at.lock() {
            Instant::now() > *g
        } else {
            true
        }
    }
}

impl ExecuteSessionStore {
    fn enqueue_finalize(&self, sess: Arc<ExecuteSession>, sid: String) {
        match self.finalize_tx.try_send((sess.clone(), sid.clone())) {
            Ok(()) => {}
            Err(TrySendError::Full(item)) => {
                let tx = self.finalize_tx.clone();
                tokio::spawn(async move {
                    if tx.send(item).await.is_err() {
                        tracing::warn!("finalize queue closed; dropped session finalization");
                    }
                });
            }
            Err(TrySendError::Closed(_)) => {
                tracing::warn!("finalize queue closed; dropped session finalization");
            }
        }
    }

    pub fn new(
        release_artifacts: Arc<RunArtifactStore>,
        release_graph_persistence: Option<Arc<SessionGraphPersistence>>,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<(Arc<ExecuteSession>, String)>(256);
        let release_artifacts_bg = Some(release_artifacts.clone());
        let release_graph_persistence_bg = release_graph_persistence.clone();
        tokio::spawn(async move {
            while let Some((sess, sid)) = rx.recv().await {
                for attempt in 0..3 {
                    let ok = finalize_session_once(
                        &sess,
                        sid.as_str(),
                        release_artifacts_bg.as_ref(),
                        release_graph_persistence_bg.as_ref(),
                    )
                    .await
                    .is_ok();
                    if ok {
                        break;
                    }
                    sleep(TokioDuration::from_millis(100 * (attempt + 1) as u64)).await;
                }
            }
        });
        let store = Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            reuse_index: Arc::new(RwLock::new(HashMap::new())),
            finalize_tx: tx,
            symbol_map_cross_cache: Arc::new(plasm_core::SymbolMapCrossRequestCache::from_env()),
        };
        {
            let inner = Arc::clone(&store.inner);
            let finalize_tx = store.finalize_tx.clone();
            tokio::spawn(async move {
                loop {
                    sleep(TokioDuration::from_secs(5)).await;
                    let expired: Vec<(String, Arc<ExecuteSession>)> = {
                        let mut g = inner.write().await;
                        let keys: Vec<ExecuteSessionKey> = g
                            .iter()
                            .filter(|(_, v)| v.is_expired())
                            .map(|(k, _)| k.clone())
                            .collect();
                        let mut out = Vec::with_capacity(keys.len());
                        for key in keys {
                            if let Some(sess) = g.remove(&key) {
                                out.push((key.session_id, sess.session));
                            }
                        }
                        out
                    };
                    for (sid, sess) in expired {
                        let _ = finalize_tx.try_send((sess, sid));
                    }
                }
            });
        }
        store
    }

    pub fn symbol_map_cross_cache(&self) -> &plasm_core::SymbolMapCrossRequestCache {
        self.symbol_map_cross_cache.as_ref()
    }

    /// Clears process-wide caches derived from loaded [`CGS`](plasm_core::schema::CGS) (symbol-map LRU).
    /// Call after plugin-dir catalog reload when the API schema set changed so no snapshot from a prior `.so` remains.
    pub fn invalidate_cgs_derived_caches(&self) {
        self.symbol_map_cross_cache.clear();
    }

    /// If a non-expired session already exists for this key, return `(session_id, session)` and refresh TTL.
    pub async fn try_reuse_session(
        &self,
        key: &SessionReuseKey,
    ) -> Option<(String, Arc<ExecuteSession>)> {
        let (ph, sid) = {
            let r = self.reuse_index.read().await;
            r.get(key).cloned()?
        };
        let sess = self.get_unchecked_by_strs(&ph, &sid).await?;
        Some((sid, sess))
    }

    pub async fn insert(
        &self,
        reuse_key: SessionReuseKey,
        prompt_hash: String,
        session_id: String,
        session: ExecuteSession,
    ) {
        let session = Arc::new(session);
        let mut g = self.inner.write().await;
        let mut r = self.reuse_index.write().await;
        let mut removed: Vec<(String, Arc<ExecuteSession>)> = Vec::new();
        if let Some((old_ph, old_sid)) = r.get(&reuse_key).cloned() {
            if old_ph != prompt_hash || old_sid != session_id {
                if let Some(old) = g.remove(&ExecuteSessionKey::new(old_ph, old_sid.clone())) {
                    removed.push((old_sid, old.session));
                }
            }
        }
        if let Some(old) = g.insert(
            ExecuteSessionKey::new(prompt_hash.clone(), session_id.clone()),
            SessionRecord::new(session),
        ) {
            removed.push((session_id.clone(), old.session));
        }
        r.insert(reuse_key, (prompt_hash, session_id));
        drop(r);
        drop(g);
        for (sid, old) in removed {
            self.enqueue_finalize(old, sid);
        }
    }

    /// Replace session payload (e.g. after incremental graph expansion).
    pub async fn replace_session(
        &self,
        prompt_hash: &PromptHashHex,
        session_id: &ExecuteSessionId,
        session: ExecuteSession,
    ) {
        let key = ExecuteSessionKey::new(prompt_hash.as_str(), session_id.as_str());
        let mut g = self.inner.write().await;
        g.insert(key, SessionRecord::new(Arc::new(session)));
    }

    /// Cross-pod reload: insert without evicting/finalizing prior rows.
    pub async fn insert_rehydrated(
        &self,
        reuse_key: SessionReuseKey,
        prompt_hash: String,
        session_id: String,
        session: ExecuteSession,
    ) {
        let session = Arc::new(session);
        let mut g = self.inner.write().await;
        let mut r = self.reuse_index.write().await;
        g.insert(
            ExecuteSessionKey::new(prompt_hash.clone(), session_id.clone()),
            SessionRecord::new(Arc::clone(&session)),
        );
        r.insert(reuse_key, (prompt_hash, session_id));
    }

    /// Returns the session if present, non-expired, and `prompt_hash` matches the stored value.
    pub async fn get(
        &self,
        prompt_hash: &PromptHashHex,
        session_id: &ExecuteSessionId,
    ) -> Option<Arc<ExecuteSession>> {
        let g = self.inner.read().await;
        let key = ExecuteSessionKey::new(prompt_hash.as_str(), session_id.as_str());
        let s = g.get(&key)?;
        if s.session.prompt_hash != prompt_hash.as_str() || s.is_expired() {
            return None;
        }
        s.touch();
        Some(s.session.clone())
    }

    pub async fn get_by_strs(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Option<Arc<ExecuteSession>> {
        let ph: PromptHashHex = prompt_hash.parse().ok()?;
        let sid: ExecuteSessionId = session_id.parse().ok()?;
        self.get(&ph, &sid).await
    }

    /// Drop one in-memory execute row (and reuse-index entry when it points at this pair).
    pub async fn remove_by_strs(&self, prompt_hash: &str, session_id: &str) {
        let removed = {
            let mut g = self.inner.write().await;
            g.remove(&ExecuteSessionKey::new(prompt_hash, session_id))
                .map(|rec| rec.session)
        };
        let Some(sess) = removed else {
            return;
        };
        {
            let mut r = self.reuse_index.write().await;
            r.retain(|_, (ph, sid)| !(ph == prompt_hash && sid == session_id));
        }
        self.enqueue_finalize(sess, session_id.to_string());
    }

    /// Drop all in-memory execute rows (plugin catalog reload; stale CGS must not survive locally).
    pub async fn purge_all(&self) {
        let removed: Vec<(String, Arc<ExecuteSession>)> = {
            let mut g = self.inner.write().await;
            let mut r = self.reuse_index.write().await;
            r.clear();
            g.drain()
                .map(|(k, rec)| (k.session_id, rec.session))
                .collect()
        };
        for (sid, sess) in removed {
            self.enqueue_finalize(sess, sid);
        }
    }

    async fn get_unchecked_by_strs(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Option<Arc<ExecuteSession>> {
        let g = self.inner.read().await;
        let key = ExecuteSessionKey::new(prompt_hash.to_string(), session_id.to_string());
        let s = g.get(&key)?;
        if s.session.prompt_hash != prompt_hash || s.is_expired() {
            return None;
        }
        s.touch();
        Some(s.session.clone())
    }
}

async fn finalize_session_once(
    sess: &Arc<ExecuteSession>,
    session_id: &str,
    release_artifacts: Option<&Arc<RunArtifactStore>>,
    release_graph_persistence: Option<&Arc<SessionGraphPersistence>>,
) -> Result<(), String> {
    if let Some(store) = release_artifacts {
        sess.finalize_run_artifacts(session_id, store).await;
    }
    if let Some(persistence) = release_graph_persistence {
        let through_seq = sess.core.tip_seq().await.0;
        let span = crate::spans::execute_graph_snapshot_finalize(through_seq);
        let _guard = span.enter();
        let started = std::time::Instant::now();
        let cache = sess.lock_graph_cache().await;
        let entity_count = cache.stats().total_entities;
        match persistence
            .write_snapshot(
                sess.prompt_hash.as_str(),
                session_id,
                through_seq,
                "application/json",
                &cache,
            )
            .await
        {
            Ok(()) => {
                crate::graph_cache_metrics::record_graph_snapshot(
                    "success",
                    entity_count,
                    started.elapsed(),
                );
            }
            Err(e) => {
                crate::graph_cache_metrics::record_graph_snapshot(
                    "error",
                    entity_count,
                    started.elapsed(),
                );
                return Err(e);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_artifacts::RunArtifactId;
    use crate::run_artifacts::{ArtifactPayload, ArtifactPayloadMetadata};
    use plasm_core::CgsContext;
    use plasm_core::CGS;

    #[tokio::test]
    async fn reuse_returns_same_session_id_for_same_entry_and_entities() {
        let store = ExecuteSessionStore::default();
        let cgs = Arc::new(CGS::new());
        let key = SessionReuseKey {
            tenant_scope: String::new(),
            entry_id: "default".into(),
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
            entities: vec!["Pet".into(), "Store".into()],
            context_intent: None,
            ranked_capabilities: None,
            principal: None,
            plugin_generation_id: None,
            logical_session_id: None,
        };

        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", cgs.clone())),
        );
        let s1 = ExecuteSession::new(
            "ph1".into(),
            "prompt-a".into(),
            cgs.clone(),
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into(), "Store".into()],
            None,
            None,
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        store
            .insert(key.clone(), "ph1".into(), "sid-one".into(), s1)
            .await;

        let reused = store.try_reuse_session(&key).await;
        assert!(reused.is_some(), "expected reuse");
        let (sid, sess) = reused.unwrap();
        assert_eq!(sid, "sid-one");
        assert_eq!(sess.prompt_text, "prompt-a");
    }

    #[tokio::test]
    async fn distinct_open_sessions_use_distinct_graph_caches() {
        let cgs = Arc::new(CGS::new());
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", cgs.clone())),
        );
        let s1 = ExecuteSession::new(
            "ph1".into(),
            "p".into(),
            cgs.clone(),
            ctxs.clone(),
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let s2 = ExecuteSession::new(
            "ph2".into(),
            "p".into(),
            cgs,
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            s1.catalog_cgs_hash.clone(),
            None,
            None,
        );
        assert!(!Arc::ptr_eq(&s1.graph_cache, &s2.graph_cache));
    }

    #[tokio::test]
    async fn session_core_tracks_artifact_and_tip_seq() {
        let core = SessionCore::new();
        let run_id = RunArtifactId::from_bytes([0xcd; 32]);
        let payload = ArtifactPayload {
            metadata: ArtifactPayloadMetadata::json_default(),
            bytes: axum::body::Bytes::from_static(br#"{"ok":true}"#),
        };
        let first = core
            .append_run_artifact(run_id, GraphEpoch(0), 1, payload.clone())
            .await;
        assert_eq!(first.seq, DeltaSeq(1));
        assert_eq!(first.epoch, GraphEpoch(0));
        assert_eq!(core.tip_seq().await, DeltaSeq(1));
        let got = core
            .get_run_artifact(run_id)
            .await
            .expect("run artifact exists");
        assert_eq!(got.payload, payload);
    }

    #[tokio::test]
    async fn delta_seq_monotonic_across_alloc_and_run_artifacts() {
        let core = SessionCore::new();
        assert_eq!(core.alloc_delta_seq().await, DeltaSeq(1));
        let run_id = RunArtifactId::from_bytes([0xab; 32]);
        let payload = ArtifactPayload {
            metadata: ArtifactPayloadMetadata::json_default(),
            bytes: axum::body::Bytes::from_static(b"{}"),
        };
        let art = core
            .append_run_artifact(run_id, GraphEpoch(0), 1, payload)
            .await;
        assert_eq!(art.seq, DeltaSeq(2));
        assert_eq!(core.alloc_delta_seq().await, DeltaSeq(3));
        assert_eq!(core.tip_seq().await, DeltaSeq(3));
    }

    #[tokio::test]
    async fn concurrent_gets_return_live_session() {
        let store = ExecuteSessionStore::default();
        let cgs = Arc::new(CGS::new());
        let key = SessionReuseKey {
            tenant_scope: String::new(),
            entry_id: "default".into(),
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
            entities: vec!["Pet".into()],
            context_intent: None,
            ranked_capabilities: None,
            principal: None,
            plugin_generation_id: None,
            logical_session_id: None,
        };
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", cgs.clone())),
        );
        let sess = ExecuteSession::new(
            "3c61dab1a208fb4c71a5079c0f513f894ce5f65700041943a3e0e2cef2cc6fc1".into(),
            "prompt".into(),
            cgs,
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            "hash".into(),
            None,
            None,
        );
        store
            .insert(
                key,
                "3c61dab1a208fb4c71a5079c0f513f894ce5f65700041943a3e0e2cef2cc6fc1".into(),
                "d8946f9c00a4474aa1ec0d1b3d4b76b8".into(),
                sess,
            )
            .await;
        let ph: PromptHashHex = "3c61dab1a208fb4c71a5079c0f513f894ce5f65700041943a3e0e2cef2cc6fc1"
            .parse()
            .expect("valid prompt hash");
        let sid: ExecuteSessionId = "d8946f9c00a4474aa1ec0d1b3d4b76b8"
            .parse()
            .expect("valid sid");
        let mut handles = Vec::new();
        for _ in 0..64 {
            let store = store.clone();
            let ph = ph.clone();
            let sid = sid.clone();
            handles.push(tokio::spawn(
                async move { store.get(&ph, &sid).await.is_some() },
            ));
        }
        for h in handles {
            assert!(h.await.expect("join"));
        }
    }

    #[test]
    fn paging_registry_mints_monotonic_handles() {
        let cgs = Arc::new(CGS::new());
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", cgs.clone())),
        );
        let sess = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let r = sample_pagination_resume();
        let h1 = sess.register_paging_continuation(r.clone(), None);
        assert_eq!(h1.as_str(), "pg1");
        let h2 = sess.register_paging_continuation(r.clone(), None);
        assert_eq!(h2.as_str(), "pg2");
        assert!(sess.peek_paging_resume(&h1).is_some());
        sess.remove_paging_resume(&h1);
        assert!(sess.peek_paging_resume(&h1).is_none());
    }

    fn sample_pagination_resume() -> QueryPaginationResumeData {
        use indexmap::indexmap;
        use plasm_compile::{
            parse_capability_template, CmlEnv, PaginationConfig, PaginationLocation,
            PaginationParam,
        };
        use plasm_runtime::QueryPaginationState;

        let template = parse_capability_template(&serde_json::json!({
            "transport": "http",
            "method": "GET",
            "path": [
                { "type": "literal", "value": "things" }
            ],
            "response": { "items": "results" },
        }))
        .expect("template");
        QueryPaginationResumeData {
            query: plasm_core::QueryExpr::all("Pet"),
            capability_name: "list".into(),
            env: CmlEnv::new(),
            template,
            config: PaginationConfig {
                params: indexmap! {
                    "page".into() => PaginationParam::Counter { counter: 0, step: 1, max: None },
                },
                location: PaginationLocation::Query,
                body_merge_path: None,
                response_prefix: None,
                stop_when: None,
                response_next_url_field: None,
            },
            state: QueryPaginationState {
                param_values: vec![("page".into(), Some(serde_json::json!(0)))],
                next_absolute_url: None,
                last_requested_limit: 10,
                from_block: None,
                final_to_block: None,
                last_requested_to_block: None,
            },
        }
    }

    #[test]
    fn parallel_async_operations_same_session() {
        let cgs = Arc::new(CGS::new());
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", cgs.clone())),
        );
        let es = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs,
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            "hash".into(),
            None,
            None,
        );
        let h1 = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
        let h2 = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
        es.try_begin_async_operation(
            h1,
            plasm_runtime::CancelSignal::new(),
            crate::operation::OpAcceptContext::default(),
        )
        .expect("first async op");
        es.try_begin_async_operation(
            h2,
            plasm_runtime::CancelSignal::new(),
            crate::operation::OpAcceptContext::default(),
        )
        .expect("second parallel async op");
    }

    #[test]
    fn running_ops_cap_rejects_when_exceeded() {
        let prev = env::var("PLASM_MAX_RUNNING_OPS_PER_SESSION").ok();
        unsafe {
            env::set_var("PLASM_MAX_RUNNING_OPS_PER_SESSION", "2");
        }
        let cgs = Arc::new(CGS::new());
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", cgs.clone())),
        );
        let es = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs,
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            "hash".into(),
            None,
            None,
        );
        for i in 0..2 {
            let h = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
            es.try_begin_async_operation(
                h,
                plasm_runtime::CancelSignal::new(),
                crate::operation::OpAcceptContext::default(),
            )
            .unwrap_or_else(|_| panic!("register op {i}"));
        }
        let h3 = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
        let err = es
            .try_begin_async_operation(
                h3,
                plasm_runtime::CancelSignal::new(),
                crate::operation::OpAcceptContext::default(),
            )
            .expect_err("cap");
        assert!(err.contains("too_many_operations"), "unexpected: {err}");
        assert!(err.contains("wait(") && err.contains("cancel("));
        match prev {
            Some(v) => unsafe {
                env::set_var("PLASM_MAX_RUNNING_OPS_PER_SESSION", v);
            },
            None => unsafe {
                env::remove_var("PLASM_MAX_RUNNING_OPS_PER_SESSION");
            },
        }
    }

    #[test]
    fn sync_live_run_allowed_while_async_running() {
        let cgs = Arc::new(CGS::new());
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", cgs.clone())),
        );
        let es = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs,
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            "hash".into(),
            None,
            None,
        );
        let h = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
        es.try_begin_async_operation(
            h,
            plasm_runtime::CancelSignal::new(),
            crate::operation::OpAcceptContext::default(),
        )
        .expect("async op");
        es.begin_sync_live_run().expect("sync while async running");
        es.end_sync_live_run();
    }

    #[test]
    fn finalize_after_cancel_keeps_cancelled_phase() {
        let cgs = Arc::new(CGS::new());
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", cgs.clone())),
        );
        let es = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs,
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            "hash".into(),
            None,
            None,
        );
        let handle = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
        es.try_begin_async_operation(
            handle.clone(),
            plasm_runtime::CancelSignal::new(),
            crate::operation::OpAcceptContext::default(),
        )
        .expect("register op");
        assert!(es.cancel_operation(&handle, None));
        es.finalize_operation_succeeded(
            &handle,
            crate::plasm_plan_run::PlasmPlanRunResult {
                version: serde_json::json!({}),
                node_results: Vec::new(),
                graph_summary: serde_json::json!({}),
                comp: serde_json::json!({}),
                code_plan_run_artifacts: Vec::new(),
                run_markdown: None,
                run_plasm_meta: None,
                return_steps: Vec::new(),
            },
            None,
        );
        let snap = es.get_operation_poll_snapshot(&handle).expect("snapshot");
        assert!(matches!(
            snap,
            crate::operation::OperationPollSnapshot::Cancelled(_)
        ));
    }
}
