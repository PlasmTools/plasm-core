//! Execute trace emission and MCP step publishing helpers.

use super::run_line::{parse_plasm_line_for_session, run_parsed_plasm_line};
use super::*;
use crate::mcp_server::CodePlanTraceInput;
use crate::plasm_comp_wire::trace_comp_wire_from_dry;
use crate::run_artifacts::{persist_execute_run, PersistExecuteRunInput};
use plasm_trace::TraceCompWire;
use std::sync::Arc;

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

/// Where a executed expression line should land in the trace timeline.
pub(crate) enum PlasmLineTraceSink<'a> {
    /// MCP / plan run: hub timeline + durable ingest via trace hub worker.
    Hub(&'a McpPlasmTraceSink),
    /// Standalone HTTP execute (no hub sink on this path).
    Durable {
        st: &'a PlasmHostState,
        ctx: &'a PlasmTraceContext,
        sess: &'a ExecuteSession,
        session_id: &'a str,
        run_id: Option<String>,
    },
}

fn prepare_plasm_line_trace(
    line: &str,
    parsed: &ParsedExpr,
    result: &ExecutionResult,
    api_entry_id: Option<String>,
) -> (
    PlasmLineTraceMeta,
    Vec<plasm_runtime::http_trace::HttpTraceEntry>,
) {
    let meta = plasm_line_trace_meta(line, parsed, result, api_entry_id);
    let http_calls = plasm_runtime::drain_active_live_http_trace_entries();
    (meta, http_calls)
}

