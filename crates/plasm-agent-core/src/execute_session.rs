//! In-memory execute sessions: prompt text + CGS + entity seeds, keyed by `(prompt_hash, session_id)`.
//! Plasm instructions text is built incrementally via [`plasm_core::DomainExposureSession`] (monotonic `e#`/`m#`/`p#`).

use indexmap::IndexMap;
use plasm_core::CgsContext;
use plasm_core::DomainExposureSession;
use plasm_core::FederationDispatch;
use plasm_core::PagingHandle;
use plasm_core::CGS;
use plasm_plugin_host::LoadedPluginGeneration;
use plasm_runtime::{CachedEntity, GraphCache, MutexGraphCacheSession, QueryPaginationResumeData};
use std::collections::{HashMap, VecDeque};
use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::{sleep, Duration as TokioDuration};

use crate::execute_path_ids::{ExecuteSessionId, PromptHashHex};
use crate::run_artifacts::{ArtifactPayload, RunArtifactStore};
use crate::session_graph_persistence::SessionGraphPersistence;
use uuid::Uuid;

/// Default time-to-live for a session (lazy expiry on lookup).
const SESSION_TTL: Duration = Duration::from_secs(3600);

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
    order: VecDeque<Uuid>,
    map: HashMap<Uuid, Arc<SessionRunArtifact>>,
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
        run_id: Uuid,
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

    fn get(&self, run_id: Uuid) -> Option<Arc<SessionRunArtifact>> {
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
}

/// Key for deduplicating execute sessions: same registry `entry_id`, same entity seed set, and
/// (in delegated auth mode) the same [`ExecuteSession::principal`].
///
/// When set, [`Self::logical_session_id`] scopes reuse to one MCP agent logical session (distinct
/// from MCP transport `MCP-Session-Id`).
///
/// [`Self::context_intent`] participates in reuse when set (MCP `plasm_context`): distinct intents
/// must not share an execute row whose DOMAIN symbols were filtered for a different wording.
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
    /// Normalized first-open `plasm_context` intent when capability-scoped DOMAIN is active.
    pub context_intent: Option<String>,
    /// Sorted deduped capability wire names for ranked mutation gating when intent-scoped DOMAIN is active.
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
    pub run_id: Uuid,
    /// Monotonic per execute session; matches `RunArtifactDocument.resource_index` and `plasm://r/{n}`.
    pub resource_index: u64,
    pub seq: DeltaSeq,
    pub epoch: GraphEpoch,
    pub payload: ArtifactPayload,
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

    pub async fn append_run_artifact(
        &self,
        run_id: Uuid,
        epoch: GraphEpoch,
        resource_index: u64,
        payload: ArtifactPayload,
    ) -> Arc<SessionRunArtifact> {
        let mut g = self.state.lock().await;
        g.seq.0 += 1;
        let seq = g.seq;
        let (item, evicted) = g
            .run_artifacts
            .insert(run_id, epoch, resource_index, seq, payload);
        if evicted > 0 {
            crate::metrics::record_run_artifact_hot_cache_evictions(evicted);
        }
        item
    }

    pub async fn get_run_artifact(&self, run_id: Uuid) -> Option<Arc<SessionRunArtifact>> {
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
    /// via [`crate::http_execute::expand_execute_domain_session`], matching [`Self::domain_exposure`].entities).
    pub entities: Vec<String>,
    /// Monotonic symbol map for incremental exposure + expression expand (exact seeds, expanded in waves).
    pub domain_exposure: Option<DomainExposureSession>,
    /// Increments on each successful [`expand_execute_domain_session`] wave.
    pub domain_revision: u32,
    /// End-user / tenant id when using delegated credential resolution (`PLASM_AUTH_RESOLUTION=delegated`).
    pub principal: Option<String>,
    /// Pins [`plasm_plugin_host::LoadedPluginGeneration`] for compile overrides (hot-swap safe).
    pub plugin_generation: Option<Arc<LoadedPluginGeneration>>,
    /// Canonical digest of the pinned primary CGS at session open.
    pub catalog_cgs_hash: String,
    /// Normalized MCP `plasm_context` intent when DOMAIN uses intent-scoped capability exposure (`None` = legacy full closure).
    pub context_intent: Option<String>,
    /// Optional ranked capability-name gate for mutators (aligned with [`SessionReuseKey::ranked_capabilities`]).
    pub ranked_capabilities: Option<Vec<String>>,
    /// Share-link / instance token bound once per execute session (Bearer + optional `share_token` CML env).
    pub session_share_token: Arc<RwLock<Option<String>>>,
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
        domain_exposure: Option<DomainExposureSession>,
        principal: Option<String>,
        plugin_generation: Option<Arc<LoadedPluginGeneration>>,
        catalog_cgs_hash: String,
        context_intent: Option<String>,
        ranked_capabilities: Option<Vec<String>>,
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
            domain_exposure,
            domain_revision: 0,
            principal,
            plugin_generation,
            catalog_cgs_hash,
            context_intent,
            ranked_capabilities,
            session_share_token: Arc::new(RwLock::new(None)),
            graph_cache: core.graph_cache(),
            core,
            run_resource_next: Arc::new(AtomicU64::new(0)),
            paging_resume_by_handle: Arc::new(StdMutex::new(HashMap::new())),
            paging_handle_next: Arc::new(AtomicU64::new(0)),
            paging_op_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Allocate the next monotonic `resource_index` for this execute session (used for `plasm://r/{n}`).
    pub fn mint_run_resource_index(&self) -> u64 {
        self.run_resource_next.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Mint a paging handle and store `resume` for subsequent `page(...)` expressions.
    /// - `logical_session_ref: None` — plain `pgN` (HTTP execute).
    /// - `Some("s0")` — namespaced `s0_pgN` (MCP `plasm` with `logical_session_ref` on the trace).
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

    /// Multi-catalog dispatch for execute (HTTP backend + auth per owning graph).
    pub fn federation_dispatch(&self) -> Option<Arc<FederationDispatch>> {
        if self.contexts_by_entry.len() <= 1 {
            return None;
        }
        let exp = self.domain_exposure.as_ref()?;
        Some(Arc::new(FederationDispatch::from_contexts_and_exposure(
            self.contexts_by_entry.clone(),
            exp,
        )))
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
                    run_id = %a.run_id,
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
        let cache = sess.graph_cache.lock().await;
        persistence
            .write_snapshot(
                sess.prompt_hash.as_str(),
                session_id,
                through_seq,
                "application/json",
                &cache,
            )
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_artifacts::{ArtifactPayload, ArtifactPayloadMetadata};
    use plasm_core::CgsContext;
    use plasm_core::CGS;
    use uuid::Uuid;

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
        let run_id = Uuid::new_v4();
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
                    "page".into() => PaginationParam::Counter { counter: 0, step: 1 },
                },
                location: PaginationLocation::Query,
                body_merge_path: None,
                response_prefix: None,
                stop_when: None,
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
}
