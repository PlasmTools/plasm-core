//! CPU-only benchmarks for trace detail assembly (no Iceberg I/O).
//!
//! Run: `cargo bench -p plasm-trace-sink --bench trace_detail_cpu`

use std::time::Duration;

use chrono::{TimeZone, Utc};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use plasm_trace_sink::iceberg_writer::durable_detail_from_events;
use plasm_trace_wire::{AuditEvent, AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT};
use serde_json::json;
use uuid::Uuid;

fn sample_line_payload(line_index: i64, call_index: i64) -> serde_json::Value {
    json!({
        "emitted_at_ms": line_index as u64 * 10,
        "kind": "plasm_line",
        "call_index": call_index,
        "line_index": line_index,
        "source_expression": "e1 | project name, id | limit 10",
        "repl_pre": "",
        "repl_post": "",
        "capability": "github.list_issues",
        "operation": "query",
        "api_entry_id": "github",
        "duration_ms": 42,
        "stats": {
            "duration_ms": 42,
            "network_requests": 1,
            "cache_hits": 0,
            "cache_misses": 1
        },
        "source": "live",
        "request_fingerprints": ["abc123def456"],
        "http_calls": [{
            "method": "GET",
            "url": "https://api.github.com/repos/org/repo/issues",
            "status": 200,
            "duration_ms": 38,
            "request_bytes": 256,
            "response_bytes": 8192
        }]
    })
}

fn synthetic_trace_events(n_lines: usize, tenant: &str) -> Vec<AuditEvent> {
    let trace_id = Uuid::new_v4();
    let base = Utc.with_ymd_and_hms(2026, 4, 7, 10, 0, 0).unwrap();
    let mut events = Vec::with_capacity(n_lines + 2);

    events.push(AuditEvent {
        event_id: Uuid::new_v4(),
        schema_version: 1,
        emitted_at: base,
        ingested_at: base,
        trace_id,
        mcp_session_id: Some("mcp-session-fixture".into()),
        logical_session_id: None,
        plasm_prompt_hash: None,
        plasm_execute_session: None,
        run_id: None,
        call_index: Some(0),
        line_index: None,
        tenant_id: Some(tenant.into()),
        principal_sub: None,
        workspace_slug: None,
        project_slug: Some("main".into()),
        event_kind: AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT.to_string(),
        request_units: 1,
        payload: json!({
            "emitted_at_ms": 0u64,
            "kind": "plasm_invocation",
            "call_index": 0,
            "expression_count": n_lines,
            "batch": false
        }),
    });

    for i in 0..n_lines {
        let t = base + Duration::from_millis(i as u64 * 10 + 1);
        events.push(AuditEvent {
            event_id: Uuid::new_v4(),
            schema_version: 1,
            emitted_at: t,
            ingested_at: t,
            trace_id,
            mcp_session_id: Some("mcp-session-fixture".into()),
            logical_session_id: None,
            plasm_prompt_hash: None,
            plasm_execute_session: None,
            run_id: None,
            call_index: Some(0),
            line_index: Some(i as i64),
            tenant_id: Some(tenant.into()),
            principal_sub: None,
            workspace_slug: None,
            project_slug: Some("main".into()),
            event_kind: AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT.to_string(),
            request_units: 1,
            payload: sample_line_payload(i as i64, 0),
        });
    }

    events
}

fn bench_durable_detail_from_events(c: &mut Criterion) {
    let mut group = c.benchmark_group("durable_detail_from_events");
    group.sample_size(30);

    for n in [50_usize, 200, 500, 1_000] {
        let events = synthetic_trace_events(n, "tenant-bench");
        let trace_id = events[0].trace_id;
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &events, |b, events| {
            b.iter(|| {
                black_box(durable_detail_from_events(
                    trace_id,
                    events.clone(),
                    "tenant-bench".to_string(),
                ))
            });
        });
    }
    group.finish();
}

fn bench_durable_detail_json_serialize(c: &mut Criterion) {
    let events = synthetic_trace_events(500, "tenant-bench");
    let trace_id = events[0].trace_id;
    let detail = durable_detail_from_events(trace_id, events, "tenant-bench".to_string());
    let response = plasm_trace_wire::TraceDetailResponse { trace_id, detail };

    c.bench_function("trace_detail_response_json_500_records", |b| {
        b.iter(|| black_box(serde_json::to_vec(&response).unwrap()));
    });
}

criterion_group!(
    benches,
    bench_durable_detail_from_events,
    bench_durable_detail_json_serialize
);
criterion_main!(benches);
