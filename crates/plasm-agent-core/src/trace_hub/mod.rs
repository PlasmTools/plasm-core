//! Project-scoped MCP session traces for demo/debug UX: in-memory capture, summaries, and SSE fan-out.
//!
//! Traces for MCP tools are keyed by **agent logical session id** (see [`Self::ensure_logical_session`]):
//! a **name-based UUID (v5)** over **`(tenant_id, logical_session_id)`**. MCP transport
//! `MCP-Session-Id` is stored only as a **correlation** field on summaries and durable audit payloads.
//! Legacy [`Self::ensure_session`] still keys by transport id for tests. Direct HTTP execute uses a
//! separate v5 root over **`(tenant_id, prompt_hash, execute_session_id)`**.
//!
//! **Versioning:** v2 preimages (namespaces `TRACE_ID_NS_*_V2`) supersede earlier single-field
//! roots; see repository file `docs/mcp-trace-correlation.md`.
//! `project_slug` is reserved for future control-plane linkage; v1 uses `"main"`.
//!
//! **Naming:** this module is **MCP session JSON traces** (not Tower HTTP request logging nor OTLP spans).
//!
//! ## Configuration
//!
//! - **[`TraceHubConfig`]** holds numeric [`TraceHubBounds`] (and future policy). Use
//!   [`TraceHubConfig::from_env`] at process start or build from [`TraceHubBuilder`].
//! - **Sanitization:** [`TraceHubBuilder::build`] clamps each bound to at least **1**. [`TraceHub::bounds`]
//!   and [`TraceHub::config`] return the **effective** values after that step (they may differ from
//!   raw input you passed into the builder).
//! - **Durable ingest vs in-memory trace:** when a [`TraceIngestClient`] is configured, segments are
//!   queued to a **bounded** [`tokio::sync::mpsc`] channel before `spawn_emit_mcp_trace_segment`.
//!   **SSE `patch` events are sent before waiting on channel capacity**, so live subscribers never
//!   block on durable backpressure. **MCP / HTTP** code paths `await` [`mpsc::Sender::send`]: when
//!   the queue is full, emitters **wait** (bounded-memory backpressure) rather than silently dropping
//!   durable work—no two-phase commit. If the channel is **closed** (shutdown), the enqueue fails,
//!   `plasm.trace_hub.ingest_enqueue_failed_total` increments, **`tracing::warn!`** fires, and an SSE
//!   **`durable_ingest`** event may follow the `patch`.
//!
//! Environment variables (optional; unset keeps the default for that field; invalid or `0` is ignored):
//! - [`TRACE_HUB_ENV_MAX_COMPLETED`]
//! - [`TRACE_HUB_ENV_SSE_BROADCAST_CAP`]
//! - [`TRACE_HUB_ENV_INGEST_QUEUE_CAP`]
//! - [`TRACE_HUB_ENV_MAX_TIMELINE_EVENTS`]

mod emit;
mod resume;
mod state;

use std::collections::{HashMap, VecDeque};
use std::env;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use plasm_runtime::http_trace::HttpTraceEntry;
use plasm_runtime::ExecutionResult;
#[cfg(test)]
use plasm_runtime::{ExecutionSource, ExecutionStats};
pub use plasm_trace::{
    totals_from_session_data, CodePlanRunArtifactRef, PlasmLineTraceMeta, RunArtifactArchiveRef,
    SessionTraceData, TraceEvent, TraceSegment, TraceTotals,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::trace_sink_emit::TraceIngestClient;

use state::{ActiveTrace, CompletedTrace, TraceHubInner, TraceIngestJob};

/// Back-compat alias for the canonical [`TraceSegment`].
pub type SessionTraceRecord = TraceSegment;
/// Back-compat alias for [`SessionTraceData`].
pub type McpSessionTrace = SessionTraceData;

/// Reference to the tenant MCP configuration that authenticated the transport (API key → config id).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpConfigRef {
    pub config_id: String,
    pub tenant_id: String,
}

