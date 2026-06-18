//! Long-operation wait/cancel/dispatch.

use super::*;
use crate::operation_error::OperationError;
use crate::terminal_plan_run::{cancelled_plan_run_result, resolve_terminal_plan_run};

fn open_handle_strings(sess: &ExecuteSession) -> Vec<String> {
    sess.open_live_operation_handles()
        .into_iter()
        .map(|h| h.as_str().to_string())
        .collect()
}

fn session_unknown_handle(
    sess: &ExecuteSession,
    handle: impl AsRef<str>,
    hint: impl Into<String>,
) -> OperationError {
    OperationError::UnknownHandle {
        handle: handle.as_ref().to_string(),
        hint: hint.into(),
        open_handles: open_handle_strings(sess),
    }
}

pub async fn handle_wait_operation(
    sess: &ExecuteSession,
    st: Option<&PlasmHostState>,
    trace: Option<&PlasmTraceContext>,
    handle: &plasm_core::OperationHandle,
) -> Result<crate::plasm_plan_run::PlasmPlanRunResult, OperationError> {
    resolve_terminal_plan_run(sess, st, trace, handle).await
}

/// Cancel an in-flight async plan operation (`cancel(l_<token>_oM)` or plain `cancel(oM)` on HTTP).
pub async fn handle_cancel_operation(
    sess: &ExecuteSession,
    trace: Option<&PlasmTraceContext>,
    handle: &plasm_core::OperationHandle,
) -> Result<crate::plasm_plan_run::PlasmPlanRunResult, OperationError> {
    let key = crate::operation::resolve_operation_storage_handle(trace, handle)
        .map_err(|e| session_unknown_handle(sess, handle.as_str(), e))?;
    if !sess.operation_has_live_executor(&key) {
        if let Some(op) = sess.get_operation(&key) {
            if op.phase == crate::operation::OperationPhase::Running {
                return Err(OperationError::NotOnReplica {
                    handle: key.as_str().to_string(),
                    progress: op.progress.clone(),
                    agent_seq: op.agent_emit.seq,
                    agent_last_line: op.agent_emit.last_line.clone(),
                });
            }
        }
        return Err(session_unknown_handle(
            sess,
            key.as_str(),
            format!("cancel({})", key.as_str()),
        ));
    }
    if !sess.cancel_operation(&key, None) {
        return Err(session_unknown_handle(
            sess,
            key.as_str(),
            format!("cancel({})", key.as_str()),
        ));
    }
    let progress = sess.get_operation_progress(&key).unwrap_or_default();
    Ok(cancelled_plan_run_result(&key, &progress, sess))
}

pub fn operation_error_to_string(err: OperationError) -> String {
    err.detail()
}

/// Default trace for HTTP execute long-op handles (plain `oN`) when no MCP `plasm_context`.
pub(crate) fn http_operation_trace() -> crate::trace_sink_emit::PlasmTraceContext {
    crate::trace_sink_emit::PlasmTraceContext {
        trace_id: Uuid::nil(),
        call_index: None,
        mcp_session_id: None,
        logical_session_id: None,
        logical_session_ref: None,
    }
}

/// Dispatch `wait(...)` / `cancel(...)` program bodies before plan compile.
pub async fn try_dispatch_operation_program(
    sess: &ExecuteSession,
    st: Option<&PlasmHostState>,
    trace: Option<&PlasmTraceContext>,
    program: &str,
    symbol_map_cross_cache: Option<&plasm_core::SymbolMapCrossRequestCache>,
) -> Option<Result<crate::plasm_plan_run::PlasmPlanRunResult, String>> {
    let expr =
        crate::operation::try_parse_operation_continuation(sess, program, symbol_map_cross_cache)?;
    Some(match expr {
        Expr::Wait(w) => handle_wait_operation(sess, st, trace, &w.handle)
            .await
            .map_err(operation_error_to_string),
        Expr::Cancel(c) => handle_cancel_operation(sess, trace, &c.handle)
            .await
            .map_err(operation_error_to_string),
        _ => None?,
    })
}
