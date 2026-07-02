//! Live MCP `plasm_run` — reviewed commits (`pcN`) and paging continuations via unified `run_ref`.

use std::sync::Arc;

use plasm_core::{PagingHandle, PlanCommitRef, PromptPipelineConfig, SymbolMapCrossRequestCache};

use crate::execute_session::ExecuteSession;
use crate::plan_commit_store::{dry_for_committed_plasm_run, CommittedPlan};
use crate::plan_dry_display::{build_plan_dry_compact_view, PlanDryVerdict};
use crate::plan_gate::{plan_requires_review_gate, PlanGateContext};
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_compile::compile_plasm_expression;
use crate::plasm_plan_run::{evaluate_plasm_comp_dry, DryPlasmPlanEvaluation, PlasmPlanRunResult};
use crate::run_artifacts::RunArtifactStore;
use crate::run_delivery::{
    deliver_live_run_await, LiveRunAwaitContext, LiveRunError, LiveRunSpawnOpts,
};
use crate::run_explorer_meta::build_run_explorer_accept_payload;
use crate::server_state::PlasmHostState;
use crate::trace_hub::TraceHub;
use crate::trace_sink_emit::PlasmTraceContext;

use plasm_trace::TraceCompWire;

use super::mcp_plasm_invoke::McpPlasmRunTarget;
use super::trace::CodePlanTraceInput;

/// MCP execute-row wire + logical session identity.
#[derive(Clone)]
pub struct McpExecuteWire {
    pub prompt_hash: String,
    pub session_id: String,
    pub session_ref: String,
    pub ls_key: String,
    pub mcp_session_key: String,
}

/// Trace + artifact persistence inputs for a live MCP run.
#[derive(Clone)]
pub struct CommittedRunArtifacts {
    pub trace_hub: Arc<TraceHub>,
    pub run_artifacts: Arc<RunArtifactStore>,
    pub program_for_trace: String,
    pub plan_call_index: u64,
}

/// Which live `plasm_run` path to execute.
#[derive(Clone)]
pub enum McpLiveRunKind {
    ReviewedCommit {
        committed: Box<CommittedPlan>,
        plan_commit_ref: PlanCommitRef,
    },
    /// Continuation of an already-reviewed list read — review gate skipped; evidence uses trace anchors only.
    PageContinuation {
        #[allow(dead_code)] // ingress marker: pairs continuation bundle with resolved handle
        page_handle: PagingHandle,
    },
}

/// Resolved bundle + live-run kind for MCP `plasm_run` (commit or paging continuation).
pub struct ResolvedMcpLiveRunIngress {
    pub bundle: PlasmCompBundle,
    pub program_for_trace: String,
    pub kind: McpLiveRunKind,
}

/// Resolve MCP `run_ref` (`pcN` or page handle) into a compile bundle and [`McpLiveRunKind`].
pub async fn resolve_mcp_live_run_ingress(
    es: &ExecuteSession,
    mcp_trace: &PlasmTraceContext,
    run_target: &McpPlasmRunTarget,
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: &SymbolMapCrossRequestCache,
    call_index: u64,
) -> Result<ResolvedMcpLiveRunIngress, String> {
    match run_target {
        McpPlasmRunTarget::Page(handle) => {
            crate::http_execute::resolve_paging_storage_handle(Some(mcp_trace), handle)
                .map_err(crate::execute_pipeline::display_run_line_error)?;
            let program = format!("page({handle})");
            let plan_name = format!("plasm_page_call_{call_index}");
            let bundle = compile_plasm_expression(
                pipeline,
                Some(symbol_map_cross_cache),
                es,
                &plan_name,
                &program,
            )?;
            Ok(ResolvedMcpLiveRunIngress {
                bundle,
                program_for_trace: program,
                kind: McpLiveRunKind::PageContinuation {
                    page_handle: handle.clone(),
                },
            })
        }
        McpPlasmRunTarget::Commit(pc) => {
            let committed =
                crate::mcp_plasm_run_phases::mcp_plasm_run_phase("resolve_commit", || async {
                    crate::plan_commit_store::resolve_committed_plan(es, pc).map_err(|e| e.detail())
                })
                .await?;
            Ok(ResolvedMcpLiveRunIngress {
                bundle: PlasmCompBundle::new(committed.artifact.clone())?,
                program_for_trace: committed.program.clone(),
                kind: McpLiveRunKind::ReviewedCommit {
                    committed: Box::new(committed),
                    plan_commit_ref: pc.clone(),
                },
            })
        }
    }
}

/// Ingress for MCP `plasm_run` live execute.
#[derive(Clone)]
pub struct ExecuteMcpLiveRun {
    pub es: Arc<ExecuteSession>,
    pub host: Arc<PlasmHostState>,
    pub wire: McpExecuteWire,
    pub bundle: PlasmCompBundle,
    pub kind: McpLiveRunKind,
    pub mcp_trace: PlasmTraceContext,
    pub artifacts: CommittedRunArtifacts,
    pub plan_trace: Option<crate::trace_hub::PlanRunTraceHooks>,
    pub mcp_result_policy: Option<crate::mcp_run_markdown::McpResultTransportPolicy>,
    pub force_run: bool,
    pub wait_live: bool,
}

