//! Live MCP execute for a reviewed [`CommittedPlan`] (`plasm_run` + `pcN`).

use std::sync::Arc;

use plasm_core::PlanCommitRef;
use plasm_runtime::{with_live_run_telemetry, CancelSignal};

use crate::execute_pipeline::{ExecutePipeline, ExecutionIntent};
use crate::execute_session::ExecuteSession;
use crate::mcp_plasm_meta::PlasmMetaIndex;
use crate::mcp_run_config::bounded_sync_run_deadline;
use crate::operation::{op_accept_context_from_executable, spawn_op_progress_ticker, ExecutionScope};
use crate::plan_commit_store::CommittedPlan;
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_plan_run::{evaluate_plasm_comp_dry, PlasmPlanRunHooks, PlasmPlanRunResult};
use crate::run_artifacts::RunArtifactStore;
use crate::server_state::PlasmHostState;
use crate::trace_hub::{McpPlasmTraceSink, TraceHub};
use crate::trace_sink_emit::PlasmTraceContext;

use super::trace::trace_archive_and_emit_code_plan_execute;

pub struct CommittedPlasmRunContext<'a> {
    pub es: Arc<ExecuteSession>,
    pub host: Arc<PlasmHostState>,
    pub prompt_hash: String,
    pub session_id: String,
    pub session_ref: String,
    pub ls_key: String,
    pub mcp_session_key: String,
    pub plan_commit_ref: Option<PlanCommitRef>,
    pub committed: CommittedPlan,
    pub bundle: PlasmCompBundle,
    pub program_for_trace: String,
    pub comp_archive: serde_json::Value,
    pub mcp_trace: PlasmTraceContext,
    pub call_count: u64,
    pub force_run: bool,
    pub wait_live: bool,
    pub idx: &'a mut PlasmMetaIndex,
    pub sink: McpPlasmTraceSink,
    pub trace_hub: Arc<TraceHub>,
    pub run_artifacts: Arc<RunArtifactStore>,
}

