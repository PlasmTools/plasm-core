//! Live MCP `plasm_run` — reviewed commits (`pcN`) and paging continuations via unified `run_ref`.

use std::sync::Arc;
use tracing::Instrument;

use plasm_core::{PagingHandle, PlanCommitRef, PromptPipelineConfig, SymbolMapCrossRequestCache};

use crate::execute_session::ExecuteSession;
use crate::plan_commit_store::{dry_for_committed_plasm_run, CommittedPlan, PlanCommitVerifyError};
use crate::plan_dry_display::{build_plan_dry_compact_view, PlanDryVerdict};
use crate::plan_gate::{plan_requires_review_gate, PlanGateContext};
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_plan_run::{DryPlasmPlanEvaluation, PlasmPlanRunResult};
use crate::run_artifacts::RunArtifactStore;
use crate::run_delivery::{
    deliver_live_run_await, LiveRunAwaitContext, LiveRunError, LiveRunSpawnOpts,
};
use crate::run_explorer_meta::build_run_explorer_accept_payload;
use crate::server_state::PlasmHostState;
use crate::trace_hub::TraceHub;
use crate::trace_sink_emit::PlasmTraceContext;

use plasm_runtime::{ExecutionFailure, FailureCause};
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
    /// MCP `plasm` fused clean read: dry already proven; skip commit persist and re-dry.
    FusedCleanRead {
        dry: Box<DryPlasmPlanEvaluation>,
        verdict: PlanDryVerdict,
    },
}

/// Resolved bundle + live-run kind for MCP `plasm_run` (commit or paging continuation).
pub struct ResolvedMcpLiveRunIngress {
    pub bundle: PlasmCompBundle,
    pub program_for_trace: String,
    pub kind: McpLiveRunKind,
}

/// Host continuation is typed IR, never source text submitted to a language parser.
pub fn compile_page_continuation(
    session: &ExecuteSession,
    handle: &PagingHandle,
    call_index: u64,
) -> Result<PlasmCompBundle, ExecutionFailure> {
    use plasm_core::plasm_monad::*;
    let owner = session.paging_qualified_entity(handle).ok_or_else(|| {
        ExecutionFailure::from(crate::http_execute::PagingHandleFault::Unavailable {
            handle: handle.clone(),
        })
    })?;
    let build_failure = |detail: plasm_core::plasm_monad::StepIdError| {
        ExecutionFailure::new(FailureCause::Program, "step_id_invalid", detail.to_string())
    };
    let id = StepId::new("page").map_err(build_failure)?;
    let mut comp = empty_comp(Some(format!("plasm_page_call_{call_index}")));
    let payload = PlasmStepPayload::Invoke(InvokePayload {
        plan_kind: SurfaceKind::Query,
        qualified_entity: Some(PlanQualifiedEntityKey {
            entry_id: owner.entry_id,
            entity: owner.entity,
        }),
        ir: Some(PlanExprIr {
            expr: plasm_core::Expr::Page(plasm_core::expr::PageExpr {
                handle: handle.clone(),
                limit: None,
            }),
            projection: None,
            display_expr: None,
        }),
        ir_template: None,
        projection: vec![],
        predicates: vec![],
        page_size: None,
        approval: None,
        display_expr: None,
        effect_class: EffectClass::Read,
        result_shape: ResultShape::Page,
    });
    plasm_pure_step(&mut comp, id.clone(), payload, "result continuation").map_err(|error| {
        ExecutionFailure::new(
            FailureCause::Runtime,
            "page_plan_construction",
            error.to_string(),
        )
    })?;
    comp.return_ = PlasmReturn::Step { step: id };
    comp.validate().map_err(|error| {
        ExecutionFailure::new(
            FailureCause::Program,
            "committed_plan_invalid",
            error.to_string(),
        )
    })?;
    PlasmCompBundle::new(PlasmCompArtifact {
        comp,
        approval_gates: vec![],
    })
    .map_err(|error| {
        ExecutionFailure::new(
            FailureCause::Program,
            "committed_plan_invalid",
            error.to_string(),
        )
    })
}