/// Metadata attached when a trace session is first opened.
#[derive(Clone, Debug)]
pub struct TraceSessionMeta {
    pub tenant_id: String,
    pub project_slug: String,
    pub mcp_config: Option<McpConfigRef>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TraceSummaryDto {
    pub trace_id: String,
    /// MCP `MCP-Session-Id` transport correlation (empty when unknown).
    pub mcp_session_id: String,
    /// Agent-scoped logical session id when using [`TraceHub::ensure_logical_session`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_session_id: Option<String>,
    pub status: &'static str,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub project_slug: String,
    pub tenant_id: String,
    pub mcp_config: Option<McpConfigRef>,
    pub totals: TraceTotals,
}

#[derive(Clone, Debug, Serialize)]
pub struct TraceDetailDto {
    #[serde(flatten)]
    pub summary: TraceSummaryDto,
    pub records: Vec<serde_json::Value>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TraceSsePayload {
    #[serde(rename = "snapshot")]
    Snapshot {
        seq: u64,
        detail: Box<TraceDetailDto>,
    },
    #[serde(rename = "patch")]
    Patch { seq: u64, record: serde_json::Value },
    /// Emitted when a durable-ingest job could not be enqueued (channel closed). Follows the same
    /// `seq` as the `patch` for correlation. Clients may show a non-fatal “may not persist” banner.
    #[serde(rename = "durable_ingest")]
    DurableIngest {
        seq: u64,
        status: String,
        reason: String,
    },
    #[serde(rename = "terminal")]
    Terminal {
        seq: u64,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        ended_at_ms: Option<u64>,
    },
}

/// Tunable numeric caps for [`TraceHub`] memory, SSE fan-out, and durable ingest backpressure.
///
/// These fields are **sanitized** at [`TraceHubBuilder::build`] (each clamped to at least `1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceHubBounds {
    /// Maximum completed traces retained locally (`VecDeque` cap).
    pub max_completed_traces: usize,
    /// Per-trace SSE [`broadcast`] channel lag capacity.
    pub sse_broadcast_capacity: usize,
    /// Bounded [`mpsc`] capacity for durable trace ingest jobs (see `plasm.trace_hub.ingest_*`).
    /// Full queues **block MCP/HTTP emitters** (`send().await`); they do not drop. Default **512**
    /// (override with [`TRACE_HUB_ENV_INGEST_QUEUE_CAP`]).
    pub ingest_queue_capacity: usize,
    /// Max timeline events per active trace in RAM ([`plasm_trace::SessionTraceData::records`] window).
    pub max_timeline_events: usize,
}

impl Default for TraceHubBounds {
    fn default() -> Self {
        Self {
            max_completed_traces: 256,
            sse_broadcast_capacity: 128,
            ingest_queue_capacity: 512,
            max_timeline_events: plasm_trace::DEFAULT_TRACE_TIMELINE_MAX_EVENTS,
        }
    }
}

impl TraceHubBounds {
    fn sanitized(self) -> Self {
        Self {
            max_completed_traces: self.max_completed_traces.max(1),
            sse_broadcast_capacity: self.sse_broadcast_capacity.max(1),
            ingest_queue_capacity: self.ingest_queue_capacity.max(1),
            max_timeline_events: self.max_timeline_events.max(16),
        }
    }
}

/// Environment key: max completed traces retained in [`TraceHub`] (positive integer).
pub const TRACE_HUB_ENV_MAX_COMPLETED: &str = "PLASM_TRACE_HUB_MAX_COMPLETED";
/// Environment key: per-trace SSE [`broadcast`] lag capacity (positive integer).
pub const TRACE_HUB_ENV_SSE_BROADCAST_CAP: &str = "PLASM_TRACE_HUB_SSE_BROADCAST_CAP";
/// Environment key: durable ingest `mpsc` queue capacity (positive integer).
pub const TRACE_HUB_ENV_INGEST_QUEUE_CAP: &str = "PLASM_TRACE_HUB_INGEST_QUEUE_CAP";
/// Environment key: max MCP trace timeline events retained in RAM per active session (positive integer).
pub const TRACE_HUB_ENV_MAX_TIMELINE_EVENTS: &str = "PLASM_TRACE_TIMELINE_MAX_EVENTS";

fn trace_hub_positive_env_usize(key: &str) -> Option<usize> {
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

/// Operational configuration for [`TraceHub`] (currently [`TraceHubBounds`] only; extensible for policy flags).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TraceHubConfig {
    pub bounds: TraceHubBounds,
}

impl TraceHubConfig {
    /// Merge optional environment variables onto [`TraceHubConfig::default`].
    ///
    /// Each variable is optional. Invalid, empty, or zero values are ignored for that field.
    /// See module docs for stable key names.
    pub fn from_env() -> Self {
        let mut bounds = TraceHubBounds::default();
        if let Some(v) = trace_hub_positive_env_usize(TRACE_HUB_ENV_MAX_COMPLETED) {
            bounds.max_completed_traces = v;
        }
        if let Some(v) = trace_hub_positive_env_usize(TRACE_HUB_ENV_SSE_BROADCAST_CAP) {
            bounds.sse_broadcast_capacity = v;
        }
        if let Some(v) = trace_hub_positive_env_usize(TRACE_HUB_ENV_INGEST_QUEUE_CAP) {
            bounds.ingest_queue_capacity = v;
        }
        if let Some(v) = trace_hub_positive_env_usize(TRACE_HUB_ENV_MAX_TIMELINE_EVENTS) {
            bounds.max_timeline_events = v;
        }
        Self { bounds }
    }
}

/// Configure [`TraceHub`] before construction (effective bounds are fixed for the hub lifetime).
#[derive(Clone, Debug)]
pub struct TraceHubBuilder {
    config: TraceHubConfig,
}

impl Default for TraceHubBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceHubBuilder {
    pub fn new() -> Self {
        Self {
            config: TraceHubConfig::default(),
        }
    }

    pub fn from_config(config: TraceHubConfig) -> Self {
        Self { config }
    }

    pub fn bounds(mut self, bounds: TraceHubBounds) -> Self {
        self.config.bounds = bounds;
        self
    }

    pub fn max_completed_traces(mut self, cap: usize) -> Self {
        self.config.bounds.max_completed_traces = cap;
        self
    }

    pub fn sse_broadcast_capacity(mut self, cap: usize) -> Self {
        self.config.bounds.sse_broadcast_capacity = cap;
        self
    }

    pub fn ingest_queue_capacity(mut self, cap: usize) -> Self {
        self.config.bounds.ingest_queue_capacity = cap;
        self
    }

