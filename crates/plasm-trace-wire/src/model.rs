//! Canonical ingest envelope (v1) and HTTP list/detail DTOs shared with trace sink and agent.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SCHEMA_VERSION: i32 = 1;

/// Ingest `AuditEvent.event_kind` for canonical MCP / execute trace segments (`payload` = [`plasm_trace::TraceEvent`] JSON).
pub const AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT: &str = "mcp_trace_segment";

fn now_utc() -> DateTime<Utc> {
    Utc::now()
}

/// Single client-emitted event (immutable audit log row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub event_id: Uuid,
    pub schema_version: i32,
    pub emitted_at: DateTime<Utc>,
    #[serde(skip_deserializing, default = "now_utc")]
    pub ingested_at: DateTime<Utc>,
    /// Correlates all spans from one logical invocation (MCP `plasm` call or a multi-line HTTP run).
    pub trace_id: Uuid,
    pub mcp_session_id: Option<String>,
    /// MCP logical session UUID (agent window). First-class lake column — not nested in `payload`.
    #[serde(default)]
    pub logical_session_id: Option<String>,
    pub plasm_prompt_hash: Option<String>,
    pub plasm_execute_session: Option<String>,
    pub run_id: Option<String>,
    pub call_index: Option<i64>,
    pub line_index: Option<i64>,
    pub tenant_id: Option<String>,
    pub principal_sub: Option<String>,
    /// Denormalized for Iceberg pruning; may also appear under `payload`.
    #[serde(default)]
    pub workspace_slug: Option<String>,
    /// Denormalized for Iceberg pruning; may also appear under `payload`.
    #[serde(default)]
    pub project_slug: Option<String>,
    pub event_kind: String,
    pub request_units: i64,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IngestBatchRequest {
    pub events: Vec<AuditEvent>,
}

#[derive(Debug, Serialize)]
pub struct IngestBatchResponse {
    pub accepted: usize,
    pub duplicate_skipped: usize,
}

#[derive(Debug, Serialize)]
pub struct TraceGetResponse {
    pub trace_id: Uuid,
    pub events: Vec<AuditEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TraceTotals {
    pub plasm_tool_calls: u64,
    pub plasm_expressions: u64,
    pub expression_lines: u64,
    pub multi_line_plasm_invocations: u64,
    pub teaching_prompt_chars: u64,
    pub plasm_invocation_chars: u64,
    pub plasm_response_chars: u64,
    #[serde(default)]
    pub mcp_resource_read_chars: u64,
    #[serde(default)]
    pub mcp_resource_read_ui_chars: u64,
    pub total_duration_ms: u64,
    pub network_requests: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub http_trace_entry_count: u64,
    #[serde(default)]
    pub code_plans_evaluated: u64,
    #[serde(default)]
    pub code_plans_executed: u64,
    #[serde(default)]
    pub code_plan_code_chars: u64,
    #[serde(default)]
    pub code_plan_nodes: u64,
    #[serde(default)]
    pub code_plan_derived_runs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSummary {
    pub trace_id: Uuid,
    pub mcp_session_id: String,
    pub status: String,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub project_slug: String,
    pub tenant_id: String,
    pub totals: TraceTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceDetailRecord {
    pub kind: String,
    pub record: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableTraceDetail {
    #[serde(flatten)]
    pub summary: TraceSummary,
    #[serde(default)]
    pub records: Vec<TraceDetailRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceListResponse {
    pub traces: Vec<TraceSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceDetailResponse {
    pub trace_id: Uuid,
    pub detail: DurableTraceDetail,
}

impl AuditEvent {
    pub fn tenant_partition(&self) -> String {
        self.tenant_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "__none__".to_string())
    }

    /// Workspace slug for lake columns and trace heads (`payload` fallback for older clients).
    pub fn audit_workspace_slug(&self) -> String {
        self.workspace_slug
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.payload
                    .get("workspace_slug")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default()
    }

    /// Project slug for lake columns and trace heads (`payload` fallback).
    pub fn audit_project_slug(&self) -> String {
        self.project_slug
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.payload
                    .get("project_slug")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default()
    }
}
