//! Server-side await loop for MCP live runs (internal async; one terminal tool response).

use std::sync::Arc;
use std::time::Duration;

use plasm_core::OperationHandle;

use crate::execute_session::ExecuteSession;
use crate::operation::OperationPhase;
use crate::plasm_plan_run::PlasmPlanRunResult;
use crate::server_state::PlasmHostState;
use crate::terminal_plan_run::{plan_run_result_is_terminal, resolve_terminal_plan_run};
use crate::trace_sink_emit::PlasmTraceContext;

#[derive(Debug, Clone)]
pub struct AwaitConfig {
    pub poll_interval: Duration,
    pub max_wait: Duration,
}

impl Default for AwaitConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(200),
            max_wait: Duration::from_secs(600),
        }
    }
}

pub struct TerminalAwaitContext {
    pub es: Arc<ExecuteSession>,
    pub st: Arc<PlasmHostState>,
    pub handle: OperationHandle,
    pub trace: PlasmTraceContext,
    pub cfg: AwaitConfig,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AwaitError {
    #[error("await timed out after {0:?}")]
    Timeout(Duration),
    #[error("{0}")]
    Operation(String),
}

fn operation_is_terminal(phase: OperationPhase) -> bool {
    matches!(
        phase,
        OperationPhase::Succeeded | OperationPhase::Failed | OperationPhase::Cancelled
    )
}

async fn fetch_terminal_result(
    ctx: &TerminalAwaitContext,
) -> Result<PlasmPlanRunResult, AwaitError> {
    resolve_terminal_plan_run(
        ctx.es.as_ref(),
        Some(ctx.st.as_ref()),
        Some(&ctx.trace),
        &ctx.handle,
    )
    .await
    .map_err(|e| AwaitError::Operation(e.detail()))
}

async fn try_fetch_terminal_result(
    ctx: &TerminalAwaitContext,
) -> Result<Option<PlasmPlanRunResult>, AwaitError> {
    match fetch_terminal_result(ctx).await {
        Ok(result) if plan_run_result_is_terminal(&result) => Ok(Some(result)),
        Ok(_) => Ok(None),
        Err(AwaitError::Operation(_)) => Ok(None),
        Err(e @ AwaitError::Timeout(_)) => Err(e),
    }
}

async fn poll_until_terminal_result(
    ctx: &TerminalAwaitContext,
) -> Result<PlasmPlanRunResult, AwaitError> {
    loop {
        if let Some(result) = try_fetch_terminal_result(ctx).await? {
            return Ok(result);
        }
        let phase = ctx
            .es
            .get_operation(&ctx.handle)
            .map(|op| op.phase)
            .unwrap_or(OperationPhase::Running);
        if matches!(phase, OperationPhase::Failed | OperationPhase::Cancelled) {
            return fetch_terminal_result(ctx).await;
        }
        tokio::time::sleep(ctx.cfg.poll_interval).await;
    }
}

async fn await_via_terminal_watch(
    ctx: &TerminalAwaitContext,
) -> Result<PlasmPlanRunResult, AwaitError> {
    let Some(mut rx) = ctx.es.subscribe_operation_terminal(&ctx.handle) else {
        return await_via_cross_pod_poll(ctx).await;
    };
    while !operation_is_terminal(*rx.borrow()) {
        if rx.changed().await.is_err() {
            break;
        }
    }
    poll_until_terminal_result(ctx).await
}

async fn await_via_cross_pod_poll(
    ctx: &TerminalAwaitContext,
) -> Result<PlasmPlanRunResult, AwaitError> {
    loop {
        if let Some(result) = try_fetch_terminal_result(ctx).await? {
            return Ok(result);
        }
        tokio::time::sleep(ctx.cfg.poll_interval).await;
    }
}

pub async fn await_operation_terminal(
    ctx: TerminalAwaitContext,
) -> Result<PlasmPlanRunResult, AwaitError> {
    tokio::time::timeout(ctx.cfg.max_wait, async {
        if ctx.es.subscribe_operation_terminal(&ctx.handle).is_some() {
            await_via_terminal_watch(&ctx).await
        } else {
            await_via_cross_pod_poll(&ctx).await
        }
    })
    .await
    .map_err(|_| AwaitError::Timeout(ctx.cfg.max_wait))?
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::operation::OpAcceptContext;
    use crate::operation_progress::{op_plasm_meta_short, OpWireSig};

    fn minimal_host() -> Arc<PlasmHostState> {
        let engine = plasm_runtime::ExecutionEngine::new(plasm_runtime::ExecutionConfig::default())
            .expect("engine");
        Arc::new(crate::http::build_plasm_host_state(
            crate::http::PlasmHostBootstrap {
                engine,
                mode: plasm_runtime::ExecutionMode::Live,
                registry: Arc::new(plasm_core::discovery::InMemoryCgsRegistry::from_pairs(
                    Vec::new(),
                )),
                catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
                plugin_manager: None,
                incoming_auth: None,
                run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
                session_graph_persistence: None,
                oss_local_filesystem_defaults: false,
            },
        ))
    }

    fn plain_trace() -> PlasmTraceContext {
        PlasmTraceContext {
            trace_id: Uuid::nil(),
            call_index: None,
            mcp_session_id: None,
            logical_session_id: None,
            logical_session_ref: None,
        }
    }

    #[test]
    fn plan_run_result_is_terminal_rejects_poll_snapshot() {
        let handle = OperationHandle::parse("o1").expect("handle");
        let mut root = serde_json::Map::new();
        root.insert(
            "plasm".into(),
            serde_json::Value::Object(op_plasm_meta_short(
                &handle,
                OpWireSig::Unchanged,
                2,
                None,
                None,
            )),
        );
        let poll = PlasmPlanRunResult {
            version: serde_json::json!({}),
            node_results: Vec::new(),
            graph_summary: serde_json::json!({}),
            comp: serde_json::json!({}),
            code_plan_run_artifacts: Vec::new(),
            run_markdown: Some(format!("`{}` =", handle.as_str())),
            run_plasm_meta: Some(root),
            return_steps: Vec::new(),
        };
        assert!(!plan_run_result_is_terminal(&poll));
    }

    #[tokio::test]
    async fn await_returns_when_operation_finalizes() {
        let es = Arc::new(crate::execute_session::ExecuteSession::new(
            "ph".into(),
            "p".into(),
            Arc::new(plasm_core::CGS::new()),
            indexmap::IndexMap::new(),
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            "hash".into(),
            None,
            None,
        ));
        let handle = es.mint_operation_handle_plain();
        es.try_begin_async_operation(
            handle.clone(),
            plasm_runtime::CancelSignal::new(),
            OpAcceptContext::default(),
        )
        .expect("begin");
        let st = minimal_host();
        let es_bg = Arc::clone(&es);
        let handle_bg = handle.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            es_bg.finalize_operation_succeeded(
                &handle_bg,
                PlasmPlanRunResult {
                    version: serde_json::json!({}),
                    node_results: vec![serde_json::json!({"id": "x"})],
                    graph_summary: serde_json::json!({}),
                    comp: serde_json::json!({}),
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
            st,
            handle,
            trace: plain_trace(),
            cfg: AwaitConfig {
                poll_interval: Duration::from_millis(20),
                max_wait: Duration::from_secs(5),
            },
        })
        .await
        .expect("terminal");
        assert_eq!(out.run_markdown.as_deref(), Some("## done"));
    }

    #[tokio::test]
    async fn await_times_out_while_operation_still_running() {
        let es = Arc::new(crate::execute_session::ExecuteSession::new(
            "ph".into(),
            "p".into(),
            Arc::new(plasm_core::CGS::new()),
            indexmap::IndexMap::new(),
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            "hash".into(),
            None,
            None,
        ));
        let handle = es.mint_operation_handle_plain();
        es.try_begin_async_operation(
            handle.clone(),
            plasm_runtime::CancelSignal::new(),
            OpAcceptContext::default(),
        )
        .expect("begin");
        let err = await_operation_terminal(TerminalAwaitContext {
            es,
            st: minimal_host(),
            handle,
            trace: plain_trace(),
            cfg: AwaitConfig {
                poll_interval: Duration::from_millis(20),
                max_wait: Duration::from_millis(80),
            },
        })
        .await
        .expect_err("timeout");
        assert!(matches!(err, AwaitError::Timeout(_)));
    }

    #[tokio::test]
    async fn await_does_not_return_early_on_unchanged_poll() {
        let es = Arc::new(crate::execute_session::ExecuteSession::new(
            "ph".into(),
            "p".into(),
            Arc::new(plasm_core::CGS::new()),
            indexmap::IndexMap::new(),
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            "hash".into(),
            None,
            None,
        ));
        let handle = es.mint_operation_handle_plain();
        es.try_begin_async_operation(
            handle.clone(),
            plasm_runtime::CancelSignal::new(),
            OpAcceptContext::default(),
        )
        .expect("begin");
        let st = minimal_host();
        let es_bg = Arc::clone(&es);
        let handle_bg = handle.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            es_bg.finalize_operation_succeeded(
                &handle_bg,
                PlasmPlanRunResult {
                    version: serde_json::json!({}),
                    node_results: vec![serde_json::json!({"id": "x"})],
                    graph_summary: serde_json::json!({}),
                    comp: serde_json::json!({}),
                    code_plan_run_artifacts: Vec::new(),
                    run_markdown: Some("## return_1 (1 rows)".into()),
                    run_plasm_meta: None,
                    return_steps: Vec::new(),
                },
                None,
            );
        });
        let out = await_operation_terminal(TerminalAwaitContext {
            es,
            st,
            handle,
            trace: plain_trace(),
            cfg: AwaitConfig {
                poll_interval: Duration::from_millis(20),
                max_wait: Duration::from_secs(5),
            },
        })
        .await
        .expect("terminal after poll");
        assert!(
            out.run_markdown
                .as_deref()
                .is_some_and(|m| m.contains("(1 rows)")),
            "expected row markdown, got {:?}",
            out.run_markdown
        );
    }
}
