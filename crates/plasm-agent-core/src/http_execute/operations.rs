//! Long-operation wait/cancel/dispatch.

use super::*;

pub async fn handle_wait_operation(
    sess: &ExecuteSession,
    trace: Option<&PlasmTraceContext>,
    handle: &plasm_core::OperationHandle,
) -> Result<crate::plasm_plan_run::PlasmPlanRunResult, String> {
    let key = crate::operation::resolve_operation_storage_handle(trace, handle)?;
    let logical_session_ref = trace
        .and_then(|t| t.logical_session_ref.as_deref())
        .unwrap_or("oN");
    let hint = if handle.is_plain() {
        format!("wait({})", key.as_str())
    } else {
        format!("wait({logical_session_ref}_oN)")
    };
    let Some(snapshot) = sess.get_operation_poll_snapshot(&key) else {
        return Err(format!(
            "unknown operation handle `{}` — stale continuation or wrong logical session; use `{hint}` from the latest tool result",
            key.as_str()
        ));
    };
    match snapshot {
        crate::operation::OperationPollSnapshot::Running(_) => {
            let Some((markdown, plasm_meta, _unchanged)) = sess.operation_poll_parts(&key) else {
                return Err(format!("unknown operation handle `{}`", key.as_str()));
            };
            let mut meta = serde_json::Map::new();
            meta.insert("plasm".into(), serde_json::Value::Object(plasm_meta));
            Ok(crate::plasm_plan_run::PlasmPlanRunResult {
                version: serde_json::json!({}),
                node_results: Vec::new(),
                graph_summary: serde_json::json!({}),
                comp: serde_json::json!({}),
                code_plan_run_artifacts: Vec::new(),
                run_markdown: Some(markdown),
                run_plasm_meta: Some(meta),
                return_steps: Vec::new(),
            })
        }
        crate::operation::OperationPollSnapshot::Succeeded(result) => Ok((*result).clone()),
        crate::operation::OperationPollSnapshot::Failed(error) => Err(error),
        crate::operation::OperationPollSnapshot::Cancelled(progress) => {
            let markdown = crate::operation::operation_cancelled_markdown(&key, Some(&progress));
            let seq = sess
                .operation_progress_snapshot_line(&key)
                .map(|(n, _)| n)
                .unwrap_or(1);
            let mut meta = serde_json::Map::new();
            meta.insert(
                "plasm".into(),
                serde_json::Value::Object(crate::operation::operation_meta_object(
                    &key,
                    crate::operation_progress::OpWireSig::Cancelled,
                    seq,
                    Some(&progress),
                    None,
                )),
            );
            Ok(crate::plasm_plan_run::PlasmPlanRunResult {
                version: serde_json::json!({}),
                node_results: Vec::new(),
                graph_summary: serde_json::json!({}),
                comp: serde_json::json!({}),
                code_plan_run_artifacts: Vec::new(),
                run_markdown: Some(markdown),
                run_plasm_meta: Some(meta),
                return_steps: Vec::new(),
            })
        }
    }
}

/// Cancel an in-flight async plan operation (`cancel(l_<token>_oM)` or plain `cancel(oM)` on HTTP).
pub async fn handle_cancel_operation(
    sess: &ExecuteSession,
    trace: Option<&PlasmTraceContext>,
    handle: &plasm_core::OperationHandle,
) -> Result<crate::plasm_plan_run::PlasmPlanRunResult, String> {
    let key = crate::operation::resolve_operation_storage_handle(trace, handle)?;
    if !sess.cancel_operation(&key, None) {
        return Err(format!("unknown operation handle `{}`", key.as_str()));
    }
    let progress = sess.get_operation_progress(&key).unwrap_or_default();
    let markdown = crate::operation::operation_cancelled_markdown(&key, Some(&progress));
    let seq = sess
        .operation_progress_snapshot_line(&key)
        .map(|(n, _)| n)
        .unwrap_or(1);
    let mut meta = serde_json::Map::new();
    meta.insert(
        "plasm".into(),
        serde_json::Value::Object(crate::operation::operation_meta_object(
            &key,
            crate::operation_progress::OpWireSig::Cancelled,
            seq,
            Some(&progress),
            None,
        )),
    );
    Ok(crate::plasm_plan_run::PlasmPlanRunResult {
        version: serde_json::json!({}),
        node_results: Vec::new(),
        graph_summary: serde_json::json!({}),
        comp: serde_json::json!({}),
        code_plan_run_artifacts: Vec::new(),
        run_markdown: Some(markdown),
        run_plasm_meta: Some(meta),
        return_steps: Vec::new(),
    })
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
    trace: Option<&PlasmTraceContext>,
    program: &str,
) -> Option<Result<crate::plasm_plan_run::PlasmPlanRunResult, String>> {
    let expr = crate::operation::try_parse_operation_continuation(sess, program)?;
    Some(match expr {
        Expr::Wait(w) => handle_wait_operation(sess, trace, &w.handle).await,
        Expr::Cancel(c) => handle_cancel_operation(sess, trace, &c.handle).await,
        _ => None?,
    })
}
