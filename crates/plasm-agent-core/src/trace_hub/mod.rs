//! Project-scoped MCP session traces for demo/debug UX: in-memory capture, summaries, and SSE fan-out.
//!
//! Traces for MCP tools are keyed by **agent logical session id** (see [`TraceHub::ensure_logical_session`]):
//! a **name-based UUID (v5)** over **`(tenant_id, logical_session_id)`**. MCP transport
//! `MCP-Session-Id` is stored only as a **correlation** field on summaries and durable audit payloads.
//! Legacy [`TraceHub::ensure_session`] still keys by transport id for tests. Direct HTTP execute uses a
//! separate v5 root over **`(tenant_id, prompt_hash, execute_session_id)`**.

#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::env;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub use plasm_trace::{
    totals_from_session_data, CodePlanRunArtifactRef, PlasmLineTraceMeta, RunArtifactArchiveRef,
    SessionTraceData, TraceEvent, TraceSegment, TraceTotals,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::trace_sink_emit::TraceIngestClient;

mod detail;
mod emit;
mod ingest;
mod record;
mod resume;
mod session;
mod sse;
mod state;

use state::{TraceHubInner, TraceIngestJob};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceHubBounds {
    pub max_completed_traces: usize,
    pub sse_broadcast_capacity: usize,
    pub ingest_queue_capacity: usize,
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

pub const TRACE_HUB_ENV_MAX_COMPLETED: &str = "PLASM_TRACE_HUB_MAX_COMPLETED";
pub const TRACE_HUB_ENV_SSE_BROADCAST_CAP: &str = "PLASM_TRACE_HUB_SSE_BROADCAST_CAP";
pub const TRACE_HUB_ENV_INGEST_QUEUE_CAP: &str = "PLASM_TRACE_HUB_INGEST_QUEUE_CAP";
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TraceHubConfig {
    pub bounds: TraceHubBounds,
}

impl TraceHubConfig {
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

    pub fn build(
        self,
        trace_ingest: Option<Arc<dyn TraceIngestClient>>,
        local_trace_archive: Option<Arc<crate::local_trace_archive::LocalTraceArchive>>,
    ) -> TraceHub {
        TraceHub::from_parts(trace_ingest, self.config, local_trace_archive)
    }
}

const MAX_TRACE_REASONING_CHARS: usize = 8192;

pub(super) fn truncate_trace_reasoning(s: &str) -> String {
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

const TRACE_ID_NS_MCP_TRANSPORT_V2: Uuid =
    Uuid::from_u128(0x018f_b8d5_4e9a_73a7_b0e1_3f2c_1a8b_09d2);
const TRACE_ID_NS_MCP_LOGICAL_V1: Uuid = Uuid::from_u128(0x018f_b8d5_4e9a_73a7_b0e1_3f2c_1a8b_09d4);
const TRACE_ID_NS_HTTP_EXECUTE_V2: Uuid =
    Uuid::from_u128(0x018f_b8d5_4e9a_73a7_b0e1_3f2c_1a8b_09d3);

fn trace_tenant_segment(tenant_id: &str) -> &str {
    if tenant_id.is_empty() {
        "anonymous"
    } else {
        tenant_id
    }
}

pub fn trace_id_for_mcp_transport_session(tenant_id: &str, mcp_session_id: &str) -> Uuid {
    let t = trace_tenant_segment(tenant_id);
    let name = format!("{t}\n{mcp_session_id}");
    Uuid::new_v5(&TRACE_ID_NS_MCP_TRANSPORT_V2, name.as_bytes())
}

pub fn trace_id_for_mcp_logical_session(tenant_id: &str, logical_session_id: &str) -> Uuid {
    let t = trace_tenant_segment(tenant_id);
    let name = format!("{t}\nlogical:{logical_session_id}");
    Uuid::new_v5(&TRACE_ID_NS_MCP_LOGICAL_V1, name.as_bytes())
}

pub fn trace_id_for_http_execute_session(
    tenant_id: &str,
    prompt_hash: &str,
    execute_session_id: &str,
) -> Uuid {
    let t = trace_tenant_segment(tenant_id);
    let name = format!("{t}\n{prompt_hash}\n{execute_session_id}");
    Uuid::new_v5(&TRACE_ID_NS_HTTP_EXECUTE_V2, name.as_bytes())
}

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

pub struct TraceHub {
    pub(super) inner: RwLock<TraceHubInner>,
    pub(super) ingest_tx: Option<mpsc::Sender<TraceIngestJob>>,
    pub(super) config: TraceHubConfig,
    pub(super) ingest_channel_backlog: Arc<AtomicUsize>,
    pub(super) local_trace_archive: Option<Arc<crate::local_trace_archive::LocalTraceArchive>>,
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
        let ingest_tx = trace_ingest.map(|ingest| {
            ingest::start_ingest_channel(
                ingest,
                config.bounds.ingest_queue_capacity,
                Arc::clone(&ingest_channel_backlog),
            )
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

    pub fn bounds(&self) -> TraceHubBounds {
        self.config.bounds
    }

    pub fn config(&self) -> TraceHubConfig {
        self.config
    }

    pub async fn active_mcp_session_count(&self) -> usize {
        let g = self.inner.read().await;
        g.active.len()
    }
}

#[derive(Clone)]
pub struct McpPlasmTraceSink {
    pub hub: Arc<TraceHub>,
    pub mcp_key: String,
    pub call_index: u64,
}

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