/// Single canonical `plasm_line` segment emit (hub and/or durable ingest).
pub(crate) async fn emit_plasm_line_trace(
    sink: PlasmLineTraceSink<'_>,
    line: &str,
    parsed: &ParsedExpr,
    result: &ExecutionResult,
    api_entry_id: Option<String>,
    call_index: u64,
    line_index: usize,
) {
    let (meta, http_calls) = prepare_plasm_line_trace(line, parsed, result, api_entry_id);
    match sink {
        PlasmLineTraceSink::Hub(hub_sink) => {
            hub_sink
                .hub
                .trace_add_plasm_line(
                    &hub_sink.mcp_key,
                    hub_sink.call_index,
                    line_index,
                    meta,
                    result,
                    http_calls,
                )
                .await;
        }
        PlasmLineTraceSink::Durable {
            st,
            ctx,
            sess,
            session_id,
            run_id,
        } => {
            use std::time::{SystemTime, UNIX_EPOCH};
            let wall_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let ev = TraceEvent::at(
                wall_ms,
                TraceSegment::PlasmLine {
                    call_index,
                    line_index,
                    source_expression: meta.source_expression,
                    repl_pre: meta.repl_pre,
                    repl_post: meta.repl_post,
                    capability: meta.capability,
                    operation: meta.operation,
                    api_entry_id: meta.api_entry_id,
                    duration_ms: result.stats.duration_ms,
                    stats: result.stats.clone(),
                    source: result.source,
                    request_fingerprints: result.request_fingerprints.clone(),
                    http_calls,
                },
            );
            crate::trace_sink_emit::spawn_emit_mcp_trace_segment(
                st.trace_ingest.as_ref(),
                &McpTraceAuditFields {
                    trace_id: ctx.trace_id,
                    mcp_session_id: ctx.mcp_session_id.clone(),
                    logical_session_id: ctx.logical_session_id.clone(),
                    plasm_prompt_hash: Some(sess.prompt_hash.to_string()),
                    plasm_execute_session: Some(session_id.to_string()),
                    run_id,
                    tenant_id: (!sess.tenant_scope.is_empty()).then(|| sess.tenant_scope.clone()),
                    principal_sub: (!sess.principal_subject.is_empty())
                        .then(|| sess.principal_subject.clone()),
                },
                &ev,
                None,
            );
        }
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
    emit_plasm_line_trace(
        PlasmLineTraceSink::Hub(sink),
        line,
        parsed,
        result,
        api,
        sink.call_index,
        line_index,
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
    plan_shared: Option<&crate::plan_execute_shared::PlanLineExecuteShared>,
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
        plan_shared,
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

/// Shared dry-run + logical-session context for HTTP code-plan trace emission.
struct HttpCodePlanTraceContext {
    ls_key: String,
    session_ref: String,
    comp_wire: Arc<TraceCompWire>,
    plan_ux_reflection: Option<serde_json::Value>,
}

async fn http_code_plan_trace_context(
    st: &PlasmHostState,
    sess: &ExecuteSession,
    prompt_hash: &str,
    session_id: &str,
    bundle: &crate::plasm_comp_bundle::PlasmCompBundle,
) -> Option<HttpCodePlanTraceContext> {
    let Ok(dry) = crate::plasm_plan_run::evaluate_plasm_comp_dry(sess, bundle) else {
        return None;
    };
    let Some(logical_uuid) = st
        .logical_session_id_for_execute_binding(prompt_hash, session_id)
        .await
    else {
        tracing::debug!(
            target: "plasm_agent::http_execute",
            %prompt_hash,
            %session_id,
            "skip code_plan trace emit: no MCP logical session binding"
        );
        return None;
    };
    Some(HttpCodePlanTraceContext {
        ls_key: logical_uuid.to_string(),
        session_ref: crate::mcp_logical_ref::format_logical_session_wire_ref(
            crate::session_identity::LogicalSessionId(logical_uuid),
        ),
        comp_wire: Arc::new(trace_comp_wire_from_dry(&dry)),
        plan_ux_reflection: Some(crate::plan_ux_reflection::plan_ux_reflection_value(
            &dry,
            &crate::plan_ux_reflection::PlanUxBuildContext {
                session: Some(sess),
                param_bindings: &[],
            },
        )),
    })
}

/// Emit `code_plan_evaluate` when HTTP execute dry-run shares an MCP logical session binding.
pub(crate) async fn maybe_emit_http_code_plan_evaluate(
    st: &PlasmHostState,
    sess: &ExecuteSession,
    prompt_hash: &str,
    session_id: &str,
    program: &str,
    bundle: &crate::plasm_comp_bundle::PlasmCompBundle,
    plan_call_index: u64,
) {
    let Some(ctx) = http_code_plan_trace_context(st, sess, prompt_hash, session_id, bundle).await
    else {
        return;
    };
    let input = CodePlanTraceInput {
        hub: &st.trace_hub,
        store: &st.run_artifacts,
        mcp_key: &ctx.ls_key,
        es: sess,
        prompt_hash,
        session_id,
        session_ref: &ctx.session_ref,
        comp: ctx.comp_wire,
        program,
        plan_call_index,
        code_chars: program.chars().count() as u64,
    };
    input.emit_evaluate(ctx.plan_ux_reflection).await;
}

/// Emit `code_plan_execute` when HTTP live run shares an MCP logical session binding.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn maybe_emit_http_code_plan_execute(
    st: &PlasmHostState,
    sess: &ExecuteSession,
    prompt_hash: &str,
    session_id: &str,
    program: &str,
    bundle: &crate::plasm_comp_bundle::PlasmCompBundle,
    plan_call_index: u64,
    out: &crate::plasm_plan_run::PlasmPlanRunResult,
) {
    let Some(ctx) = http_code_plan_trace_context(st, sess, prompt_hash, session_id, bundle).await
    else {
        return;
    };
    let comp = match out.comp.as_ref() {
        Some(wire) => Arc::new(wire.clone()),
        None => {
            tracing::warn!(
                target: "plasm_agent::http_execute",
                %prompt_hash,
                %session_id,
                "execute trace missing comp; skipping code_plan_execute trace emit"
            );
            return;
        }
    };
    let input = CodePlanTraceInput {
        hub: &st.trace_hub,
        store: &st.run_artifacts,
        mcp_key: &ctx.ls_key,
        es: sess,
        prompt_hash,
        session_id,
        session_ref: &ctx.session_ref,
        comp,
        program,
        plan_call_index,
        code_chars: program.chars().count() as u64,
    };
    input
        .emit_execute_completed(None, ctx.plan_ux_reflection, out)
        .await;
}