/// Resolve MCP `run_ref` (`pcN` or page handle) into a compile bundle and [`McpLiveRunKind`].
pub async fn resolve_mcp_live_run_ingress(
    es: &ExecuteSession,
    mcp_trace: &PlasmTraceContext,
    run_target: &McpPlasmRunTarget,
    _pipeline: &PromptPipelineConfig,
    _symbol_map_cross_cache: &SymbolMapCrossRequestCache,
    call_index: u64,
) -> Result<ResolvedMcpLiveRunIngress, ExecutionFailure> {
    match run_target {
        McpPlasmRunTarget::Page(handle) => {
            crate::http_execute::resolve_paging_storage_handle(Some(mcp_trace), handle)
                .map_err(ExecutionFailure::from)?;
            let program = format!("Continue result page {handle}");
            let bundle = compile_page_continuation(es, handle, call_index)?;
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
                    crate::plan_commit_store::resolve_committed_plan(es, pc)
                })
                .await
                .map_err(commit_verify_failure)?;
            Ok(ResolvedMcpLiveRunIngress {
                bundle: PlasmCompBundle::new(committed.artifact.clone()).map_err(|detail| {
                    ExecutionFailure::new(
                        FailureCause::Runtime,
                        "committed_plan_invalid",
                        detail.to_string(),
                    )
                })?,
                program_for_trace: committed.program.clone(),
                kind: McpLiveRunKind::ReviewedCommit {
                    committed: Box::new(committed),
                    plan_commit_ref: pc.clone(),
                },
            })
        }
    }
}

fn commit_verify_failure(error: PlanCommitVerifyError) -> ExecutionFailure {
    let (cause, code) = match &error {
        PlanCommitVerifyError::Unknown { .. } => (FailureCause::Program, "plan_commit_unknown"),
        PlanCommitVerifyError::Expired { .. } => (FailureCause::Program, "plan_commit_expired"),
        PlanCommitVerifyError::Mismatch { .. } => (FailureCause::Program, "plan_commit_mismatch"),
        PlanCommitVerifyError::PlanAheadOfSession { .. } => {
            (FailureCause::Program, "plan_commit_ahead_of_session")
        }
        PlanCommitVerifyError::StalePolicy { .. } => {
            (FailureCause::Program, "plan_commit_stale_policy")
        }
        PlanCommitVerifyError::Evidence { .. } => {
            (FailureCause::Runtime, "plan_commit_evidence_mismatch")
        }
    };
    ExecutionFailure::new(cause, code, error.to_string())
}

/// Ingress for MCP `plasm_run` live execute.
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

fn code_plan_trace_input<'a>(
    artifacts: &'a CommittedRunArtifacts,
    es: &'a ExecuteSession,
    wire: &'a McpExecuteWire,
    comp: Arc<TraceCompWire>,
) -> CodePlanTraceInput<'a> {
    CodePlanTraceInput {
        hub: artifacts.trace_hub.as_ref(),
        store: Arc::clone(&artifacts.run_artifacts),
        mcp_key: wire.ls_key.as_str(),
        es,
        prompt_hash: wire.prompt_hash.as_str(),
        session_id: wire.session_id.as_str(),
        comp,
        program: artifacts.program_for_trace.as_str(),
        plan_call_index: artifacts.plan_call_index,
        code_chars: artifacts.program_for_trace.chars().count() as u64,
    }
}

struct LiveDryOutcome {
    dry: DryPlasmPlanEvaluation,
    verdict: PlanDryVerdict,
    plan_commit_ref: Option<PlanCommitRef>,
}

async fn prepare_live_dry(
    kind: McpLiveRunKind,
    es: &ExecuteSession,
    bundle: &PlasmCompBundle,
    force_run: bool,
) -> Result<LiveDryOutcome, plasm_runtime::ExecutionFailure> {
    match kind {
        McpLiveRunKind::ReviewedCommit {
            committed,
            plan_commit_ref,
        } => {
            let dry = dry_for_committed_plasm_run(
                &es.preflight_snapshot().await,
                bundle,
                committed.as_ref(),
            )
            .map_err(|error| {
                plasm_runtime::ExecutionFailure::new(
                    plasm_runtime::FailureCause::Program,
                    "committed_plan_dry_evaluation_failed",
                    error.detail(),
                )
            })?;
            let gate = dry.evaluate_gate();
            if plan_requires_review_gate(
                &gate,
                PlanGateContext {
                    force: force_run,
                    plan_commit_ref: Some(&plan_commit_ref),
                },
            ) {
                return Err(plasm_runtime::ExecutionFailure::new(
                    plasm_runtime::FailureCause::Program,
                    "plan_requires_review",
                    "plan requires review: run `plasm` dry-run first, then pass its `run_ref` to `plasm_run`",
                ));
            }
            Ok(LiveDryOutcome {
                dry,
                verdict: committed.verdict,
                plan_commit_ref: Some(plan_commit_ref),
            })
        }
        McpLiveRunKind::PageContinuation { .. } => {
            let dry = crate::plasm_plan_run::evaluate_plasm_comp_dry_snapshot(es, bundle)
                .await
                .map_err(|diagnostic| {
                    plasm_runtime::ExecutionFailure::new(
                        plasm_runtime::FailureCause::Program,
                        "plan_dry_evaluation_failed",
                        diagnostic,
                    )
                })?;
            let compact = build_plan_dry_compact_view(
                dry.validated_plan(),
                &dry.topological_order,
                &dry.review,
                &dry.graph_summary,
                Some(es),
                None,
            );
            Ok(LiveDryOutcome {
                dry,
                verdict: compact.verdict,
                plan_commit_ref: None,
            })
        }
        McpLiveRunKind::FusedCleanRead { dry, verdict } => Ok(LiveDryOutcome {
            dry: *dry,
            verdict,
            plan_commit_ref: None,
        }),
    }
}

