//! MCP `plasm` dry-run path: compile, evaluate, commit (writes), or fuse clean reads.

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
use crate::trace_hub::PlanRunTraceHooks;
use crate::trace_sink_emit::PlasmTraceContext;

use super::plasm_tool_dry_meta;
use super::trace::CodePlanTraceInput;
use super::transport::PlasmExecBinding;

pub(crate) struct PlasmDryRunContext<'a> {
    pub host: Arc<PlasmHostState>,
    pub es: Arc<ExecuteSession>,
    pub binding: &'a PlasmExecBinding,
    pub session_ref: &'a str,
    pub ls_key: &'a str,
    pub call_index: u64,
    pub mcp_session_key: &'a str,
    pub mcp_trace: PlasmTraceContext,
    pub plan_trace: PlanRunTraceHooks,
    pub mcp_result_policy: crate::mcp_run_markdown::McpResultTransportPolicy,
}

pub(crate) async fn execute_plasm_tool_dry_run(
    ctx: PlasmDryRunContext<'_>,
    program: &str,
) -> Result<PlasmPlanRunResult, String> {
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
    let gate_decision = plan_gate(
        &flow_gate,
        PlanGateContext {
            force: false,
            plan_commit_ref: None,
        },
    );
    if let crate::PlanGateDecision::Denied(denial) = &gate_decision {
        return Err(format!(
            "plan denied by flow policy ({:?}): {} violation(s)",
            denial.verdict,
            denial.violations.len()
        ));
    }
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

    let auto_execute = matches!(gate_decision, crate::PlanGateDecision::Proceed(_))
        && matches!(dry.flow.verdict, crate::plan_flow::FlowVerdict::Clean)
        && !crate::plan_flow::validated_plan_has_remote_mutation(dry.validated_plan());
    if auto_execute {
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
        .emit_evaluate(Some(plan_ux_reflection), inline_fits)
        .await;
        let _ = plan_refs;
        record_mcp_plasm_dry_run_phase("trace_emit", phase.elapsed());
        record_mcp_plasm_dry_run_phase("total", total_started.elapsed());

        return super::committed_plasm_run::execute_mcp_live_run(
            super::committed_plasm_run::ExecuteMcpLiveRun {
                es: Arc::clone(&ctx.es),
                host: Arc::clone(&ctx.host),
                wire: super::committed_plasm_run::McpExecuteWire {
                    prompt_hash: ctx.binding.prompt_hash.clone(),
                    session_id: ctx.binding.session_id.clone(),
                    session_ref: ctx.session_ref.to_string(),
                    ls_key: ctx.ls_key.to_string(),
                    mcp_session_key: ctx.mcp_session_key.to_string(),
                },
                bundle,
                kind: super::committed_plasm_run::McpLiveRunKind::FusedCleanRead {
                    dry: Box::new(dry),
                    verdict: compact.verdict,
                },
                mcp_trace: ctx.mcp_trace,
                artifacts: super::committed_plasm_run::CommittedRunArtifacts {
                    trace_hub: Arc::clone(&ctx.host.trace_hub),
                    run_artifacts: Arc::clone(&ctx.host.run_artifacts),
                    program_for_trace,
                    plan_call_index: ctx.call_index,
                },
                plan_trace: Some(ctx.plan_trace),
                mcp_result_policy: Some(ctx.mcp_result_policy),
                force_run: false,
                wait_live: true,
            },
        )
        .await;
    }

    let commit_ref = ctx.es.mint_plan_commit_ref();
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

    phase = Instant::now();
    crate::plan_commit_store::register_plan_commit_with_persist(
        ctx.host.as_ref(),
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
        crate::symbol_map_resolve::symbol_map_fingerprint_for_session(ctx.es.as_ref()).as_deref(),
        projection_warning,
    );
    let dry_verdict = agent_plasm
        .get("dry_verdict")
        .and_then(|v| v.as_str())
        .unwrap_or("ok");
    let markdown = crate::mcp_agent_present::AgentContent::plan(
        &crate::mcp_agent_present::PlanTokenRefs {
            run_ref: commit_ref.as_str(),
            dry_verdict,
            logical_session_ref: ctx.session_ref,
            plan_uri: Some(plan_refs.canonical_plan_uri.as_str()),
        },
        &dry_text,
    )
    .render();
    let inline_plan_ui = inline_fits.then(|| UiInlinePlanPayload {
        comp: comp_val.expect("inline_fits implies comp_val"),
        plan_ux_reflection: plan_ux_reflection.clone(),
    });
    let mut meta = serde_json::Map::new();
    meta.insert("plasm".into(), serde_json::Value::Object(agent_plasm));
    record_mcp_plasm_dry_run_phase("build_response", phase.elapsed());
    record_mcp_plasm_dry_run_phase("total", total_started.elapsed());

    Ok(PlasmPlanRunResult {
        version: dry.version,
        node_results: dry.node_results,
        graph_summary: dry.graph_summary,
        comp: Some(comp_wire.as_ref().clone()),
        code_plan_run_artifacts: Vec::new(),
        run_markdown: Some(markdown),
        run_plasm_meta: Some(meta),
        return_steps: Vec::new(),
        inline_plan_ui,
    })
}
