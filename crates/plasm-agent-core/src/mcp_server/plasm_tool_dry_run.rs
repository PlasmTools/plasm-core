//! MCP `plasm` dry-run path: compile, evaluate, commit, trace, and build [`PlasmPlanRunResult`].

use std::sync::Arc;
use std::time::Instant;

use crate::execute_session::ExecuteSession;
use crate::mcp_ui_payload::{inline_ui_payload_fits, UiInlinePlanPayload};
use crate::metrics::record_mcp_plasm_dry_run_phase;
use crate::operation::{compute_plan_commit_id_from_dry, PlanCommitRecord, PLAN_COMMIT_TTL};
use crate::plan_dry_display::build_plan_dry_compact_view;
use crate::plan_gate::{plan_gate, PlanGateContext};
use crate::plasm_comp_wire::trace_comp_wire_from_dry;
use crate::plasm_plan_run::{
    evaluate_plasm_comp_dry, render_plasm_plan_dry_text_for_session, PlasmPlanRunResult,
};
use crate::server_state::PlasmHostState;

use super::plasm_tool_dry_meta;
use super::trace::CodePlanTraceInput;
use super::transport::PlasmExecBinding;

pub(crate) struct PlasmDryRunOutcome {
    pub result: PlasmPlanRunResult,
    pub inline_plan_ui: Option<UiInlinePlanPayload>,
}

/// Unified dry/live MCP `plasm` / `plasm_run` tool outcome for finalization.
pub(crate) struct PlasmToolRunOutcome {
    pub result: PlasmPlanRunResult,
    pub inline_plan_ui: Option<UiInlinePlanPayload>,
}

pub(crate) struct PlasmDryRunContext<'a> {
    pub host: &'a PlasmHostState,
    pub es: Arc<ExecuteSession>,
    pub binding: &'a PlasmExecBinding,
    pub session_ref: &'a str,
    pub ls_key: &'a str,
    pub call_index: u64,
}

