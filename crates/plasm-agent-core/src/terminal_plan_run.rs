//! Terminal operation resolution for server-side await and client `wait(oN)` poll.

use crate::execute_session::ExecuteSession;
use crate::operation_error::OperationError;
use crate::plasm_plan_run::PlasmPlanRunResult;
use crate::run_artifacts::RunArtifactId;
use crate::server_state::PlasmHostState;
use crate::trace_hub::CodePlanRunArtifactRef;
use crate::trace_sink_emit::PlasmTraceContext;
use plasm_core::OperationHandle;

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

fn wait_hint(
    trace: Option<&PlasmTraceContext>,
    handle: &OperationHandle,
    key: &OperationHandle,
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

fn empty_plan_run_with_markdown(
    markdown: String,
    meta: serde_json::Map<String, serde_json::Value>,
) -> PlasmPlanRunResult {
    PlasmPlanRunResult {
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

fn not_on_replica_running_result(
    sess: &ExecuteSession,
    key: &OperationHandle,
    op: &crate::operation::OperationState,
) -> PlasmPlanRunResult {
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

pub(crate) fn cancelled_plan_run_result(
    key: &OperationHandle,
    progress: &crate::operation::OperationProgress,
    sess: &ExecuteSession,
) -> PlasmPlanRunResult {
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
    handle: &OperationHandle,
    wire_id: &str,
) -> Result<PlasmPlanRunResult, OperationError> {
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
    let (out, node_results) =
        crate::terminal_result_format::hydrate_plan_run_from_artifact_formatted(
            &doc, sess, wire_id,
        )?;
    Ok(PlasmPlanRunResult {
        version: serde_json::json!({}),
        node_results,
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
        run_markdown: Some(out.markdown),
        run_plasm_meta: out.tool_meta,
        return_steps: Vec::new(),
    })
}

/// Resolve terminal rows for an async live plan operation (shared by MCP await and HTTP `wait(oN)`).
pub async fn resolve_terminal_plan_run(
    sess: &ExecuteSession,
    st: Option<&PlasmHostState>,
    trace: Option<&PlasmTraceContext>,
    handle: &OperationHandle,
) -> Result<PlasmPlanRunResult, OperationError> {
    let key = crate::operation::resolve_operation_storage_handle(trace, handle)
        .map_err(|e| session_unknown_handle(sess, handle.as_str(), e))?;
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
                return Err(session_unknown_handle(sess, key.as_str(), err));
            }
        }
    }

    let Some(snapshot) = sess.get_operation_poll_snapshot(&key) else {
        return Err(session_unknown_handle(sess, key.as_str(), hint));
    };

    match snapshot {
        crate::operation::OperationPollSnapshot::Running(_) => {
            let Some((markdown, plasm_meta, _unchanged)) = sess.operation_poll_parts(&key) else {
                return Err(session_unknown_handle(sess, key.as_str(), hint));
            };
            let mut meta = serde_json::Map::new();
            meta.insert("plasm".into(), serde_json::Value::Object(plasm_meta));
            Ok(empty_plan_run_with_markdown(markdown, meta))
        }
        crate::operation::OperationPollSnapshot::Succeeded(result) => Ok((*result).clone()),
        crate::operation::OperationPollSnapshot::Failed(error) => {
            Err(session_unknown_handle(sess, key.as_str(), error))
        }
        crate::operation::OperationPollSnapshot::Cancelled(progress) => {
            Ok(cancelled_plan_run_result(&key, &progress, sess))
        }
    }
}