    /// Build the hub. When `trace_ingest` is [`Some`], a bounded ingest worker is started.
    ///
    /// [`TraceHubBounds`] are **sanitized** here (each field `max(1)`); see [`TraceHub::bounds`].
    pub fn build(
        self,
        trace_ingest: Option<Arc<dyn TraceIngestClient>>,
        local_trace_archive: Option<Arc<crate::local_trace_archive::LocalTraceArchive>>,
    ) -> TraceHub {
        TraceHub::from_parts(trace_ingest, self.config, local_trace_archive)
    }
}
/// Cap stored reasoning text per `plasm` invocation (full `reasoning_chars` may be larger).
const MAX_TRACE_REASONING_CHARS: usize = 8192;

fn truncate_trace_reasoning(s: &str) -> String {
    let count = s.chars().count();
    if count <= MAX_TRACE_REASONING_CHARS {
        return s.to_string();
    }
    let mut t: String = s.chars().take(MAX_TRACE_REASONING_CHARS).collect();
    t.push('…');
    t
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Plasm-specific UUID v5 namespace (v2): MCP `(tenant_id, transport session id)` → trace root.
const TRACE_ID_NS_MCP_TRANSPORT_V2: Uuid =
    Uuid::from_u128(0x018f_b8d5_4e9a_73a7_b0e1_3f2c_1a8b_09d2);

/// MCP logical session: `(tenant_id, logical_session_id)` → trace root (agent-scoped, not transport).
const TRACE_ID_NS_MCP_LOGICAL_V1: Uuid = Uuid::from_u128(0x018f_b8d5_4e9a_73a7_b0e1_3f2c_1a8b_09d4);

/// Plasm-specific UUID v5 namespace (v2): HTTP execute `(tenant_id, prompt_hash, session_id)`.
const TRACE_ID_NS_HTTP_EXECUTE_V2: Uuid =
    Uuid::from_u128(0x018f_b8d5_4e9a_73a7_b0e1_3f2c_1a8b_09d3);

fn trace_tenant_segment(tenant_id: &str) -> &str {
    if tenant_id.is_empty() {
        "anonymous"
    } else {
        tenant_id
    }
}

/// RFC 4122 name-based (v5) trace id for one MCP transport session under a tenant.
///
/// Preimage: `"{tenant_id}\\n{mcp_session_id}"` with empty `tenant_id` treated as `anonymous`,
/// matching [`TraceSessionMeta::tenant_id`] defaults from MCP incoming auth.
pub fn trace_id_for_mcp_transport_session(tenant_id: &str, mcp_session_id: &str) -> Uuid {
    let t = trace_tenant_segment(tenant_id);
    let name = format!("{t}\n{mcp_session_id}");
    Uuid::new_v5(&TRACE_ID_NS_MCP_TRANSPORT_V2, name.as_bytes())
}

/// Trace id for MCP tool traffic keyed by **logical session** (server-minted UUID), not transport.
///
/// Preimage: `"{tenant_id}\\nlogical:{logical_session_id}"` (empty tenant → `anonymous`).
pub fn trace_id_for_mcp_logical_session(tenant_id: &str, logical_session_id: &str) -> Uuid {
    let t = trace_tenant_segment(tenant_id);
    let name = format!("{t}\nlogical:{logical_session_id}");
    Uuid::new_v5(&TRACE_ID_NS_MCP_LOGICAL_V1, name.as_bytes())
}

/// Trace id for direct `POST /execute/...` runs (no MCP transport session).
///
/// Preimage: `"{tenant_id}\\n{prompt_hash}\\n{execute_session_id}"` (empty tenant → `anonymous`).
pub fn trace_id_for_http_execute_session(
    tenant_id: &str,
    prompt_hash: &str,
    execute_session_id: &str,
) -> Uuid {
    let t = trace_tenant_segment(tenant_id);
    let name = format!("{t}\n{prompt_hash}\n{execute_session_id}");
    Uuid::new_v5(&TRACE_ID_NS_HTTP_EXECUTE_V2, name.as_bytes())
}

fn tenant_visible_to_viewer(viewer_tenant_id: Option<&str>, trace_tenant: &str) -> bool {
    match viewer_tenant_id {
        None | Some("") => trace_tenant.is_empty() || trace_tenant == "anonymous",
        Some(v) => trace_tenant == v,
    }
}

async fn trace_ingest_worker(
    mut rx: mpsc::Receiver<TraceIngestJob>,
    ingest: Arc<dyn TraceIngestClient>,
    backlog: Arc<AtomicUsize>,
    queue_cap: i64,
) {
    while let Some(job) = rx.recv().await {
        backlog.fetch_sub(1, Ordering::Relaxed);
        crate::trace_hub_metrics::record_trace_hub_ingest_dequeued(queue_cap);
        let wait_ms = job.enqueued_at.elapsed().as_millis() as u64;
        crate::trace_hub_metrics::record_trace_hub_ingest_queue_wait_ms(wait_ms, queue_cap);
        crate::trace_sink_emit::spawn_emit_mcp_trace_segment(
            ingest.as_ref(),
            &job.fields,
            &job.trace_event,
            job.precomputed_payload,
        );
    }
}

/// Payload for [`TraceHub::trace_record_plasm_context`].
#[derive(Debug, Clone)]
pub struct PlasmContextTrace {
    pub teaching_prompt_chars_added: u64,
    pub reused_session: bool,
    pub mode: String,
    pub entry_id: Option<String>,
    pub entities: Vec<String>,
    pub seeds: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CodePlanTrace {
    pub plan_handle: String,
    pub plan_id: String,
    pub plan_name: String,
    pub plan_hash: String,
    pub plan_uri: String,
    pub canonical_plan_uri: String,
    pub plan_http_path: String,
    pub prompt_hash: String,
    pub session_id: String,
    pub node_count: usize,
    pub code_chars: u64,
    pub comp: serde_json::Value,
    pub dag: serde_json::Value,
    pub plan_ux_reflection: Option<serde_json::Value>,
    pub plasm_call_index: Option<u64>,
    pub run_ids: Vec<String>,
    pub run_artifacts: Vec<CodePlanRunArtifactRef>,
}

/// In-memory trace registry + live SSE fan-out.
pub struct TraceHub {
    inner: RwLock<TraceHubInner>,
    ingest_tx: Option<mpsc::Sender<TraceIngestJob>>,
    config: TraceHubConfig,
    /// Jobs in the ingest `mpsc` not yet received by the worker (incremented after successful `send`).
    ingest_channel_backlog: Arc<AtomicUsize>,
    /// When set, completed sessions are also written to disk (`PLASM_TRACE_ARCHIVE_DIR`) for durable reads.
    local_trace_archive: Option<Arc<crate::local_trace_archive::LocalTraceArchive>>,
}

impl Default for TraceHub {
    fn default() -> Self {
        Self::new(None)
    }
}

impl TraceHub {
    pub fn new(trace_ingest: Option<Arc<dyn TraceIngestClient>>) -> Self {
        TraceHubBuilder::default().build(trace_ingest, None)
    }

    fn from_parts(
        trace_ingest: Option<Arc<dyn TraceIngestClient>>,
        mut config: TraceHubConfig,
        local_trace_archive: Option<Arc<crate::local_trace_archive::LocalTraceArchive>>,
    ) -> Self {
        config.bounds = config.bounds.sanitized();
        let ingest_channel_backlog = Arc::new(AtomicUsize::new(0));
        let queue_cap_i64 = config.bounds.ingest_queue_capacity as i64;
        let ingest_tx = trace_ingest.map(|ingest| {
            let (tx, rx) = mpsc::channel(config.bounds.ingest_queue_capacity);
            let backlog = Arc::clone(&ingest_channel_backlog);
            tokio::spawn(trace_ingest_worker(rx, ingest, backlog, queue_cap_i64));
            tx
        });
        Self {
            inner: RwLock::new(TraceHubInner {
                active: HashMap::new(),
                completed: VecDeque::new(),
                tx_by_trace: HashMap::new(),
            }),
            ingest_tx,
            config,
            ingest_channel_backlog,
            local_trace_archive,
        }
    }

    /// Effective [`TraceHubBounds`] after construction-time sanitization.
    pub fn bounds(&self) -> TraceHubBounds {
        self.config.bounds
    }

    /// Full hub configuration (currently bounds only).
    pub fn config(&self) -> TraceHubConfig {
        self.config
    }

    pub(crate) fn broadcast_tx(
        inner: &mut TraceHubInner,
        trace_id: Uuid,
        sse_broadcast_capacity: usize,
    ) -> broadcast::Sender<String> {
        inner
            .tx_by_trace
            .entry(trace_id)
            .or_insert_with(|| broadcast::channel(sse_broadcast_capacity).0)
            .clone()
    }

    pub async fn subscribe_trace_async(
        &self,
        trace_id: Uuid,
    ) -> Option<broadcast::Receiver<String>> {
        let g = self.inner.read().await;
        g.tx_by_trace.get(&trace_id).map(|tx| tx.subscribe())
    }

    /// Active transport session keys with an in-memory trace (for debug / metrics).
    pub async fn active_mcp_session_count(&self) -> usize {
        let g = self.inner.read().await;
        g.active.len()
    }

    async fn emit_json(&self, trace_id: Uuid, payload: &TraceSsePayload) {
        let json = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
        let g = self.inner.read().await;
        if let Some(tx) = g.tx_by_trace.get(&trace_id) {
            let _ = tx.send(json);
        }
    }

    /// After the SSE `patch` is sent: block the MCP/HTTP caller until the job is accepted into the
    /// bounded `mpsc` (backpressure). Does not hold [`TraceHubInner`] locks across `.await`.
    /// Downstream HTTP / Parquet durability remains async in [`trace_ingest_worker`].
    async fn enqueue_durable_job_after_patch(
        &self,
        tx: &mpsc::Sender<TraceIngestJob>,
        job: TraceIngestJob,
        patch_seq: u64,
    ) {
        let queue_cap = self.config.bounds.ingest_queue_capacity as i64;
        let wait_start = Instant::now();
        match tx.send(job).await {
            Ok(()) => {
                self.ingest_channel_backlog.fetch_add(1, Ordering::Relaxed);
                let wait_ms = wait_start.elapsed().as_millis() as u64;
                crate::trace_hub_metrics::record_trace_hub_ingest_send_wait_ms(wait_ms, queue_cap);
                let depth = self.ingest_channel_backlog.load(Ordering::Relaxed) as u64;
                crate::trace_hub_metrics::record_trace_hub_ingest_accepted(depth, queue_cap);
            }
            Err(e) => {
                let job = e.0;
                crate::trace_hub_metrics::record_trace_hub_ingest_enqueue_failed(
                    "closed", queue_cap,
                );
                tracing::warn!(
                    target: "plasm_agent::trace_hub",
                    trace_id = %job.fields.trace_id,
                    tenant_id = %job.fields.tenant_id.as_deref().unwrap_or(""),
                    queue_reason = "closed",
                    queue_capacity = queue_cap,
                    "durable trace ingest channel closed (SSE patch already delivered)"
                );
                self.emit_json(
                    job.fields.trace_id,
                    &TraceSsePayload::DurableIngest {
                        seq: patch_seq,
                        status: "enqueue_dropped".to_string(),
                        reason: "closed".to_string(),
                    },
                )
                .await;
            }
        }
    }

    /// Ensure an active trace exists for this MCP **transport** session key (legacy / tests).
    ///
    /// `trace_id` is [`trace_id_for_mcp_transport_session`] for `(meta.tenant_id, mcp_key)`.
    /// Prefer [`Self::ensure_logical_session`] for production MCP traffic.
    pub async fn ensure_session(&self, mcp_key: &str, meta: TraceSessionMeta) -> Uuid {
        let trace_id = trace_id_for_mcp_transport_session(meta.tenant_id.as_str(), mcp_key);
        let session_trace_key = mcp_key.to_string();
        loop {
            let eviction = {
                let g = self.inner.read().await;
                match g.active.get(&session_trace_key) {
                    None => false,
                    Some(a) => a.meta.tenant_id != meta.tenant_id || a.trace_id != trace_id,
                }
            };
            if eviction {
                self.finalize_mcp_session(&session_trace_key).await;
                continue;
            }
            break;
        }

        loop {
            let mut g = self.inner.write().await;
            if let Some(a) = g.active.get(&session_trace_key) {
                if a.meta.tenant_id == meta.tenant_id && a.trace_id == trace_id {
                    return trace_id;
                }
                drop(g);
                self.finalize_mcp_session(&session_trace_key).await;
                continue;
            }

            let last_activity_ms = now_ms();
            let cap = self.config.bounds.max_timeline_events;
            let criteria =
                resume::CompletedResumeCriteria::strict(trace_id, meta.tenant_id.as_str());
            let resumed = resume::find_completed_index(&g.completed, &session_trace_key, criteria)
                .and_then(|pos| g.completed.remove(pos));
            let mut active = if let Some(c) = resumed {
                resume::active_from_completed(c, cap, last_activity_ms)
            } else {
                ActiveTrace {
                    trace_id,
                    session_trace_key: session_trace_key.clone(),
                    logical_session_id: None,
                    mcp_transport_session_id: Some(mcp_key.to_string()),
                    meta: meta.clone(),
                    data: SessionTraceData::new_with_timeline_cap(session_trace_key.clone(), cap),
                    started_ms: last_activity_ms,
                    last_activity_ms,
                    seq: 0,
                }
            };
            active.mcp_transport_session_id = Some(mcp_key.to_string());
            active.meta = meta;
            let _tx =
                Self::broadcast_tx(&mut g, trace_id, self.config.bounds.sse_broadcast_capacity);
            g.active.insert(session_trace_key.clone(), active);
            crate::trace_hub_metrics::record_trace_hub_queue_state(
                g.completed.len(),
                g.active.len(),
                false,
                self.config.bounds.max_completed_traces as i64,
            );
            return trace_id;
        }
    }

    /// Ensure an active trace for an MCP **logical session** (agent-scoped), not transport.
    ///
    /// `logical_session_id` is the canonical UUID string from `plasm_context`. `mcp_transport_id`
    /// is optional `MCP-Session-Id` for correlation on summaries and audit payloads.
    pub async fn ensure_logical_session(
        &self,
        logical_session_id: &str,
        mcp_transport_id: Option<&str>,
        meta: TraceSessionMeta,
    ) -> Uuid {
        let trace_id =
            trace_id_for_mcp_logical_session(meta.tenant_id.as_str(), logical_session_id);
        let session_trace_key = logical_session_id.to_string();
        loop {
            let eviction = {
                let g = self.inner.read().await;
                match g.active.get(&session_trace_key) {
                    None => false,
                    Some(a) => a.meta.tenant_id != meta.tenant_id || a.trace_id != trace_id,
                }
            };
            if eviction {
                self.finalize_mcp_session(&session_trace_key).await;
                continue;
            }
            break;
        }

        loop {
            let mut g = self.inner.write().await;
            if let Some(a) = g.active.get(&session_trace_key) {
                if a.meta.tenant_id == meta.tenant_id && a.trace_id == trace_id {
                    return trace_id;
                }
                drop(g);
                self.finalize_mcp_session(&session_trace_key).await;
                continue;
            }

            let last_activity_ms = now_ms();
            let cap = self.config.bounds.max_timeline_events;
            let criteria =
                resume::CompletedResumeCriteria::strict(trace_id, meta.tenant_id.as_str());
            let resumed = resume::find_completed_index(&g.completed, &session_trace_key, criteria)
                .and_then(|pos| g.completed.remove(pos));
            let mut active = if let Some(c) = resumed {
                resume::active_from_completed(c, cap, last_activity_ms)
            } else {
                ActiveTrace {
                    trace_id,
                    session_trace_key: session_trace_key.clone(),
                    logical_session_id: Some(logical_session_id.to_string()),
                    mcp_transport_session_id: mcp_transport_id.map(str::to_string),
                    meta: meta.clone(),
                    data: SessionTraceData::new_with_timeline_cap(session_trace_key.clone(), cap),
                    started_ms: last_activity_ms,
                    last_activity_ms,
                    seq: 0,
                }
            };
            active.logical_session_id = Some(logical_session_id.to_string());
            active.mcp_transport_session_id = mcp_transport_id.map(str::to_string);
            active.meta = meta;
            let _tx =
                Self::broadcast_tx(&mut g, trace_id, self.config.bounds.sse_broadcast_capacity);
            g.active.insert(session_trace_key.clone(), active);
            crate::trace_hub_metrics::record_trace_hub_queue_state(
                g.completed.len(),
                g.active.len(),
                false,
                self.config.bounds.max_completed_traces as i64,
            );
            return trace_id;
        }
    }

    fn summary_dto_from_active(a: &ActiveTrace) -> TraceSummaryDto {
        TraceSummaryDto {
            trace_id: a.trace_id.to_string(),
            mcp_session_id: a.mcp_transport_session_id.clone().unwrap_or_default(),
            logical_session_id: a.logical_session_id.clone(),
            status: "live",
            started_at_ms: a.started_ms,
            ended_at_ms: None,
            project_slug: a.meta.project_slug.clone(),
            tenant_id: a.meta.tenant_id.clone(),
            mcp_config: a.meta.mcp_config.clone(),
            totals: totals_from_session_data(&a.data),
        }
    }

    fn detail_dto_from_active(a: &ActiveTrace) -> Option<TraceDetailDto> {
        let summary = Self::summary_dto_from_active(a);
        Some(Self::detail_dto_from_session(summary, &a.data))
    }

    fn completed_to_summary(c: &CompletedTrace) -> TraceSummaryDto {
        TraceSummaryDto {
            trace_id: c.trace_id.to_string(),
            mcp_session_id: c.mcp_transport_session_id.clone().unwrap_or_default(),
            logical_session_id: c.logical_session_id.clone(),
            status: "completed",
            started_at_ms: c.started_ms,
            ended_at_ms: Some(c.ended_ms),
            project_slug: c.meta.project_slug.clone(),
            tenant_id: c.meta.tenant_id.clone(),
            mcp_config: c.meta.mcp_config.clone(),
            totals: totals_from_session_data(&c.data),
        }
    }

    fn completed_to_detail(c: &CompletedTrace) -> TraceDetailDto {
        let summary = Self::completed_to_summary(c);
        Self::detail_dto_from_session(summary, &c.data)
    }

    fn detail_dto_from_session(
        summary: TraceSummaryDto,
        data: &SessionTraceData,
    ) -> TraceDetailDto {
        let records: Vec<serde_json::Value> = data
            .records
            .iter()
            .filter_map(|r| serde_json::to_value(r).ok())
            .collect();
        TraceDetailDto { summary, records }
    }

    /// List traces visible to this tenant (incoming-auth tenant id, or `"anonymous"` bucket).
    pub async fn list_for_tenant(
        &self,
        viewer_tenant_id: Option<&str>,
        project_slug: Option<&str>,
        offset: usize,
        limit: usize,
        status: TraceListStatus,
    ) -> Vec<TraceSummaryDto> {
        let lim = limit.clamp(1, 200);

        let tenant_ok = |t: &str| tenant_visible_to_viewer(viewer_tenant_id, t);
        let project_ok = |p: &str| match project_slug {
            None | Some("") => p == "main" || p.is_empty(),
            Some(want) => p == want,
        };
        let (active, completed) = {
            let g = self.inner.read().await;
            let active: Vec<ActiveTrace> = g
                .active
                .values()
                .filter(|a| tenant_ok(&a.meta.tenant_id) && project_ok(&a.meta.project_slug))
                .cloned()
                .collect();
            let completed: Vec<CompletedTrace> = g
                .completed
                .iter()
                .filter(|c| tenant_ok(&c.meta.tenant_id) && project_ok(&c.meta.project_slug))
                .cloned()
                .collect();
            (active, completed)
        };
        let mut out: Vec<TraceSummaryDto> = Vec::new();

        for a in &active {
            let st = "live";
            match status {
                TraceListStatus::All => {}
                TraceListStatus::Live if st != "live" => continue,
                TraceListStatus::Completed if st != "completed" => continue,
                _ => {}
            }
            out.push(Self::summary_dto_from_active(a));
        }
        for c in completed.iter().rev() {
            if status == TraceListStatus::Live {
                continue;
            }
            out.push(Self::completed_to_summary(c));
        }
        out.sort_by_key(|t| std::cmp::Reverse(t.started_at_ms));
        out.into_iter().skip(offset).take(lim).collect()
    }

    pub async fn get_detail(
        &self,
        trace_id: Uuid,
        viewer_tenant_id: Option<&str>,
    ) -> Option<TraceDetailDto> {
        let tenant_ok = |t: &str| tenant_visible_to_viewer(viewer_tenant_id, t);
        let selected = {
            let g = self.inner.read().await;
            if let Some(a) = g
                .active
                .values()
                .find(|a| a.trace_id == trace_id && tenant_ok(&a.meta.tenant_id))
                .cloned()
            {
                return Self::detail_dto_from_active(&a);
            }
            g.completed
                .iter()
                .find(|c| c.trace_id == trace_id && tenant_ok(&c.meta.tenant_id))
                .cloned()
        };
        selected.map(|c| Self::completed_to_detail(&c))
    }

    /// Finalize every active trace whose hub key (logical session id or legacy transport key) is
    /// **not** in `live_trace_session_keys` (no active MCP transport still holds that session).
    pub async fn finalize_disconnected_sessions(
        &self,
        live_trace_session_keys: &std::collections::HashSet<String>,
    ) -> Vec<String> {
        let stale: Vec<String> = {
            let g = self.inner.read().await;
            g.active
                .keys()
                .filter(|k| !live_trace_session_keys.contains(k.as_str()))
                .cloned()
                .collect()
        };
        for k in &stale {
            self.finalize_mcp_session(k).await;
        }
        stale
    }

    /// Finalize active traces whose key is **still** in `live_trace_session_keys` but have had no
    /// activity for `idle_ms` (0 = disabled). The next tool activity **resumes** the same trace root.
    pub async fn finalize_idle_traces(
        &self,
        live_trace_session_keys: &std::collections::HashSet<String>,
        idle_ms: u64,
    ) -> Vec<String> {
        if idle_ms == 0 {
            return Vec::new();
        }
        let now = now_ms();
        let stale: Vec<String> = {
            let g = self.inner.read().await;
            g.active
                .iter()
                .filter(|(k, a)| {
                    live_trace_session_keys.contains(k.as_str())
                        && now.saturating_sub(a.last_activity_ms) >= idle_ms
                })
                .map(|(k, _)| k.clone())
                .collect()
        };
        for k in &stale {
            self.finalize_mcp_session(k).await;
        }
        stale
    }

    pub async fn finalize_mcp_session(&self, trace_session_key: &str) {
        let (trace_id, seq, ended_ms, detail_for_archive) = {
            let mut g = self.inner.write().await;
            let Some(active) = g.active.remove(trace_session_key) else {
                return;
            };
            let ended_ms = now_ms();
            let seq = active.seq.saturating_add(1);
            let trace_id = active.trace_id;
            let completed = CompletedTrace {
                trace_id,
                session_trace_key: active.session_trace_key.clone(),
                logical_session_id: active.logical_session_id.clone(),
                mcp_transport_session_id: active.mcp_transport_session_id.clone(),
                meta: active.meta.clone(),
                data: active.data.clone(),
                started_ms: active.started_ms,
                ended_ms,
                last_seq_emitted: seq,
            };
            let detail_for_archive = self
                .local_trace_archive
                .as_ref()
                .map(|_| Self::completed_to_detail(&completed));
            let evicted_oldest_completed =
                g.completed.len() >= self.config.bounds.max_completed_traces;
            if evicted_oldest_completed {
                g.completed.pop_front();
            }
            g.completed.push_back(completed);
            g.tx_by_trace.remove(&trace_id);
            let completed_len = g.completed.len();
            let active_len = g.active.len();
            crate::trace_hub_metrics::record_trace_hub_queue_state(
                completed_len,
                active_len,
                evicted_oldest_completed,
                self.config.bounds.max_completed_traces as i64,
            );
            (trace_id, seq, ended_ms, detail_for_archive)
        };
        if let (Some(arch), Some(detail)) = (self.local_trace_archive.as_ref(), detail_for_archive)
        {
            let arch = arch.clone();
            tokio::spawn(async move {
                if let Err(e) = arch.persist_trace(&detail).await {
                    tracing::warn!(
                        target: "plasm_agent::trace_hub",
                        error = %e,
                        "PLASM_TRACE_ARCHIVE_DIR: failed to persist completed trace (non-fatal)"
                    );
                }
            });
        }
        self.emit_json(
            trace_id,
            &TraceSsePayload::Terminal {
                seq,
                status: "completed".into(),
                ended_at_ms: Some(ended_ms),
            },
        )
        .await;
    }

    pub async fn trace_note_teaching_prompt_chars(&self, mcp_key: &str, chars_added: u64) {
        if chars_added == 0 {
            return;
        }
        self.bump_and_emit(
            mcp_key,
            TraceSegment::TeachingPromptCharsDelta { chars_added },
        )
        .await;
    }

    pub async fn trace_record_plasm_context(&self, mcp_key: &str, trace: PlasmContextTrace) {
        self.bump_and_emit(
            mcp_key,
            TraceSegment::PlasmContext {
                teaching_prompt_chars_added: trace.teaching_prompt_chars_added,
                reused_session: trace.reused_session,
                mode: trace.mode,
                entry_id: trace.entry_id,
                entities: trace.entities,
                seeds: trace.seeds,
            },
        )
        .await;
    }

    pub async fn trace_record_expand_domain(
        &self,
        mcp_key: &str,
        teaching_prompt_chars_added: u64,
        entry_id: Option<String>,
        entities: Vec<String>,
        seeds: Vec<String>,
    ) {
        self.bump_and_emit(
            mcp_key,
            TraceSegment::ExpandDomain {
                teaching_prompt_chars_added,
                entry_id,
                entities,
                seeds,
            },
        )
        .await;
    }

    pub async fn trace_record_code_plan_evaluate(&self, mcp_key: &str, trace: CodePlanTrace) {
        self.bump_and_emit(
            mcp_key,
            TraceSegment::CodePlanEvaluate {
                plan_handle: trace.plan_handle,
                plan_id: trace.plan_id,
                plan_name: trace.plan_name,
                plan_hash: trace.plan_hash,
                plan_uri: trace.plan_uri,
                canonical_plan_uri: trace.canonical_plan_uri,
                plan_http_path: trace.plan_http_path,
                prompt_hash: trace.prompt_hash,
                session_id: trace.session_id,
                node_count: trace.node_count,
                code_chars: trace.code_chars,
                comp: Some(trace.comp),
                dag: Some(trace.dag),
                plan_ux_reflection: trace.plan_ux_reflection,
            },
        )
        .await;
    }

    pub async fn trace_record_code_plan_execute(&self, mcp_key: &str, trace: CodePlanTrace) {
        self.bump_and_emit(
            mcp_key,
            TraceSegment::CodePlanExecute {
                plan_handle: trace.plan_handle,
                plan_id: trace.plan_id,
                plan_name: trace.plan_name,
                plan_hash: trace.plan_hash,
                plan_uri: trace.plan_uri,
                canonical_plan_uri: trace.canonical_plan_uri,
                plan_http_path: trace.plan_http_path,
                prompt_hash: trace.prompt_hash,
                session_id: trace.session_id,
                node_count: trace.node_count,
                code_chars: trace.code_chars,
                comp: Some(trace.comp),
                dag: Some(trace.dag),
                plasm_call_index: trace.plasm_call_index,
                run_ids: trace.run_ids,
                run_artifacts: trace.run_artifacts,
                plan_ux_reflection: trace.plan_ux_reflection,
            },
        )
        .await;
    }

    /// Start of a `plasm` tool invocation. Returns monotonic `call_index` for line records.
    ///
    /// Intentionally not routed through [`Self::bump_and_emit`]: `call_index` allocation, event push,
    /// and `seq` ordering must stay aligned with subsequent [`Self::trace_add_plasm_line`] / error rows.
    pub async fn trace_record_plasm_invocation(
        &self,
        mcp_key: &str,
        multi_line: bool,
        expression_count: usize,
        reasoning_chars: Option<u64>,
        plasm_invocation_chars_added: u64,
        reasoning: Option<String>,
    ) -> u64 {
        if !self.ensure_active_for_emit(mcp_key).await {
            tracing::warn!(
                target: "plasm_agent::trace_hub",
                mcp_key,
                "plasm_invocation trace dropped: no active or resumable completed trace"
            );
            crate::trace_hub_metrics::record_trace_emit_dropped_no_active();
            return 0;
        }
        let reasoning_stored = reasoning
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(truncate_trace_reasoning);
        let (call_index, trace_id, seq, record, job_opt) = {
            let mut g = self.inner.write().await;
            let Some(a) = g.active.get_mut(mcp_key) else {
                crate::trace_hub_metrics::record_trace_emit_dropped_no_active();
                return 0;
            };
            let next_call = a.data.plasm_call_count.saturating_add(1);
            let ev = TraceEvent::at(
                now_ms(),
                TraceSegment::PlasmInvocation {
                    call_index: next_call,
                    multi_line,
                    expression_count,
                    plasm_invocation_chars_added,
                    reasoning_chars,
                    reasoning: reasoning_stored.clone(),
                },
            );
            let dropped = a.data.push_event(ev);
            if dropped > 0 {
                crate::metrics::record_trace_timeline_events_dropped(dropped);
            }
            let ev_ref = a
                .data
                .records
                .back()
                .expect("push_event always appends one record");
            let record = serde_json::to_value(ev_ref).unwrap_or_default();
            let job_opt = self.ingest_tx.as_ref().map(|_| {
                TraceIngestJob::new_mcp_active_segment(a, ev_ref.clone(), Some(record.clone()))
            });
            a.last_activity_ms = now_ms();
            a.seq = a.seq.saturating_add(1);
            let seq = a.seq;
            let trace_id = a.trace_id;
            (next_call, trace_id, seq, record, job_opt)
        };
        self.emit_json(trace_id, &TraceSsePayload::Patch { seq, record })
            .await;
        if let (Some(tx), Some(job)) = (self.ingest_tx.as_ref(), job_opt) {
            self.enqueue_durable_job_after_patch(tx, job, seq).await;
        }
        call_index
    }

    pub async fn trace_note_plasm_response_chars(
        &self,
        mcp_key: &str,
        chars: u64,
        tool: &str,
        call_index: u64,
        multi_line: bool,
        expression_count: usize,
    ) {
        if chars == 0 {
            return;
        }
        self.bump_and_emit(
            mcp_key,
            TraceSegment::PlasmResponseCharsDelta {
                chars_added: chars,
                tool: tool.to_string(),
                call_index: Some(call_index),
                multi_line,
                expression_count: Some(expression_count),
            },
        )
        .await;
    }

    /// MCP `resources/read` timeline row (payload size + archive ref for future web deep-links).
    #[allow(clippy::too_many_arguments)]
    pub async fn trace_record_mcp_resource_read(
        &self,
        mcp_key: &str,
        archive: Option<RunArtifactArchiveRef>,
        uri_display: String,
        chars_added: u64,
        is_binary: bool,
        duration_ms: u64,
        result: &str,
        error_class: Option<&str>,
    ) {
        self.bump_and_emit(
            mcp_key,
            TraceSegment::McpResourceRead {
                archive,
                uri_display,
                chars_added,
                is_binary,
                duration_ms,
                result: result.to_string(),
                error_class: error_class.map(str::to_string),
            },
        )
        .await;
    }

    pub async fn trace_add_plasm_line(
        &self,
        mcp_key: &str,
        call_index: u64,
        line_index: usize,
        meta: PlasmLineTraceMeta,
        result: &ExecutionResult,
        http_calls: Vec<HttpTraceEntry>,
    ) {
        let rec = TraceSegment::PlasmLine {
            call_index,
            line_index,
            source_expression: meta.source_expression,
            repl_pre: meta.repl_pre,
            repl_post: meta.repl_post,
            capability: meta.capability,
            operation: meta.operation,
            api_entry_id: meta.api_entry_id,
            duration_ms: result.stats.duration_ms,
            stats: result.stats.clone(),
            source: result.source,
            request_fingerprints: result.request_fingerprints.clone(),
            http_calls,
        };
        self.bump_and_emit(mcp_key, rec).await;
    }

    pub async fn trace_add_plasm_error(
        &self,
        mcp_key: &str,
        call_index: u64,
        line_index: Option<usize>,
        message: String,
    ) {
        self.bump_and_emit(
            mcp_key,
            TraceSegment::PlasmError {
                call_index,
                line_index,
                message,
            },
        )
        .await;
    }

    /// Initial SSE payload after subscribe: full detail snapshot.
    ///
    /// `seq` matches the latest emitted patch (or [`CompletedTrace::last_seq_emitted`] after
    /// terminal) so clients can align snapshot ordering with the patch stream.
    pub async fn sse_snapshot_payload(
        &self,
        trace_id: Uuid,
        viewer_tenant_id: Option<&str>,
    ) -> Option<String> {
        let seq = {
            let g = self.inner.read().await;
            if let Some(a) = g.active.values().find(|a| a.trace_id == trace_id) {
                a.seq
            } else {
                g.completed
                    .iter()
                    .filter(|c| {
                        c.trace_id == trace_id
                            && tenant_visible_to_viewer(viewer_tenant_id, &c.meta.tenant_id)
                    })
                    .max_by_key(|c| c.ended_ms)
                    .map(|c| c.last_seq_emitted)
                    .unwrap_or(0)
            }
        };
        let detail = self.get_detail(trace_id, viewer_tenant_id).await?;
        let payload = TraceSsePayload::Snapshot {
            seq,
            detail: Box::new(detail),
        };
        serde_json::to_string(&payload).ok()
    }
}

/// Passed into [`crate::http_execute::execute_session_run_markdown`] to append [`TraceSegment::PlasmLine`] rows.
#[derive(Clone)]
pub struct McpPlasmTraceSink {
    pub hub: Arc<TraceHub>,
    pub mcp_key: String,
    pub call_index: u64,
}

/// Cloneable trace hooks for async MCP plan runs (carried on [`crate::operation::OpAcceptContext`]).
#[derive(Clone)]
pub struct PlanRunTraceHooks {
    pub trace: crate::trace_sink_emit::PlasmTraceContext,
    pub sink: McpPlasmTraceSink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceListStatus {
    All,
    Live,
    Completed,
}

impl TraceListStatus {
    pub fn parse(s: Option<&str>) -> Self {
        match s.unwrap_or("all").to_ascii_lowercase().as_str() {
            "live" => TraceListStatus::Live,
            "completed" => TraceListStatus::Completed,
            _ => TraceListStatus::All,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_hub_builder_clamps_zero_bounds() {
        let hub = TraceHubBuilder::new()
            .max_completed_traces(0)
            .sse_broadcast_capacity(0)
            .ingest_queue_capacity(0)
            .build(None, None);
        let b = hub.bounds();
        assert_eq!(b.max_completed_traces, 1);
        assert_eq!(b.sse_broadcast_capacity, 1);
        assert_eq!(b.ingest_queue_capacity, 1);
    }

    #[test]
    fn plasm_context_record_serializes_metadata() {
        let r = TraceSegment::PlasmContext {
            teaching_prompt_chars_added: 12,
            reused_session: false,
            mode: "federate".into(),
            entry_id: Some("linear".into()),
            entities: vec!["Issue".into(), "Team".into()],
            seeds: vec!["linear:Issue".into(), "linear:Team".into()],
        };
        let v = serde_json::to_value(&r).expect("json");
        assert_eq!(v.get("kind"), Some(&serde_json::json!("plasm_context")));
        assert_eq!(v.get("mode"), Some(&serde_json::json!("federate")));
        assert_eq!(v.get("entry_id"), Some(&serde_json::json!("linear")));
        assert_eq!(
            v.get("entities"),
            Some(&serde_json::json!(["Issue", "Team"]))
        );
    }

    #[test]
    fn expand_domain_record_serializes_metadata() {
        let r = TraceSegment::ExpandDomain {
            teaching_prompt_chars_added: 8,
            entry_id: Some("petstore".into()),
            entities: vec!["Order".into()],
            seeds: vec!["petstore:Order".into()],
        };
        let v = serde_json::to_value(&r).expect("json");
        assert_eq!(v.get("kind"), Some(&serde_json::json!("expand_domain")));
        assert_eq!(v.get("entry_id"), Some(&serde_json::json!("petstore")));
        assert_eq!(v.get("entities"), Some(&serde_json::json!(["Order"])));
    }

    #[test]
    fn plasm_line_record_serializes() {
        let r = TraceSegment::PlasmLine {
            call_index: 1,
            line_index: 0,
            source_expression: "Pet.query".into(),
            repl_pre: String::new(),
            repl_post: String::new(),
            capability: Some("pet_query".into()),
            operation: "query".into(),
            api_entry_id: Some("petstore".into()),
            duration_ms: 5,
            stats: ExecutionStats {
                duration_ms: 5,
                network_requests: 1,
                cache_hits: 0,
                cache_misses: 1,
                ..Default::default()
            },
            source: ExecutionSource::Live,
            request_fingerprints: vec!["ab".into()],
            http_calls: vec![],
        };
        let v = serde_json::to_value(&r).expect("json");
        assert_eq!(v.get("kind"), Some(&serde_json::json!("plasm_line")));
        assert_eq!(v.get("capability"), Some(&serde_json::json!("pet_query")));
        assert_eq!(v.get("operation"), Some(&serde_json::json!("query")));
        assert_eq!(v.get("api_entry_id"), Some(&serde_json::json!("petstore")));
    }

    #[test]
    fn mcp_transport_trace_id_is_stable_per_tenant_and_session() {
        let a = trace_id_for_mcp_transport_session("tenant-1", "mcp-session-abc");
        let b = trace_id_for_mcp_transport_session("tenant-1", "mcp-session-abc");
        let c = trace_id_for_mcp_transport_session("tenant-1", "mcp-session-xyz");
        let d = trace_id_for_mcp_transport_session("tenant-2", "mcp-session-abc");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d, "same MCP session id must not collide across tenants");
    }

    #[test]
    fn mcp_logical_trace_id_is_stable_per_tenant_and_logical_session() {
        let ls = "550e8400-e29b-41d4-a716-446655440000";
        let a = trace_id_for_mcp_logical_session("tenant-1", ls);
        let b = trace_id_for_mcp_logical_session("tenant-1", ls);
        let c =
            trace_id_for_mcp_logical_session("tenant-1", "6ba7b810-9dad-11d1-80b4-00c04fd430c8");
        let d = trace_id_for_mcp_logical_session("tenant-2", ls);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(
            a, d,
            "same logical session id must not collide across tenants"
        );
    }

    #[tokio::test]
    async fn sse_snapshot_completed_trace_reports_terminal_seq() {
        let hub = TraceHub::default();
        let meta = TraceSessionMeta {
            tenant_id: "t1".into(),
            project_slug: "main".into(),
            mcp_config: None,
        };
        let tid = hub.ensure_session("sess-sse", meta).await;
        hub.finalize_mcp_session("sess-sse").await;
        let snap = hub
            .sse_snapshot_payload(tid, Some("t1"))
            .await
            .expect("snapshot json");
        let v: serde_json::Value = serde_json::from_str(&snap).expect("parse");
        assert_eq!(v.get("kind"), Some(&serde_json::json!("snapshot")));
        assert_eq!(v.get("seq"), Some(&serde_json::json!(1)));
    }

    #[tokio::test]
    async fn finalize_disconnected_sessions_closes_stale_keys() {
        let hub = TraceHub::default();
        let meta = TraceSessionMeta {
            tenant_id: "t1".into(),
            project_slug: "main".into(),
            mcp_config: None,
        };
        hub.ensure_session("sess-a", meta.clone()).await;
        let tid_b = hub.ensure_session("sess-b", meta).await;
        assert_eq!(tid_b, trace_id_for_mcp_transport_session("t1", "sess-b"));
        let mut live = std::collections::HashSet::new();
        live.insert("sess-a".into());
        let mut done = hub.finalize_disconnected_sessions(&live).await;
        done.sort();
        assert_eq!(done, vec!["sess-b".to_string()]);
        let detail = hub
            .get_detail(tid_b, Some("t1"))
            .await
            .expect("completed detail");
        assert_eq!(detail.summary.status, "completed");
    }

    #[tokio::test]
    async fn emit_after_finalize_resumes_completed_trace() {
        let hub = TraceHub::default();
        let meta = TraceSessionMeta {
            tenant_id: "t1".into(),
            project_slug: "main".into(),
            mcp_config: None,
        };
        let ls = "550e8400-e29b-41d4-a716-446655440001";
        let trace_id = hub
            .ensure_logical_session(ls, None, meta.clone())
            .await;
        hub.finalize_mcp_session(ls).await;
        hub.bump_and_emit(
            ls,
            TraceSegment::CodePlanExecute {
                plan_handle: "p1".into(),
                plan_id: "00000000-0000-0000-0000-000000000001".into(),
                plan_name: "demo".into(),
                plan_hash: "abc".into(),
                plan_uri: String::new(),
                canonical_plan_uri: String::new(),
                plan_http_path: String::new(),
                prompt_hash: "p".repeat(64),
                session_id: "s1".into(),
                node_count: 1,
                code_chars: 10,
                comp: None,
                dag: None,
                plasm_call_index: Some(1),
                run_ids: vec![],
                run_artifacts: vec![],
                plan_ux_reflection: None,
            },
        )
        .await;
        let detail = hub
            .get_detail(trace_id, Some("t1"))
            .await
            .expect("detail after resume emit");
        let kinds: Vec<_> = detail
            .records
            .iter()
            .filter_map(|r| r.get("kind").and_then(|v| v.as_str()))
            .collect();
        assert!(
            kinds.iter().any(|k| *k == "code_plan_execute"),
            "expected code_plan_execute segment, got {kinds:?}"
        );
    }
}
