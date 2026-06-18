use plasm_runtime::{ExecutionSource, ExecutionStats};
use plasm_trace::TraceSegment;

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
    let c = trace_id_for_mcp_logical_session("tenant-1", "6ba7b810-9dad-11d1-80b4-00c04fd430c8");
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
    let trace_id = hub.ensure_logical_session(ls, None, meta.clone()).await;
    hub.finalize_mcp_session(ls).await;
    hub.trace_record_code_plan_execute(
        ls,
        CodePlanTrace {
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
            comp: serde_json::json!({}),
            dag: serde_json::json!({}),
            plan_ux_reflection: None,
            plasm_call_index: Some(1),
            run_ids: vec![],
            run_artifacts: vec![],
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
        kinds.contains(&"code_plan_execute"),
        "expected code_plan_execute segment, got {kinds:?}"
    );
}
