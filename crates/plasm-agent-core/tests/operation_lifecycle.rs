//! CEP-7/8: operation terminal resolution and MCP await semantics.

mod common;

use std::time::Duration;

use common::{begin_plain_operation, empty_session, minimal_host};
use plasm_agent_core::execute_pipeline::GRAPH_WRITE_CONFLICT_USER_MESSAGE;
use plasm_agent_core::mcp_run_await::{
    await_operation_terminal, AwaitConfig, AwaitError, TerminalAwaitContext,
};
use plasm_agent_core::operation_error::OperationError;
use plasm_agent_core::plasm_plan_run::PlasmPlanRunResult;
use plasm_agent_core::terminal_plan_run::resolve_terminal_plan_run;
use plasm_core::OperationHandle;
use uuid::Uuid;

#[tokio::test]
async fn cep_7_failed_operation_surfaces_verbatim_error() {
    let es = empty_session();
    let handle = begin_plain_operation(es.as_ref());
    es.finalize_operation_failed(&handle, GRAPH_WRITE_CONFLICT_USER_MESSAGE.to_string(), None);
    let err = resolve_terminal_plan_run(es.as_ref(), None, None, &handle)
        .await
        .expect_err("terminal failed");
    assert!(matches!(err, OperationError::OperationFailed { .. }));
    assert_eq!(
        err.detail(),
        format!("operation `{handle}` failed: {GRAPH_WRITE_CONFLICT_USER_MESSAGE}")
    );
    assert_eq!(err.code(), OperationError::CODE_OPERATION_FAILED);
}

#[tokio::test]
async fn cep_7_unknown_handle_only_when_absent() {
    let es = empty_session();
    let handle = OperationHandle::parse("o99").expect("handle");
    let err = resolve_terminal_plan_run(es.as_ref(), None, None, &handle)
        .await
        .expect_err("missing handle");
    assert!(matches!(err, OperationError::UnknownHandle { .. }));
    assert_eq!(err.code(), OperationError::CODE_UNKNOWN);
}

#[tokio::test]
async fn cep_8_await_propagates_terminal_failure() {
    let es = empty_session();
    let handle = begin_plain_operation(es.as_ref());
    let fail_msg = "plan relation materialize failed: test";
    let es_bg = es.clone();
    let handle_bg = handle.clone();
    let fail_msg_bg = fail_msg.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        es_bg.finalize_operation_failed(&handle_bg, fail_msg_bg, None);
    });
    let err = await_operation_terminal(TerminalAwaitContext {
        es,
        st: minimal_host(),
        handle,
        trace: plasm_agent_core::trace_sink_emit::PlasmTraceContext {
            trace_id: Uuid::nil(),
            call_index: None,
            mcp_session_id: None,
            logical_session_id: None,
            logical_session_ref: None,
        },
        cfg: AwaitConfig {
            poll_interval: Duration::from_millis(20),
            max_wait: Duration::from_secs(2),
        },
    })
    .await
    .expect_err("failed op");
    assert!(matches!(err, AwaitError::Operation(ref m) if m.contains(fail_msg)));
}

#[tokio::test]
async fn cep_8_await_returns_success_when_finalized() {
    let es = empty_session();
    let handle = begin_plain_operation(es.as_ref());
    let es_bg = es.clone();
    let handle_bg = handle.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        es_bg.finalize_operation_succeeded(
            &handle_bg,
            PlasmPlanRunResult {
                version: serde_json::json!({}),
                node_results: vec![serde_json::json!({"id": "x"})],
                graph_summary: serde_json::json!({}),
                comp: None,
                code_plan_run_artifacts: Vec::new(),
                run_markdown: Some("## done".into()),
                run_plasm_meta: None,
                return_steps: Vec::new(),
            },
            None,
        );
    });
    let out = await_operation_terminal(TerminalAwaitContext {
        es,
        st: minimal_host(),
        handle,
        trace: plasm_agent_core::trace_sink_emit::PlasmTraceContext {
            trace_id: Uuid::nil(),
            call_index: None,
            mcp_session_id: None,
            logical_session_id: None,
            logical_session_ref: None,
        },
        cfg: AwaitConfig {
            poll_interval: Duration::from_millis(20),
            max_wait: Duration::from_secs(2),
        },
    })
    .await
    .expect("success");
    assert_eq!(out.run_markdown.as_deref(), Some("## done"));
}
