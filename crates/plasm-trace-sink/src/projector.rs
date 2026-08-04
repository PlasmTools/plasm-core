//! Project audit events into denormalized `trace_spans` rows (billing + retrieval).

use plasm_trace::TraceSegment;
use uuid::Uuid;

use crate::model::{AuditEvent, TraceSpanRow, AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT};
use crate::trace_event_decode::decode_mcp_trace_segment;

/// `mcp_trace_segment` rows deserialize to [`TraceEvent`]; only `plasm_line` spans bill per line.
pub fn project_trace_spans(ev: &AuditEvent) -> Vec<TraceSpanRow> {
    if ev.event_kind != AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT {
        return Vec::new();
    }
    let Some(te) = decode_mcp_trace_segment(ev, None) else {
        return Vec::new();
    };
    let TraceSegment::PlasmLine {
        stats,
        capability,
        api_entry_id,
        ..
    } = &te.segment
    else {
        return Vec::new();
    };
    let span_id = Uuid::new_v4();
    vec![TraceSpanRow {
        span_id,
        event_id: ev.event_id,
        trace_id: ev.trace_id,
        emitted_at: ev.emitted_at,
        tenant_partition: ev.tenant_partition(),
        mcp_session_id: ev.mcp_session_id.clone(),
        plasm_prompt_hash: ev.plasm_prompt_hash.clone(),
        plasm_execute_session: ev.plasm_execute_session.clone(),
        run_id: ev.run_id.clone(),
        call_index: ev.call_index,
        line_index: ev.line_index,
        span_name: "plasm_line".to_string(),
        is_billing_event: true,
        billing_event_type: Some("plasm_line".to_string()),
        request_units: ev.request_units,
        duration_ms: Some(stats.duration_ms as i64),
        api_entry_id: api_entry_id.clone(),
        capability: capability.clone(),
        attributes_json: ev.payload.clone(),
    }]
}