impl ExecuteMcpLiveRun {
    fn code_plan_trace_input(&self, comp: Arc<TraceCompWire>) -> CodePlanTraceInput<'_> {
        CodePlanTraceInput {
            hub: self.artifacts.trace_hub.as_ref(),
            store: self.artifacts.run_artifacts.as_ref(),
            mcp_key: self.wire.ls_key.as_str(),
            es: self.es.as_ref(),
            prompt_hash: self.wire.prompt_hash.as_str(),
            session_id: self.wire.session_id.as_str(),
            session_ref: self.wire.session_ref.as_str(),
            comp,
            program: self.artifacts.program_for_trace.as_str(),
            plan_call_index: self.artifacts.plan_call_index,
            code_chars: self.artifacts.program_for_trace.chars().count() as u64,
        }
    }
}

struct LiveDryOutcome {
    dry: DryPlasmPlanEvaluation,
    verdict: PlanDryVerdict,
    plan_commit_ref: Option<PlanCommitRef>,
}

fn prepare_live_dry(run: &ExecuteMcpLiveRun) -> Result<LiveDryOutcome, String> {
    match &run.kind {
        McpLiveRunKind::ReviewedCommit {
            committed,
            plan_commit_ref,
        } => {
            let dry =
                dry_for_committed_plasm_run(run.es.as_ref(), &run.bundle, committed.as_ref())?;
            let gate = dry.evaluate_gate();
            if plan_requires_review_gate(
                &gate,
                PlanGateContext {
                    force: run.force_run,
                    plan_commit_ref: Some(plan_commit_ref),
                },
            ) {
                return Err(
                    "plan_requires_review: call `plasm` dry-run first, then pass the returned `run_ref` (`pcN`) to `plasm_run`"
                        .to_string(),
                );
            }
            Ok(LiveDryOutcome {
                dry,
                verdict: committed.verdict,
                plan_commit_ref: Some(plan_commit_ref.clone()),
            })
        }
        McpLiveRunKind::PageContinuation { .. } => {
            let dry = evaluate_plasm_comp_dry(run.es.as_ref(), &run.bundle)?;
            let compact = build_plan_dry_compact_view(
                dry.validated_plan(),
                &dry.topological_order,
                &dry.review,
                &dry.graph_summary,
                Some(run.es.as_ref()),
                None,
            );
            Ok(LiveDryOutcome {
                dry,
                verdict: compact.verdict,
                plan_commit_ref: None,
            })
        }
    }
}

pub async fn execute_mcp_live_run(run: ExecuteMcpLiveRun) -> Result<PlasmPlanRunResult, String> {
    if !run.wait_live {
        return Err("plasm_run requires live execute".to_string());
    }

    let live = prepare_live_dry(&run)?;
    let comp_wire = Arc::new(crate::plasm_comp_wire::trace_comp_wire_from_dry(&live.dry));
    let plan_ux_reflection = Some(crate::plan_ux_reflection::plan_ux_reflection_value(
        &live.dry,
        &crate::plan_ux_reflection::PlanUxBuildContext {
            session: Some(run.es.as_ref()),
            param_bindings: &[],
        },
    ));
    let trace_input = run.code_plan_trace_input(Arc::clone(&comp_wire));
    let execute_plan_id = trace_input.emit_execute_started().await;

    let await_out =
        match crate::mcp_plasm_run_phases::mcp_plasm_run_phase("async_live_await", || async {
            let accept_payload =
                build_run_explorer_accept_payload(&live.dry, Some(run.es.as_ref()));
            deliver_live_run_await(
                LiveRunAwaitContext::for_mcp_plasm_run(
                    Arc::clone(&run.es),
                    Arc::clone(&run.host),
                    run.wire.prompt_hash.clone(),
                    run.wire.session_id.clone(),
                    run.wire.session_ref.clone(),
                    run.wire.mcp_session_key.clone(),
                    run.bundle.clone(),
                    accept_payload,
                    live.verdict,
                    live.plan_commit_ref.clone(),
                    run.mcp_trace.clone(),
                    live.dry,
                ),
                LiveRunSpawnOpts {
                    plan_trace: run.plan_trace.clone(),
                    mcp_result_policy: run.mcp_result_policy,
                },
            )
            .await
            .map_err(|e| match e {
                LiveRunError::Timeout(d) => format!("live run timed out after {d:?}"),
                LiveRunError::Failed(msg) => msg,
            })
        })
        .await
        {
            Ok(result) => result,
            Err(err) => {
                run.code_plan_trace_input(Arc::clone(&comp_wire))
                    .emit_execute_failed(execute_plan_id)
                    .await;
                return Err(err);
            }
        };

    crate::mcp_plasm_run_phases::mcp_plasm_run_phase("artifact_persist", || async {
        run.code_plan_trace_input(Arc::clone(&comp_wire))
            .emit_execute_completed(Some(execute_plan_id), plan_ux_reflection, &await_out)
            .await;
        Ok(await_out)
    })
    .await
}
