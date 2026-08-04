//! Round-trip: durable `mcp_trace_segment` payloads → detail projection with lake columns.

use std::sync::Arc;

use chrono::Utc;
use plasm_trace::{minimal_trace_comp_json, TraceCompWire, TraceEvent, TraceSegment};
use uuid::Uuid;

use crate::iceberg_writer::{durable_detail_from_events, trace_detail_record_from_audit_event};
use crate::model::{AuditEvent, AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT, SCHEMA_VERSION};

fn sample_code_plan_audit(with_ux: bool) -> AuditEvent {
    let comp =
        Arc::new(TraceCompWire::from_json_value(minimal_trace_comp_json()).expect("minimal comp"));
    let ux = with_ux.then(|| {
        serde_json::json!({
            "schema_version": 3,
            "flow": {
                "schema_version": 1,
                "verdict": "clean",
                "counts": { "allow": 0, "approve": 0, "review": 0, "deny": 0 },
                "violations": [],
                "trace": []
            }
        })
    });
    let ev = TraceEvent::at(
        99,
        TraceSegment::CodePlanEvaluate {
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
            comp,
            plan_ux_reflection: ux,
        },
    );
    let payload = serde_json::to_value(&ev).expect("ser");
    AuditEvent {
        event_id: Uuid::new_v4(),
        schema_version: SCHEMA_VERSION,
        emitted_at: Utc::now(),
        ingested_at: Utc::now(),
        trace_id: Uuid::new_v4(),
        mcp_session_id: Some("mcp".into()),
        logical_session_id: Some("ls-1".into()),
        plasm_prompt_hash: None,
        plasm_execute_session: None,
        run_id: None,
        call_index: None,
        line_index: None,
        tenant_id: Some("t1".into()),
        principal_sub: None,
        workspace_slug: None,
        project_slug: Some("main".into()),
        event_kind: AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT.to_string(),
        request_units: 0,
        payload,
    }
}

#[test]
fn code_plan_evaluate_projects_logical_session_from_audit_column() {
    let e = sample_code_plan_audit(true);
    let rec = trace_detail_record_from_audit_event(&e).expect("project");
    assert_eq!(
        rec.record.get("kind").and_then(|k| k.as_str()),
        Some("code_plan_evaluate")
    );
    assert_eq!(
        rec.record
            .get("logical_session_id")
            .and_then(|v| v.as_str()),
        Some("ls-1")
    );
    assert!(rec.record.get("_plasm_audit").is_none());
    assert!(rec.record.get("comp").is_some());
    assert_eq!(
        rec.record
            .get("plan_ux_reflection")
            .and_then(|u| u.get("flow"))
            .and_then(|f| f.get("verdict"))
            .and_then(|v| v.as_str()),
        Some("clean")
    );
}

#[test]
fn durable_detail_keeps_code_plan_alongside_resource_read() {
    let plan = sample_code_plan_audit(true);
    let read_payload = serde_json::json!({
        "emitted_at_ms": 100,
        "kind": "mcp_resource_read",
        "uri_display": "plasm://execute/p/s/run/r",
        "chars_added": 12,
        "duration_ms": 1,
        "result": "success"
    });
    let read = AuditEvent {
        event_id: Uuid::new_v4(),
        schema_version: SCHEMA_VERSION,
        emitted_at: Utc::now(),
        ingested_at: Utc::now(),
        trace_id: plan.trace_id,
        mcp_session_id: Some("mcp".into()),
        logical_session_id: Some("ls-1".into()),
        plasm_prompt_hash: None,
        plasm_execute_session: None,
        run_id: None,
        call_index: None,
        line_index: None,
        tenant_id: Some("t1".into()),
        principal_sub: None,
        workspace_slug: None,
        project_slug: Some("main".into()),
        event_kind: AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT.to_string(),
        request_units: 0,
        payload: read_payload,
    };
    let detail = durable_detail_from_events(plan.trace_id, vec![plan, read], "t1".into());
    let kinds: Vec<&str> = detail
        .records
        .iter()
        .filter_map(|r| r.record.get("kind").and_then(|k| k.as_str()))
        .collect();
    assert!(
        kinds.contains(&"code_plan_evaluate"),
        "expected code_plan_evaluate in {kinds:?}"
    );
    assert!(
        kinds.contains(&"mcp_resource_read"),
        "expected mcp_resource_read in {kinds:?}"
    );
    assert!(detail.summary.totals.code_plans_evaluated >= 1);
}

#[test]
fn legacy_payload_plasm_audit_nest_still_decodes() {
    let mut e = sample_code_plan_audit(true);
    e.logical_session_id = None;
    if let serde_json::Value::Object(ref mut map) = e.payload {
        map.insert(
            "_plasm_audit".into(),
            serde_json::json!({ "logical_session_id": "legacy-ls" }),
        );
    }
    let rec = trace_detail_record_from_audit_event(&e).expect("project strips nest");
    assert_eq!(
        rec.record.get("kind").and_then(|k| k.as_str()),
        Some("code_plan_evaluate")
    );
    // Column empty → detail omits logical_session_id (no re-attach from nest).
    assert!(rec.record.get("logical_session_id").is_none());
    assert!(rec.record.get("_plasm_audit").is_none());
}