pub async fn execute_mcp_live_run(
    run: ExecuteMcpLiveRun,
) -> Result<PlasmPlanRunResult, plasm_runtime::ExecutionFailure> {
    execute_mcp_live_run_inner(run)
        .instrument(crate::spans::plan_live_run())
        .await
}

async fn execute_mcp_live_run_inner(
    run: ExecuteMcpLiveRun,
) -> Result<PlasmPlanRunResult, plasm_runtime::ExecutionFailure> {
    if !run.wait_live {
        return Err(plasm_runtime::ExecutionFailure::new(
            plasm_runtime::FailureCause::Program,
            "plasm_run_requires_live_execution",
            "plasm_run requires live execution",
        ));
    }

    let ExecuteMcpLiveRun {
        es,
        host,
        wire,
        bundle,
        kind,
        mcp_trace,
        artifacts,
        plan_trace,
        mcp_result_policy,
        force_run,
        wait_live: _,
    } = run;
    let live = prepare_live_dry(kind, es.as_ref(), &bundle, force_run).await?;
    let comp_wire = Arc::new(crate::plasm_comp_wire::trace_comp_wire_from_dry(&live.dry));
    let plan_ux_reflection = Some(crate::plan_ux_reflection::plan_ux_reflection_value(
        &live.dry,
        &crate::plan_ux_reflection::PlanUxBuildContext {
            session: Some(es.as_ref()),
            param_bindings: &[],
        },
    ));
    let execute_plan_id =
        code_plan_trace_input(&artifacts, es.as_ref(), &wire, Arc::clone(&comp_wire))
            .emit_execute_started()
            .await;

    let await_out =
        match crate::mcp_plasm_run_phases::mcp_plasm_run_phase("async_live_await", || async {
            let accept_payload = build_run_explorer_accept_payload(&live.dry, Some(es.as_ref()));
            deliver_live_run_await(
                LiveRunAwaitContext::for_mcp_plasm_run(
                    Arc::clone(&es),
                    Arc::clone(&host),
                    wire.prompt_hash.clone(),
                    wire.session_id.clone(),
                    wire.session_ref.clone(),
                    wire.mcp_session_key.clone(),
                    bundle.clone(),
                    accept_payload,
                    live.verdict,
                    live.plan_commit_ref.clone(),
                    mcp_trace.clone(),
                    live.dry,
                ),
                LiveRunSpawnOpts {
                    plan_trace: plan_trace.clone(),
                    mcp_result_policy,
                },
            )
            .await
            .map_err(|e| match e {
                LiveRunError::Timeout(d) => plasm_runtime::ExecutionFailure::new(
                    plasm_runtime::FailureCause::Runtime,
                    "live_run_timeout",
                    format!("live run timed out after {d:?}"),
                ),
                LiveRunError::Failed(failure) => failure,
            })
        })
        .await
        {
            Ok(result) => result,
            Err(err) => {
                code_plan_trace_input(&artifacts, es.as_ref(), &wire, Arc::clone(&comp_wire))
                    .emit_execute_failed(execute_plan_id)
                    .await;
                return Err(err);
            }
        };

    crate::mcp_plasm_run_phases::mcp_plasm_run_phase("artifact_persist", || async {
        code_plan_trace_input(&artifacts, es.as_ref(), &wire, Arc::clone(&comp_wire))
            .emit_execute_completed(Some(execute_plan_id), plan_ux_reflection, &await_out)
            .await;
        Ok(await_out)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use plasm_core::{CgsContext, CGS};
    use plasm_runtime::{FailureCause, RecoveryDisposition};

    #[test]
    fn unavailable_page_handle_is_a_repairable_read_only_error() {
        let cgs = Arc::new(CGS::new());
        let mut contexts = IndexMap::new();
        contexts.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", Arc::clone(&cgs))),
        );
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            Arc::clone(&cgs),
            contexts,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
        );
        let handle = PagingHandle::parse("pg1").unwrap();
        let failure = compile_page_continuation(&session, &handle, 1).unwrap_err();
        assert_eq!(failure.cause, FailureCause::Program);
        assert_eq!(failure.code, "page_handle_unavailable");
        assert_eq!(failure.recovery, RecoveryDisposition::RepairProgram);
        assert!(!failure.effects_unresolved);
        assert!(failure.effects.is_empty());
        assert!(failure.dispatches.is_empty());
    }
}
