//! In-memory trace session state (`active` / `completed` maps).

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use plasm_trace::{TraceEvent, TraceSegment};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::{McpSessionTrace, TraceSessionMeta};
use crate::trace_sink_emit::McpTraceAuditFields;

#[derive(Clone)]
pub(crate) struct ActiveTrace {
    pub trace_id: Uuid,
    /// Key for [`TraceHubInner::active`]: logical session UUID string (preferred) or legacy transport id.
    pub session_trace_key: String,
    pub logical_session_id: Option<String>,
    pub mcp_transport_session_id: Option<String>,
    pub meta: TraceSessionMeta,
    pub data: McpSessionTrace,
    pub started_ms: u64,
    pub last_activity_ms: u64,
    pub seq: u64,
}

#[derive(Clone)]
pub(crate) struct CompletedTrace {
    pub trace_id: Uuid,
    pub session_trace_key: String,
    pub logical_session_id: Option<String>,
    pub mcp_transport_session_id: Option<String>,
    pub meta: TraceSessionMeta,
    pub data: McpSessionTrace,
    pub started_ms: u64,
    pub ended_ms: u64,
    pub last_seq_emitted: u64,
}

pub(crate) struct TraceHubInner {
    pub active: HashMap<String, ActiveTrace>,
    pub completed: VecDeque<CompletedTrace>,
    pub tx_by_trace: HashMap<Uuid, broadcast::Sender<String>>,
}

pub(crate) struct TraceIngestJob {
    pub fields: McpTraceAuditFields,
    pub trace_event: TraceEvent,
    pub precomputed_payload: Option<serde_json::Value>,
    pub enqueued_at: Instant,
}

pub(crate) fn mcp_trace_audit_fields_from_active(a: &ActiveTrace) -> McpTraceAuditFields {
    McpTraceAuditFields {
        trace_id: a.trace_id,
        mcp_session_id: a.mcp_transport_session_id.clone(),
        logical_session_id: a.logical_session_id.clone(),
        plasm_prompt_hash: None,
        plasm_execute_session: None,
        run_id: None,
        tenant_id: (!a.meta.tenant_id.is_empty()).then(|| a.meta.tenant_id.clone()),
        principal_sub: None,
    }
}

impl TraceIngestJob {
    pub(crate) fn new_mcp_active_segment(
        a: &ActiveTrace,
        trace_event: TraceEvent,
        precomputed_payload: Option<serde_json::Value>,
    ) -> Self {
        Self {
            fields: mcp_trace_audit_fields_from_active(a),
            trace_event,
            precomputed_payload,
            enqueued_at: Instant::now(),
        }
    }
}

pub(crate) fn trace_segment_kind(segment: &TraceSegment) -> &'static str {
    match segment {
        TraceSegment::PlasmContext { .. } => "plasm_context",
        TraceSegment::ExpandDomain { .. } => "expand_domain",
        TraceSegment::PlasmInvocation { .. } => "plasm_invocation",
        TraceSegment::TeachingPromptCharsDelta { .. } => "teaching_prompt_chars_delta",
        TraceSegment::PlasmResponseCharsDelta { .. } => "plasm_response_chars_delta",
        TraceSegment::McpResourceRead { .. } => "mcp_resource_read",
        TraceSegment::PlasmLine { .. } => "plasm_line",
        TraceSegment::CodePlanEvaluate { .. } => "code_plan_evaluate",
        TraceSegment::CodePlanExecute { .. } => "code_plan_execute",
        TraceSegment::PlasmError { .. } => "plasm_error",
    }
}
