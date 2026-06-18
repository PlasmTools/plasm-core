//! Trace head row → [`TraceTotals`] for list views (shared by Iceberg decoders and SQL projection).

use plasm_trace::{
    session_data_from_ordered_events, totals_from_session_data, SessionTraceCountersSnapshot,
    SessionTraceData, TraceEvent,
};

use crate::model::{TraceHeadRow, TraceTotals};

/// True when list/detail should recompute KPIs from durable segment rows instead of head snapshot.
pub(crate) fn head_needs_segment_recompute(h: &TraceHeadRow) -> bool {
    head_totals_snapshot_stale(h)
}

fn head_totals_snapshot_stale(h: &TraceHeadRow) -> bool {
    let tj = h.totals_json.trim();
    if tj.is_empty() {
        return true;
    }
    let Ok(snap) = serde_json::from_str::<SessionTraceCountersSnapshot>(tj) else {
        return false;
    };
    snap.aggregate_expression_lines == 0
        && snap.aggregate_network_requests == 0
        && snap.code_plans_evaluated == 0
        && snap.code_plans_executed == 0
        && snap.plasm_call_count == 0
}

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

/// Prefer head snapshot when populated; otherwise recompute from segment rows.
pub(crate) fn trace_totals_from_head_or_records(
    h: &TraceHeadRow,
    records: &[serde_json::Value],
) -> TraceTotals {
    if !records.is_empty() && head_needs_segment_recompute(h) {
        return trace_totals_from_segment_records(
            records,
            h.mcp_session_id.as_deref().unwrap_or(""),
        );
    }
    trace_totals_from_head_row(h)
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

    #[test]
    fn stale_head_with_resource_read_chars_recomputes_from_segments() {
        let mut head = test_head();
        head.totals_json = serde_json::to_string(&plasm_trace::SessionTraceCountersSnapshot {
            mcp_resource_read_chars: 1722,
            ..Default::default()
        })
        .unwrap();
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
        assert!(
            head_needs_segment_recompute(&head),
            "resource-only snapshot should be stale for segment recompute"
        );
        let totals = trace_totals_from_head_or_records(&head, &records);
        assert_eq!(totals.network_requests, 3);
    }

    #[test]
    fn all_zero_head_snapshot_recomputes_from_segments() {
        let mut head = test_head();
        head.totals_json = serde_json::to_string(&plasm_trace::SessionTraceCountersSnapshot {
            ..Default::default()
        })
        .unwrap();
        let records = vec![serde_json::json!({
            "emitted_at_ms": 100,
            "kind": "mcp_resource_read",
            "uri_display": "plasm://run/1",
            "chars_added": 100,
            "duration_ms": 42,
            "result": "success"
        })];
        let totals = trace_totals_from_head_or_records(&head, &records);
        assert_eq!(totals.mcp_resource_read_chars, 100);
        assert_eq!(totals.total_duration_ms, 42);
    }

    #[test]
    fn head_snapshot_wins_when_totals_json_present() {
        let mut head = test_head();
        head.totals_json = serde_json::to_string(&plasm_trace::SessionTraceCountersSnapshot {
            aggregate_network_requests: 99,
            aggregate_expression_lines: 1,
            ..Default::default()
        })
        .unwrap();
        let records = vec![serde_json::json!({
            "emitted_at_ms": 100,
            "kind": "plasm_line",
            "call_index": 0,
            "line_index": 0,
            "source_expression": "e1",
            "duration_ms": 12,
            "stats": { "network_requests": 3, "duration_ms": 12 },
            "source": "live",
            "request_fingerprints": [],
            "http_calls": []
        })];
        let totals = trace_totals_from_head_or_records(&head, &records);
        assert_eq!(totals.network_requests, 99);
    }

    #[test]
    fn list_and_detail_use_same_totals_helper() {
        let head = test_head();
        let records = vec![serde_json::json!({
            "emitted_at_ms": 100,
            "kind": "plasm_line",
            "call_index": 0,
            "line_index": 0,
            "source_expression": "e1",
            "duration_ms": 5,
            "stats": { "network_requests": 2, "duration_ms": 5 },
            "source": "live",
            "request_fingerprints": [],
            "http_calls": []
        })];
        let list_totals = trace_totals_from_head_or_records(&head, &records);
        let detail_totals = trace_totals_from_head_or_records(&head, &records);
        assert_eq!(list_totals.network_requests, detail_totals.network_requests);
        assert_eq!(
            list_totals.total_duration_ms,
            detail_totals.total_duration_ms
        );
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
