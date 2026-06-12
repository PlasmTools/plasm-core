//! Integration: same-step row progress coalescing through ExecuteSession fan-out.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_core::CgsContext;
use plasm_runtime::{CancelSignal, ExecutionConfig, ExecutionEngine, ExecutionMode};

use crate::execute_session::ExecuteSession;
use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
use crate::operation::{ExecutionScope, OpAcceptContext};
use crate::operation_progress::{OpProgressEvent, OP_PROGRESS_COALESCE};
use crate::run_artifacts::RunArtifactStore;
use crate::server_state::CatalogBootstrap;

fn host_state() -> Arc<crate::server_state::PlasmHostState> {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = Arc::new(load_schema_dir(&dir).expect("plasm_language_matrix"));
    let reg = InMemoryCgsRegistry::from_pairs(vec![(
        "langmatrix".into(),
        "Lang Matrix".into(),
        vec!["matrix".into()],
        cgs,
    )]);
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    Arc::new(build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(reg),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        plugin_manager: None,
        incoming_auth: None,
        run_artifacts: Arc::new(RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    }))
}

fn execute_session() -> Arc<ExecuteSession> {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = Arc::new(load_schema_dir(&dir).expect("plasm_language_matrix"));
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        "langmatrix".into(),
        Arc::new(CgsContext::entry("langmatrix", cgs.clone())),
    );
    Arc::new(ExecuteSession::new(
        "ph_coalesce".into(),
        "LangItem".into(),
        cgs,
        ctxs,
        "langmatrix".into(),
        String::new(),
        String::new(),
        None,
        vec!["LangItem".into()],
        None,
        None,
        None,
        "hash".into(),
        None,
        None,
    ))
}

fn is_running_line(line: &str) -> bool {
    line.contains('~')
}

async fn collect_progress_lines(
    rx: &mut tokio::sync::broadcast::Receiver<OpProgressEvent>,
    until: Instant,
) -> Vec<(Instant, String)> {
    let mut out = Vec::new();
    while Instant::now() < until {
        match tokio::time::timeout(Duration::from_millis(40), rx.recv()).await {
            Ok(Ok(ev)) => out.push((Instant::now(), ev.line)),
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
            Err(_) => continue,
        }
    }
    out
}

#[tokio::test]
async fn coalesce_row_spam_via_execution_scope_broadcast() {
    let es = execute_session();
    let st = host_state();
    let handle = es.mint_operation_handle("s0");
    let cancel = CancelSignal::new();

    let accept = OpAcceptContext {
        host: Some(Arc::downgrade(&st)),
        mcp_transport_key: Some("mcp-coalesce-test".into()),
        ..Default::default()
    };
    es.try_begin_async_operation(handle.clone(), cancel.clone(), accept)
        .expect("register op");
    es.emit_op_accept(&handle, st.as_ref())
        .expect("accept emit");

    let mut rx = es
        .operation_progress_subscribe(&handle)
        .expect("progress subscribe");
    let scope = ExecutionScope::for_async_operation(Arc::clone(&es), handle.clone(), cancel);

    scope.set_progress(1, 1, Some("n0".into()));
    for _ in 0..50 {
        scope.add_rows_materialized(1);
    }

    let burst_deadline = Instant::now() + Duration::from_millis(200);
    let burst = collect_progress_lines(&mut rx, burst_deadline).await;
    let running_burst: Vec<_> = burst
        .iter()
        .filter(|(_, line)| is_running_line(line))
        .collect();
    assert!(
        running_burst.len() <= 1,
        "same-step row spam should coalesce to at most one ~ emit before 2s window, got {}: {:?}",
        running_burst.len(),
        running_burst
            .iter()
            .map(|(_, l)| l.as_str())
            .collect::<Vec<_>>()
    );

    st.op_progress_hub.drain_mcp_pending();

    tokio::time::sleep(OP_PROGRESS_COALESCE + Duration::from_millis(80)).await;
    scope.add_rows_materialized(1);

    let after_coalesce_deadline = Instant::now() + Duration::from_secs(1);
    let after = collect_progress_lines(&mut rx, after_coalesce_deadline).await;
    let running_after: Vec<_> = after
        .iter()
        .filter(|(_, line)| is_running_line(line))
        .collect();
    assert_eq!(
        running_after.len(),
        1,
        "expected one ~ emit after coalesce window, got {:?}",
        running_after
            .iter()
            .map(|(_, l)| l.as_str())
            .collect::<Vec<_>>()
    );

    let mcp = st.op_progress_hub.drain_mcp_pending();
    assert_eq!(
        mcp.len(),
        1,
        "MCP hub should receive exactly one post-coalesce notify"
    );
    assert!(mcp[0].line.contains('~'));
}

#[tokio::test]
async fn coalesce_at_most_two_running_emits_per_two_second_window() {
    let es = execute_session();
    let st = host_state();
    let handle = es.mint_operation_handle("s0");
    let cancel = CancelSignal::new();

    let accept = OpAcceptContext {
        host: Some(Arc::downgrade(&st)),
        ..Default::default()
    };
    es.try_begin_async_operation(handle.clone(), cancel.clone(), accept)
        .expect("register op");
    es.emit_op_accept(&handle, st.as_ref())
        .expect("accept emit");

    let mut rx = es
        .operation_progress_subscribe(&handle)
        .expect("progress subscribe");
    let scope = ExecutionScope::for_async_operation(Arc::clone(&es), handle.clone(), cancel);

    scope.set_progress(1, 1, None);

    let mut running_times: Vec<Instant> = Vec::new();
    let window_end = Instant::now() + OP_PROGRESS_COALESCE * 2 + Duration::from_millis(200);

    while Instant::now() < window_end {
        for _ in 0..10 {
            scope.add_rows_materialized(1);
        }
        if let Ok(ev) = rx.try_recv() {
            if is_running_line(&ev.line) {
                running_times.push(Instant::now());
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    assert!(
        running_times.len() >= 2,
        "expected at least step + post-coalesce running emits, got {}",
        running_times.len()
    );

    let window = OP_PROGRESS_COALESCE;
    for (i, &t0) in running_times.iter().enumerate() {
        let count_in_window = running_times
            .iter()
            .skip(i)
            .take_while(|&&t| t <= t0 + window)
            .count();
        assert!(
            count_in_window <= 2,
            "sliding 2s window should contain at most 2 ~ emits, saw {count_in_window} at index {i}: {:?}",
            running_times
        );
    }

    es.finalize_operation_succeeded(
        &handle,
        crate::plasm_plan_run::PlasmPlanRunResult {
            version: serde_json::json!({}),
            node_results: Vec::new(),
            graph_summary: serde_json::json!({}),
            comp: serde_json::json!({}),
            code_plan_run_artifacts: Vec::new(),
            run_markdown: None,
            run_plasm_meta: None,
            return_steps: Vec::new(),
        },
        Some(st.as_ref()),
    );
}