pub(crate) async fn execute_plasm_tool_dry_run(
    ctx: PlasmDryRunContext<'_>,
    program: &str,
) -> Result<PlasmDryRunOutcome, String> {
    let total_started = Instant::now();
    let plan_name = format!("plasm_dag_call_{}", ctx.call_index);
    let pipeline = ctx.host.engine.prompt_pipeline();
    let cross = ctx.host.sessions.symbol_map_cross_cache();

    let mut phase = Instant::now();
    let bundle = crate::compile_plasm_expression(
        pipeline,
        Some(cross),
        ctx.es.as_ref(),
        &plan_name,
        program,
    )?;
    record_mcp_plasm_dry_run_phase("compile", phase.elapsed());

    phase = Instant::now();
    let program_for_trace = program.to_string();
    let dry = evaluate_plasm_comp_dry(ctx.es.as_ref(), &bundle)?;
    record_mcp_plasm_dry_run_phase("dry_eval", phase.elapsed());

    if !dry.probe_preflight_passed() {
        return Err("plan dry-run preflight failed — fix errors before run_ref".into());
    }

    phase = Instant::now();
    let dry_text = render_plasm_plan_dry_text_for_session(&dry, None, Some(ctx.es.as_ref()));
    let comp_wire = Arc::new(trace_comp_wire_from_dry(&dry));
    let compact = build_plan_dry_compact_view(
        dry.validated_plan(),
        &dry.topological_order,
        &dry.review,
        &dry.graph_summary,
        Some(ctx.es.as_ref()),
        None,
    );
    let flow_gate = dry.evaluate_gate();
    if let crate::PlanGateDecision::Denied(denial) = plan_gate(
        &flow_gate,
        PlanGateContext {
            force: false,
            plan_commit_ref: None,
        },
    ) {
        return Err(format!(
            "plan denied by flow policy ({:?}): {} violation(s)",
            denial.verdict,
            denial.violations.len()
        ));
    }
    let commit_ref = ctx.es.mint_plan_commit_ref();
    let agent_plan_text = dry_text.clone();
    let mut markdown = format!("```text\n{dry_text}\n```");
    markdown.push_str(&format!(
        "\n\n**Run:** pass `run_ref`: `{}` to **`plasm_run`**. Do not echo the program.",
        commit_ref.as_str()
    ));
    let commit_record = PlanCommitRecord::from_dry_review(
        commit_ref.clone(),
        compute_plan_commit_id_from_dry(&dry),
        ctx.es.domain_revision,
        &dry,
        program_for_trace.clone(),
        compact.verdict,
        std::time::Instant::now() + PLAN_COMMIT_TTL,
    )
    .map_err(|denial| {
        format!(
            "plan commit blocked by flow policy ({:?}): {} violation(s)",
            denial.verdict,
            denial.violations.len()
        )
    })?;
    let ux_ctx = crate::plan_ux_reflection::PlanUxBuildContext {
        session: Some(ctx.es.as_ref()),
        param_bindings: &[],
    };
    let plan_ux_reflection = crate::plan_ux_reflection::plan_ux_reflection_value(&dry, &ux_ctx);
    let comp_val = serde_json::to_value(comp_wire.as_ref()).ok();
    let inline_fits = comp_val
        .as_ref()
        .is_some_and(|comp| inline_ui_payload_fits(comp, &plan_ux_reflection));
    record_mcp_plasm_dry_run_phase("prepare", phase.elapsed());

    phase = Instant::now();
    crate::plan_commit_store::register_plan_commit_with_persist(
        ctx.host,
        ctx.binding.prompt_hash.as_str(),
        ctx.binding.session_id.as_str(),
        commit_record,
        !inline_fits,
    )
    .await
    .map_err(|e| e.to_string())?;
    record_mcp_plasm_dry_run_phase("commit_register", phase.elapsed());

    phase = Instant::now();
    let plan_refs = CodePlanTraceInput {
        hub: &ctx.host.trace_hub,
        store: Arc::clone(&ctx.host.run_artifacts),
        mcp_key: ctx.ls_key,
        es: ctx.es.as_ref(),
        prompt_hash: ctx.binding.prompt_hash.as_str(),
        session_id: ctx.binding.session_id.as_str(),
        comp: Arc::clone(&comp_wire),
        program: &program_for_trace,
        plan_call_index: ctx.call_index,
        code_chars: program_for_trace.chars().count() as u64,
    }
    .emit_evaluate(Some(plan_ux_reflection.clone()), inline_fits)
    .await;
    record_mcp_plasm_dry_run_phase("trace_emit", phase.elapsed());

    phase = Instant::now();
    let projection_warning = dry
        .graph_summary
        .get("dry_review")
        .and_then(|v| v.get("has_unprojected_multi_row_read"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let agent_plasm = plasm_tool_dry_meta::build_dry_run_agent_plasm_meta(
        &commit_ref,
        compact.verdict,
        ctx.session_ref,
        &plan_refs,
        ctx.es.domain_revision,
        crate::symbol_map_resolve::symbol_map_fingerprint_for_session(ctx.es.as_ref())
            .as_deref(),
        projection_warning,
    );
    let inline_plan_ui = inline_fits.then(|| UiInlinePlanPayload {
        comp: comp_val.expect("inline_fits implies comp_val"),
        plan_ux_reflection: plan_ux_reflection.clone(),
    });
    let mut meta = serde_json::Map::new();
    meta.insert("plasm".into(), serde_json::Value::Object(agent_plasm));
    record_mcp_plasm_dry_run_phase("build_response", phase.elapsed());
    record_mcp_plasm_dry_run_phase("total", total_started.elapsed());

    Ok(PlasmDryRunOutcome {
        result: PlasmPlanRunResult {
            version: dry.version,
            node_results: dry.node_results,
            graph_summary: dry.graph_summary,
            comp: Some(comp_wire.as_ref().clone()),
            code_plan_run_artifacts: Vec::new(),
            run_markdown: Some(markdown),
            run_plasm_meta: Some(meta),
            agent_structured_plan_text: Some(agent_plan_text),
            return_steps: Vec::new(),
        },
        inline_plan_ui,
    })
}
