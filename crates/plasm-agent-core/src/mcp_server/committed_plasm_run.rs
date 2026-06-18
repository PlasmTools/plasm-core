//! Live MCP execute for a reviewed [`CommittedPlan`] (`plasm_run` + `pcN`).

use std::sync::Arc;

use plasm_core::PlanCommitRef;

use crate::execute_session::ExecuteSession;
use crate::plan_commit_store::{dry_for_committed_plasm_run, CommittedPlan};
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_plan_run::PlasmPlanRunResult;
use crate::run_artifacts::RunArtifactStore;
use crate::run_delivery::{
    deliver_live_run_await, LiveRunAwaitContext, LiveRunError, LiveRunSpawnOpts,
};
use crate::run_explorer_meta::build_run_explorer_accept_payload;
use crate::server_state::PlasmHostState;
use crate::trace_hub::TraceHub;
use crate::trace_sink_emit::PlasmTraceContext;

use super::trace::trace_archive_and_emit_code_plan_execute;

/// MCP execute-row wire + logical session identity.
#[derive(Clone)]
pub struct McpExecuteWire {
    pub prompt_hash: String,
    pub session_id: String,
    pub session_ref: String,
    pub ls_key: String,
    pub mcp_session_key: String,
}

/// Trace + artifact persistence inputs for a committed live run.
#[derive(Clone)]
pub struct CommittedRunArtifacts {
    pub trace_hub: Arc<TraceHub>,
    pub run_artifacts: Arc<RunArtifactStore>,
    pub comp_archive: serde_json::Value,
    pub program_for_trace: String,
    pub plan_call_index: u64,
}

/// Ingress for MCP `plasm_run` with a reviewed plan commit.
#[derive(Clone)]
pub struct ExecuteCommittedMcpRun {
    pub es: Arc<ExecuteSession>,
    pub host: Arc<PlasmHostState>,
    pub wire: McpExecuteWire,
    pub plan_commit_ref: Option<PlanCommitRef>,
    pub committed: CommittedPlan,
    pub bundle: PlasmCompBundle,
    pub mcp_trace: PlasmTraceContext,
    pub artifacts: CommittedRunArtifacts,
    pub plan_trace: Option<crate::trace_hub::PlanRunTraceHooks>,
    pub force_run: bool,
    pub wait_live: bool,
}

pub async fn execute_committed_plasm_run(
    run: ExecuteCommittedMcpRun,
) -> Result<PlasmPlanRunResult, String> {
    crate::mcp_plasm_run_phases::mcp_plasm_run_phase("evidence_begin", || async {
        crate::evidence_chain::begin_plan_evidence_with_anchors(
            run.es.as_ref(),
            run.wire.session_id.as_str(),
            crate::evidence_chain::evidence_anchors(
                run.plan_commit_ref.as_ref(),
                Some(run.mcp_trace.trace_id),
                Some(run.artifacts.plan_call_index),
            ),
        )
        .map_err(|e| format!("evidence begin: {e}"))
    })
    .await?;

    if crate::operation::plan_requires_review_gate(
        run.committed.verdict,
        run.force_run,
        run.plan_commit_ref.as_ref(),
    ) {
        return Err(
            "plan_requires_review: call `plasm` dry-run first, then pass the returned `plan_commit_ref` (`pcN`) to `plasm_run`"
                .to_string(),
        );
    }

    if !run.wait_live {
        return Err("plasm_run requires live execute".to_string());
    }

    let await_out =
        crate::mcp_plasm_run_phases::mcp_plasm_run_phase("async_live_await", || async {
            let dry = dry_for_committed_plasm_run(run.es.as_ref(), &run.bundle, &run.committed)?;
            let dag_json = crate::plasm_plan_run::plan_dag_trace_json(&dry);
            let accept_payload = build_run_explorer_accept_payload(&dry, Some(run.es.as_ref()));
            let run_result = deliver_live_run_await(
                LiveRunAwaitContext::for_mcp_plasm_run(
                    Arc::clone(&run.es),
                    Arc::clone(&run.host),
                    run.wire.prompt_hash.clone(),
                    run.wire.session_id.clone(),
                    run.wire.session_ref.clone(),
                    run.wire.mcp_session_key.clone(),
                    run.bundle.clone(),
                    accept_payload,
                    run.committed.verdict,
                    run.plan_commit_ref.clone(),
                    run.mcp_trace.clone(),
                    dry,
                ),
                LiveRunSpawnOpts {
                    plan_trace: run.plan_trace.clone(),
                },
            )
            .await
            .map_err(|e| match e {
                LiveRunError::Timeout(d) => format!("live run timed out after {d:?}"),
                LiveRunError::Failed(msg) => msg,
            })?;
            Ok::<(PlasmPlanRunResult, serde_json::Value), String>((run_result, dag_json))
        })
        .await?;

    let (result, dag_json) = await_out;

    crate::mcp_plasm_run_phases::mcp_plasm_run_phase("artifact_persist", || async {
        let dry = dry_for_committed_plasm_run(run.es.as_ref(), &run.bundle, &run.committed)?;
        let plan_ux_reflection = Some(crate::plan_ux_reflection::plan_ux_reflection_value(
            &dry,
            &crate::plan_ux_reflection::PlanUxBuildContext {
                session: Some(run.es.as_ref()),
                param_bindings: &[],
            },
        ));
        trace_archive_and_emit_code_plan_execute(
            run.artifacts.trace_hub.as_ref(),
            run.artifacts.run_artifacts.as_ref(),
            run.wire.ls_key.as_str(),
            run.es.as_ref(),
            run.wire.prompt_hash.as_str(),
            run.wire.session_id.as_str(),
            run.wire.session_ref.as_str(),
            &run.artifacts.comp_archive,
            run.artifacts.program_for_trace.as_str(),
            result.comp.clone(),
            dag_json,
            plan_ux_reflection,
            run.artifacts.plan_call_index,
            &result,
        )
        .await;
        Ok(result)
    })
    .await
}
