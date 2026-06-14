//! Execute trace emission and MCP step publishing helpers.

use super::run_line::{parse_plasm_line_for_session, run_parsed_plasm_line};
use super::*;
use crate::run_artifacts::{persist_execute_run, PersistExecuteRunInput};

pub(crate) fn trace_expr_api_meta(expr: &plasm_core::Expr) -> (Option<String>, String) {
    use plasm_core::Expr;
    match expr {
        Expr::Query(q) => (
            q.capability_name.as_ref().map(|c| c.as_str().to_string()),
            "query".to_string(),
        ),
        Expr::Invoke(i) => (
            Some(i.capability.as_str().to_string()),
            "invoke".to_string(),
        ),
        Expr::Get(_) => (None, "get".to_string()),
        Expr::Create(c) => (
            Some(c.capability.as_str().to_string()),
            "create".to_string(),
        ),
        Expr::Delete(d) => (
            Some(d.capability.as_str().to_string()),
            "delete".to_string(),
        ),
        Expr::Chain(_) => (None, "chain".to_string()),
        Expr::Page(p) => (None, format!("page {}", p.handle)),
        Expr::Wait(w) => (None, format!("wait {}", w.handle)),
        Expr::Cancel(c) => (None, format!("cancel {}", c.handle)),
        Expr::TeachingValue { .. } => (None, "teaching_value".to_string()),
    }
}

pub(crate) fn trace_api_entry_id_for_execute_root(
    sess: &ExecuteSession,
    root_entity: &str,
) -> String {
    crate::catalog_ownership::entry_id_for_entity_trace(sess, root_entity)
}

fn trace_api_entry_id_for_parsed_line(sess: &ExecuteSession, parsed: &ParsedExpr) -> String {
    match &parsed.expr {
        plasm_core::Expr::Page(p) => sess
            .peek_paging_resume(&p.handle)
            .map(|r| trace_api_entry_id_for_execute_root(sess, r.query.entity.as_str()))
            .unwrap_or_else(|| sess.entry_id.clone()),
        plasm_core::Expr::TeachingValue { .. } => sess.entry_id.clone(),
        _ => trace_api_entry_id_for_execute_root(sess, parsed.expr.primary_entity()),
    }
}

pub(crate) fn plasm_line_trace_meta(
    line: &str,
    parsed: &ParsedExpr,
    result: &ExecutionResult,
    api_entry_id: Option<String>,
) -> PlasmLineTraceMeta {
    let (capability, operation) = trace_expr_api_meta(&parsed.expr);
    let mut repl_pre = String::from("→ ");
    repl_pre.push_str(&crate::expr_display::expr_display(&parsed.expr));
    if let Some(ref proj) = parsed.projection {
        if !proj.is_empty() {
            repl_pre.push_str(&format!("\n  projection: [{}]", proj.join(", ")));
        }
    }
    let repl_post = format!(
        "{} results · {:?} · {}ms · net {} · cache {}/{} · rows {}",
        result.count,
        result.source,
        result.stats.duration_ms,
        result.stats.network_requests,
        result.stats.cache_hits,
        result.stats.cache_misses,
        result.stats.cache.rows_materialized
    );
    PlasmLineTraceMeta {
        source_expression: line.to_string(),
        repl_pre,
        repl_post,
        capability,
        operation,
        api_entry_id,
    }
}

async fn trace_emit_plasm_line(
    sink: &McpPlasmTraceSink,
    line_index: usize,
    line: &str,
    parsed: &ParsedExpr,
    result: &ExecutionResult,
    sess: &ExecuteSession,
) {
    let api = Some(trace_api_entry_id_for_parsed_line(sess, parsed));
    let meta = plasm_line_trace_meta(line, parsed, result, api);
    sink.hub
        .trace_add_plasm_line(
            &sink.mcp_key,
            sink.call_index,
            line_index,
            meta,
            result,
            vec![],
        )
        .await;
}

fn run_line_error_string(e: RunLineError) -> String {
    match e {
        RunLineError::Parse(d) | RunLineError::Normalize(d) | RunLineError::Projection(d) => d,
        RunLineError::Runtime(e, src) => format!("{e}\nsource expression: {src}"),
        RunLineError::ArtifactSerialization(e) => format!("artifact serialization failed: {e}"),
        RunLineError::ArtifactPersist(d) => format!("run artifact persist failed: {d}"),
        RunLineError::StaleGraphEpoch {
            expected,
            found,
            attempts,
        } => format!(
            "session graph changed during execute (epoch {found:?} != {expected:?}) after {attempts} attempt(s); retry"
        ),
        RunLineError::Operation(_) => "operation continuation".to_string(),
    }
}

