//! Long-operation wait/cancel/dispatch.

use super::*;
use crate::operation_error::OperationError;
use crate::run_artifacts::RunArtifactId;
use crate::trace_hub::CodePlanRunArtifactRef;

pub async fn handle_wait_operation(
    sess: &ExecuteSession,
    st: Option<&PlasmHostState>,
    trace: Option<&PlasmTraceContext>,
    handle: &plasm_core::OperationHandle,
) -> Result<crate::plasm_plan_run::PlasmPlanRunResult, OperationError> {
    let key = crate::operation::resolve_operation_storage_handle(trace, handle).map_err(|e| {
        OperationError::UnknownHandle {
            handle: handle.as_str().to_string(),
            hint: e,
        }
    })?;
    let hint = wait_hint(trace, handle, &key);

    if let Some(op) = sess.get_operation(&key) {
        if op.phase == crate::operation::OperationPhase::Running && !op.live_executor {
            return Ok(not_on_replica_running_result(sess, &key, &op));
        }
        if op.phase == crate::operation::OperationPhase::Succeeded && op.result.is_none() {
            if let (Some(st), Some(wire_id)) = (st, op.run_artifact_id.as_deref()) {
                let wire = sess.operation_wire_snapshot();
                let session_id = wire.session_id.as_deref().unwrap_or("");
                return hydrate_plan_run_from_artifact(st, sess, session_id, &key, wire_id).await;
            }
        }
        if op.phase == crate::operation::OperationPhase::Failed {
            if let Some(err) = op.error.clone() {
                return Err(OperationError::UnknownHandle {
                    handle: key.as_str().to_string(),
                    hint: err,
                });
            }
        }
    }

    let Some(snapshot) = sess.get_operation_poll_snapshot(&key) else {
        return Err(OperationError::UnknownHandle {
            handle: key.as_str().to_string(),
            hint,
        });
    };

    match snapshot {
        crate::operation::OperationPollSnapshot::Running(_) => {
            let Some((markdown, plasm_meta, _unchanged)) = sess.operation_poll_parts(&key) else {
                return Err(OperationError::UnknownHandle {
                    handle: key.as_str().to_string(),
                    hint,
                });
            };
            let mut meta = serde_json::Map::new();
            meta.insert("plasm".into(), serde_json::Value::Object(plasm_meta));
            Ok(empty_plan_run_with_markdown(markdown, meta))
        }
        crate::operation::OperationPollSnapshot::Succeeded(result) => Ok((*result).clone()),
        crate::operation::OperationPollSnapshot::Failed(error) => {
            Err(OperationError::UnknownHandle {
                handle: key.as_str().to_string(),
                hint: error,
            })
        }
        crate::operation::OperationPollSnapshot::Cancelled(progress) => {
            Ok(cancelled_plan_run_result(&key, &progress, sess))
        }
    }
}

fn wait_hint(
    trace: Option<&PlasmTraceContext>,
    handle: &plasm_core::OperationHandle,
    key: &plasm_core::OperationHandle,
) -> String {
    let logical_session_ref = trace
        .and_then(|t| t.logical_session_ref.as_deref())
        .unwrap_or("oN");
    if handle.is_plain() {
        format!("wait({})", key.as_str())
    } else {
        format!("wait({logical_session_ref}_oN)")
    }
}

fn not_on_replica_running_result(
    sess: &ExecuteSession,
    key: &plasm_core::OperationHandle,
    op: &crate::operation::OperationState,
) -> crate::plasm_plan_run::PlasmPlanRunResult {
    let markdown = sess
        .operation_poll_parts(key)
        .map(|(md, _, _)| md)
        .unwrap_or_else(|| {
            crate::operation_progress::render_op_wire_line(
                key,
                crate::operation_progress::OpWireSig::Running,
                Some(&op.progress),
                None,
                None,
                None,
            )
        });
    let mut meta = sess
        .operation_poll_parts(key)
        .map(|(_, m, _)| m)
        .unwrap_or_default();
    meta.insert(
        "code".into(),
        serde_json::Value::String(OperationError::CODE_NOT_ON_REPLICA.to_string()),
    );
    let mut root = serde_json::Map::new();
    root.insert("plasm".into(), serde_json::Value::Object(meta));
    empty_plan_run_with_markdown(markdown, root)
}

