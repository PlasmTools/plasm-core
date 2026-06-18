//! Trace head row → [`TraceTotals`] for list views (shared by Iceberg decoders and SQL projection).

use plasm_trace::{session_data_from_ordered_events, totals_from_session_data, TraceEvent, SessionTraceData};

use crate::model::{TraceHeadRow, TraceTotals};

/// Derive list totals from a durable head row (`totals_json` snapshot or line-based fallback).
pub(crate) fn trace_totals_from_head_row(h: &TraceHeadRow) -> TraceTotals {
    let tj = h.totals_json.trim();
    if !tj.is_empty() {
        if let Ok(snap) = serde_json::from_str::<plasm_trace::SessionTraceCountersSnapshot>(tj) {
            let mcp = h.mcp_session_id.clone().unwrap_or_default();
            let data = snap.into_session_data(mcp);
            return totals_from_session_data(&data).into();
        }
    }
    TraceTotals {
        plasm_tool_calls: h.max_call_index.map(|c| (c.max(0) as u64) + 1).unwrap_or(0),
        plasm_expressions: 0,
        expression_lines: h.expression_lines.max(0) as u64,
        ..TraceTotals::default()
    }
}

/// Recompute totals from flattened trace segment JSON (detail projection / legacy heads).
pub(crate) fn trace_totals_from_segment_records(
    records: &[serde_json::Value],
    mcp_session_id: &str,
) -> TraceTotals {
    let events: Vec<TraceEvent> = records
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();
    if events.is_empty() {
        return TraceTotals::default();
    }
    let session: SessionTraceData = session_data_from_ordered_events(mcp_session_id, events);
    totals_from_session_data(&session).into()
}

/// Prefer head snapshot; when `totals_json` is empty but segment rows exist, recompute from records.
pub(crate) fn trace_totals_from_head_or_records(
    h: &TraceHeadRow,
    records: &[serde_json::Value],
) -> TraceTotals {
    let from_head = trace_totals_from_head_row(h);
    if h.totals_json.trim().is_empty() && !records.is_empty() {
        let from_records = trace_totals_from_segment_records(
            records,
            h.mcp_session_id.as_deref().unwrap_or(""),
        );
        if totals_richer_than(&from_records, &from_head) {
            return from_records;
        }
    }
    from_head
}

fn totals_richer_than(a: &TraceTotals, b: &TraceTotals) -> bool {
    a.network_requests > b.network_requests
        || a.total_duration_ms > b.total_duration_ms
        || a.code_plans_evaluated > b.code_plans_evaluated
        || a.code_plans_executed > b.code_plans_executed
        || a.teaching_prompt_chars > b.teaching_prompt_chars
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn segment_recompute_fills_network_when_head_json_empty() {
        let head = test_head();
        let records = vec![serde_json::json!({
            "emitted_at_ms": 100,
            "kind": "plasm_line",
            "call_index": 0,
            "line_index": 0,
            "source_expression": "e1",
            "duration_ms": 12,
            "stats": {
                "network_requests": 3,
                "cache_hits": 1,
                "cache_misses": 2,
                "duration_ms": 12
            },
            "source": "live",
            "request_fingerprints": [],
            "http_calls": []
        })];
        let totals = trace_totals_from_head_or_records(&head, &records);
        assert_eq!(totals.network_requests, 3);
        assert_eq!(totals.total_duration_ms, 12);
    }

    fn test_head() -> TraceHeadRow {
        TraceHeadRow {
            trace_id: Uuid::new_v4(),
            tenant_partition: "t".into(),
            tenant_id: "t".into(),
            project_slug: "main".into(),
            mcp_session_id: Some("s1".into()),
            status: "completed".into(),
            started_at_ms: 0,
            ended_at_ms: None,
            updated_at_ms: 0,
            expression_lines: 1,
            max_call_index: Some(0),
            totals_json: String::new(),
            workspace_slug: String::new(),
        }
    }
}