pub async fn execute_plasm_plasm_line(
    st: &PlasmHostState,
    sess: &ExecuteSession,
    session_id: &str,
    line: &str,
    trace: Option<&PlasmTraceContext>,
    line_index: i64,
) -> Result<(ParsedExpr, ExecutionResult, Option<RunArtifactHandle>), String> {
    let parsed = parse_plasm_line_for_session(line, sess, st).map_err(run_line_error_string)?;
    crate::execute_pipeline::ExecutePipeline::run_expression(
        line, sess, st, session_id, parsed, trace, line_index,
    )
    .await
    .map_err(run_line_error_string)
}

#[allow(clippy::too_many_arguments)]
pub async fn execute_plasm_parsed_expr(
    st: &PlasmHostState,
    sess: &ExecuteSession,
    session_id: &str,
    source_label: &str,
    parsed: ParsedExpr,
    trace: Option<&PlasmTraceContext>,
    line_index: i64,
    host_page_size: Option<usize>,
    surface_read_budget: Option<crate::plan_read_bounds::PushedReadBudget>,
    rows_progress: Option<plasm_runtime::RowsProgressFn>,
) -> Result<(ParsedExpr, ExecutionResult, Option<RunArtifactHandle>), String> {
    crate::execute_pipeline::PlasmPreflight::preflight_parsed_line(sess, source_label, &parsed)
        .map_err(|e| run_line_error_string(RunLineError::Parse(e)))?;
    run_parsed_plasm_line(
        source_label,
        sess,
        st,
        session_id,
        parsed,
        trace,
        line_index,
        host_page_size,
        surface_read_budget,
        rows_progress,
        Some(plasm_core::PreflightToken::VERIFIED),
    )
    .await
    .map_err(run_line_error_string)
}

pub async fn trace_record_plasm_line(
    sink: &McpPlasmTraceSink,
    line_index: usize,
    line: &str,
    parsed: &ParsedExpr,
    result: &ExecutionResult,
    sess: &ExecuteSession,
) {
    trace_emit_plasm_line(sink, line_index, line, parsed, result, sess).await;
}

#[allow(clippy::too_many_arguments)]
pub async fn archive_plasm_result_snapshot(
    st: &PlasmHostState,
    sess: &ExecuteSession,
    session_id: &str,
    entry_id_override: Option<&str>,
    display_lines: Vec<String>,
    parsed_preimage: &ParsedExpr,
    result: &ExecutionResult,
    trace: Option<&PlasmTraceContext>,
) -> Result<RunArtifactHandle, String> {
    let entry_id = entry_id_override.unwrap_or(sess.entry_id.as_str());
    let source_line = display_lines.join("\n");
    persist_execute_run(PersistExecuteRunInput {
        st,
        sess,
        session_id,
        entry_id,
        source_line: &source_line,
        display_lines,
        parsed: parsed_preimage,
        result,
        trace,
    })
    .await
    .map_err(|e| e.to_string())
}

/// Build `RunSealRecord` from the persisted run snapshot (same preimage as `mint_run_artifact_id`).
pub async fn run_seal_record_for_handle(
    st: &PlasmHostState,
    _es: &ExecuteSession,
    prompt_hash: &str,
    session_id: &str,
    handle: &RunArtifactHandle,
    step_id: Option<String>,
) -> Result<crate::evidence_chain::RunSealRecord, String> {
    let bytes = st
        .run_artifacts
        .get(prompt_hash, session_id, handle.run_id)
        .await
        .ok_or_else(|| "stored run artifact missing for run_sealed".to_string())?;
    let artifact: plasm_evidence::RunArtifactForSeal = serde_json::from_slice(&bytes)
        .map_err(|e| format!("artifact decode for run_sealed: {e}"))?;
    let source_line = artifact.source_line();
    Ok(crate::evidence_chain::RunSealRecord {
        expected_run_id_wire: handle.run_id.to_wire(),
        step_id,
        resource_index: Some(handle.resource_index),
        entry_id: artifact.entry_id,
        source_line,
        parsed: artifact.parsed_preimage,
        request_fingerprints: handle.request_fingerprints.clone(),
    })
}