fn empty_plan_run_with_markdown(
    markdown: String,
    meta: serde_json::Map<String, serde_json::Value>,
) -> crate::plasm_plan_run::PlasmPlanRunResult {
    crate::plasm_plan_run::PlasmPlanRunResult {
        version: serde_json::json!({}),
        node_results: Vec::new(),
        graph_summary: serde_json::json!({}),
        comp: serde_json::json!({}),
        code_plan_run_artifacts: Vec::new(),
        run_markdown: Some(markdown),
        run_plasm_meta: Some(meta),
        return_steps: Vec::new(),
    }
}

fn cancelled_plan_run_result(
    key: &plasm_core::OperationHandle,
    progress: &crate::operation::OperationProgress,
    sess: &ExecuteSession,
) -> crate::plasm_plan_run::PlasmPlanRunResult {
    let markdown = crate::operation::operation_cancelled_markdown(key, Some(progress));
    let seq = sess
        .operation_progress_snapshot_line(key)
        .map(|(n, _)| n)
        .unwrap_or(1);
    let mut meta = serde_json::Map::new();
    meta.insert(
        "plasm".into(),
        serde_json::Value::Object(crate::operation::operation_meta_object(
            key,
            crate::operation_progress::OpWireSig::Cancelled,
            seq,
            Some(progress),
            None,
        )),
    );
    empty_plan_run_with_markdown(markdown, meta)
}

async fn hydrate_plan_run_from_artifact(
    st: &PlasmHostState,
    sess: &ExecuteSession,
    session_id: &str,
    handle: &plasm_core::OperationHandle,
    wire_id: &str,
) -> Result<crate::plasm_plan_run::PlasmPlanRunResult, OperationError> {
    let run_id =
        RunArtifactId::from_wire(wire_id).ok_or_else(|| OperationError::ResultArtifactMissing {
            handle: handle.as_str().to_string(),
            run_artifact_id: wire_id.to_string(),
        })?;
    let payload = st
        .run_artifacts
        .get_payload(sess.prompt_hash.as_str(), session_id, run_id)
        .await
        .ok_or_else(|| OperationError::ResultArtifactMissing {
            handle: handle.as_str().to_string(),
            run_artifact_id: wire_id.to_string(),
        })?;
    let doc: crate::run_artifacts::RunArtifactDocument = serde_json::from_slice(&payload.bytes)
        .map_err(|_| OperationError::ResultArtifactMissing {
            handle: handle.as_str().to_string(),
            run_artifact_id: wire_id.to_string(),
        })?;
    let markdown = if doc.entities.is_empty() {
        "## return (0 rows)\n\n```tsv\n(no rows)\n```".to_string()
    } else {
        format!(
            "## return ({} rows)\n\n```json\n{}\n```",
            doc.entities.len(),
            serde_json::to_string_pretty(&doc.entities).unwrap_or_else(|_| "[]".into())
        )
    };
    Ok(crate::plasm_plan_run::PlasmPlanRunResult {
        version: serde_json::json!({}),
        node_results: doc.entities,
        graph_summary: serde_json::json!({}),
        comp: serde_json::json!({}),
        code_plan_run_artifacts: vec![CodePlanRunArtifactRef {
            run_id: wire_id.to_string(),
            artifact_uri: None,
            canonical_artifact_uri: None,
            artifact_path: None,
            run_step: None,
            node_id: None,
            display: None,
            request_fingerprints: doc.request_fingerprints,
        }],
        run_markdown: Some(markdown),
        run_plasm_meta: None,
        return_steps: Vec::new(),
    })
}

/// Cancel an in-flight async plan operation (`cancel(l_<token>_oM)` or plain `cancel(oM)` on HTTP).
pub async fn handle_cancel_operation(
    sess: &ExecuteSession,
    trace: Option<&PlasmTraceContext>,
    handle: &plasm_core::OperationHandle,
) -> Result<crate::plasm_plan_run::PlasmPlanRunResult, OperationError> {
    let key = crate::operation::resolve_operation_storage_handle(trace, handle).map_err(|e| {
        OperationError::UnknownHandle {
            handle: handle.as_str().to_string(),
            hint: e,
        }
    })?;
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
        return Err(OperationError::UnknownHandle {
            handle: key.as_str().to_string(),
            hint: format!("cancel({})", key.as_str()),
        });
    }
    if !sess.cancel_operation(&key, None) {
        return Err(OperationError::UnknownHandle {
            handle: key.as_str().to_string(),
            hint: format!("cancel({})", key.as_str()),
        });
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
) -> Option<Result<crate::plasm_plan_run::PlasmPlanRunResult, String>> {
    let expr = crate::operation::try_parse_operation_continuation(sess, program)?;
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