pub async fn execute_committed_plasm_run(
    ctx: CommittedPlasmRunContext<'_>,
) -> Result<PlasmPlanRunResult, String> {
    crate::mcp_plasm_run_phases::mcp_plasm_run_phase("evidence_begin", || async {
        crate::evidence_chain::begin_plan_evidence_with_anchors(
            ctx.es.as_ref(),
            ctx.session_id.as_str(),
            crate::evidence_chain::evidence_anchors(
                ctx.plan_commit_ref.as_ref(),
                Some(ctx.mcp_trace.trace_id),
                Some(ctx.call_count),
            ),
        )
        .map_err(|e| format!("evidence begin: {e}"))
    })
    .await?;

    if crate::operation::plan_requires_review_gate(
        ctx.committed.verdict,
        ctx.force_run,
        ctx.plan_commit_ref.as_ref(),
    ) {
        return Err(
            "plan_requires_review: call `plasm` dry-run first, then pass the returned `plan_commit_ref` (`pcN`) to `plasm_run`"
                .to_string(),
        );
    }

    if crate::run_delivery::should_spawn_async_for_policy(
        crate::run_delivery::RunDeliveryPolicy::McpAwaitTerminal,
        ctx.wait_live,
        &ctx.committed.dry_review,
    ) {
        let awaited =
            crate::mcp_plasm_run_phases::mcp_plasm_run_phase("async_dry_eval", || async {
                let dry_gate = evaluate_plasm_comp_dry(ctx.es.as_ref(), &ctx.bundle)?;
                let accept_payload = crate::run_explorer_meta::build_run_explorer_accept_payload(
                    &dry_gate,
                    Some(ctx.es.as_ref()),
                );
                crate::run_delivery::deliver_mcp_expensive_live_run(
                    crate::run_delivery::McpExpensiveLiveRunContext {
                        es: Arc::clone(&ctx.es),
                        st: Arc::clone(&ctx.host),
                        prompt_hash: ctx.prompt_hash.clone(),
                        session_id: ctx.session_id.clone(),
                        session_ref: ctx.session_ref.clone(),
                        mcp_session_key: ctx.mcp_session_key.clone(),
                        bundle: ctx.bundle.clone(),
                        review: ctx.committed.dry_review.clone(),
                        accept_payload,
                        dry_verdict: ctx.committed.verdict,
                        plan_commit_ref: ctx.plan_commit_ref.clone(),
                        trace: ctx.mcp_trace.clone(),
                        wait_live: ctx.wait_live,
                        await_cfg: crate::mcp_run_await::AwaitConfig::default(),
                    },
                )
                .await
                .map_err(|e| e.to_string())
            })
            .await?;
        if let Some(result) = awaited {
            return Ok(result);
        }
    }

    let deadline = bounded_sync_run_deadline();
    let sync_result = crate::mcp_plasm_run_phases::mcp_plasm_run_phase("http_execute", || async {
        let telemetry = ctx.es.begin_sync_live_run()?;
        let handle = ctx.es.mint_operation_handle(ctx.session_ref.as_str());
        let dry_gate = evaluate_plasm_comp_dry(ctx.es.as_ref(), &ctx.bundle)?;
        let accept_payload = crate::run_explorer_meta::build_run_explorer_accept_payload(
            &dry_gate,
            Some(ctx.es.as_ref()),
        );
        let mut accept = op_accept_context_from_executable(
            ctx.plan_commit_ref.clone(),
            Some(ctx.committed.verdict),
            false,
            Some(ctx.mcp_session_key.clone()),
            ctx.bundle.executable(),
            &ctx.bundle.artifact().comp,
        );
        accept.comp = Some(accept_payload.comp);
        accept.plan_ux_reflection = Some(accept_payload.plan_ux_reflection);
        accept.step_order = accept_payload.step_order;
        accept.host = Some(Arc::downgrade(&ctx.host));
        ctx.es.bind_operation_wire(ctx.session_id.as_str());
        if let Err(e) = ctx.es.try_begin_async_operation(
            handle.clone(),
            CancelSignal::new(),
            accept,
        ) {
            ctx.es.end_sync_live_run();
            return Err(e);
        }
        if let Err(e) = ctx.es.emit_op_accept(&handle, ctx.host.as_ref()) {
            ctx.es.end_sync_live_run();
            return Err(e);
        }
        let scope = ExecutionScope::for_async_operation(
            Arc::clone(&ctx.es),
            handle.clone(),
            CancelSignal::new(),
        );
        let ticker_cancel = tokio_util::sync::CancellationToken::new();
        let ticker = spawn_op_progress_ticker(
            Arc::clone(&ctx.es),
            handle.clone(),
            Arc::clone(&ctx.host),
            ticker_cancel.clone(),
        );
        let sync_future = with_live_run_telemetry(telemetry, async {
            ExecutePipeline::run_program(
                ctx.es.as_ref(),
                ctx.host.as_ref(),
                ctx.prompt_hash.as_str(),
                ctx.session_id.as_str(),
                &ctx.bundle,
                ExecutionIntent::Live,
                Some(PlasmPlanRunHooks {
                    meta_index: ctx.idx,
                    trace: ctx.mcp_trace.clone(),
                    sink: ctx.sink.clone(),
                }),
                Some(&scope),
                None,
            )
            .await
        });
        let result = if ctx.committed.dry_review.execution_is_expensive() {
            sync_future.await
        } else {
            match tokio::time::timeout(deadline, sync_future).await {
                Ok(result) => result,
                Err(_) => Err(format!(
                    "sync plasm_run deadline exceeded ({:.0}s); outbound HTTP or auth may be stalled",
                    deadline.as_secs_f64()
                )),
            }
        };
        ticker_cancel.cancel();
        let _ = ticker.await;
        let final_result = match result {
            Ok(out) => {
                let run_artifact_id = out
                    .code_plan_run_artifacts
                    .first()
                    .map(|a| a.run_id.clone());
                let returned = out.clone();
                ctx.es.finalize_operation_succeeded_with_artifact(
                    &handle,
                    out,
                    run_artifact_id,
                    Some(ctx.host.as_ref()),
                );
                Ok(returned)
            }
            Err(e) => {
                ctx.es.finalize_operation_failed(&handle, e.clone(), Some(ctx.host.as_ref()));
                Err(e)
            }
        };
        ctx.es.end_sync_live_run();
        final_result
    })
    .await?;

    crate::mcp_plasm_run_phases::mcp_plasm_run_phase("artifact_persist", || async {
        trace_archive_and_emit_code_plan_execute(
            ctx.trace_hub.as_ref(),
            ctx.run_artifacts.as_ref(),
            ctx.ls_key.as_str(),
            ctx.es.as_ref(),
            ctx.prompt_hash.as_str(),
            ctx.session_id.as_str(),
            ctx.session_ref.as_str(),
            &ctx.comp_archive,
            ctx.program_for_trace.as_str(),
            sync_result.comp.clone(),
            ctx.call_count,
            &sync_result,
        )
        .await;
        Ok(sync_result)
    })
    .await
}
