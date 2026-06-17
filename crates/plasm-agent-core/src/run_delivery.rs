//! Live-run delivery policy (MCP await-by-default vs HTTP async surface).
//!
//! Bounded synchronous live execute is unified in [`crate::sync_live_run`] — see concurrent
//! invariants I1–I12 documented there.

use std::sync::Arc;

use plasm_core::PlanCommitRef;
use plasm_runtime::CancelSignal;

use crate::execute_session::ExecuteSession;
use crate::mcp_run_await::{await_operation_terminal, AwaitConfig, AwaitError};
use crate::operation::{
    live_run_should_auto_async, op_accept_context_from_executable, should_spawn_async_live_run,
    spawn_async_plan_run,
};
use crate::plan_dry_display::{PlanDryReview, PlanDryVerdict};
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_plan_run::PlasmPlanRunResult;
use crate::run_explorer_meta::RunExplorerAcceptPayload;
use crate::server_state::PlasmHostState;
use crate::trace_sink_emit::PlasmTraceContext;

pub use crate::sync_live_run::{
    run_bounded_sync_live_run, BoundedSyncLiveRunRequest, SyncLiveProgress,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunDeliveryPolicy {
    /// MCP default: spawn internally when needed, await terminal server-side, one TSV response.
    McpAwaitTerminal,
    /// HTTP execute: explicit `wait=false` + SSE / plain `oN` handles.
    HttpExecute,
}

#[must_use]
pub fn should_spawn_async_for_policy(
    policy: RunDeliveryPolicy,
    wait_live: bool,
    review: &PlanDryReview,
) -> bool {
    match policy {
        RunDeliveryPolicy::McpAwaitTerminal => wait_live && review.execution_is_expensive(),
        RunDeliveryPolicy::HttpExecute => should_spawn_async_live_run(wait_live, review),
    }
}

pub struct McpExpensiveLiveRunContext {
    pub es: Arc<ExecuteSession>,
    pub st: Arc<PlasmHostState>,
    pub prompt_hash: String,
    pub session_id: String,
    pub session_ref: String,
    pub mcp_session_key: String,
    pub bundle: PlasmCompBundle,
    pub review: PlanDryReview,
    pub accept_payload: RunExplorerAcceptPayload,
    pub dry_verdict: PlanDryVerdict,
    pub plan_commit_ref: Option<PlanCommitRef>,
    pub trace: PlasmTraceContext,
    pub wait_live: bool,
    pub await_cfg: AwaitConfig,
}

/// When the plan is expensive and `wait_live`, spawn internally and await one terminal response.
/// Returns `Ok(None)` when the caller should run the synchronous live path instead.
pub async fn deliver_mcp_expensive_live_run(
    ctx: McpExpensiveLiveRunContext,
) -> Result<Option<PlasmPlanRunResult>, String> {
    let policy = RunDeliveryPolicy::McpAwaitTerminal;
    if !should_spawn_async_for_policy(policy, ctx.wait_live, &ctx.review) {
        return Ok(None);
    }

    let handle = ctx.es.mint_operation_handle(ctx.session_ref.as_str());
    let mut accept = op_accept_context_from_executable(
        ctx.plan_commit_ref.clone(),
        Some(ctx.dry_verdict),
        false,
        Some(ctx.mcp_session_key.clone()),
        ctx.bundle.executable(),
        &ctx.bundle.artifact().comp,
    );
    accept.comp = Some(ctx.accept_payload.comp.clone());
    accept.plan_ux_reflection = Some(ctx.accept_payload.plan_ux_reflection.clone());
    accept.step_order = ctx.accept_payload.step_order.clone();

    spawn_async_plan_run(
        Arc::clone(&ctx.es),
        Arc::clone(&ctx.st),
        ctx.prompt_hash,
        ctx.session_id,
        ctx.bundle,
        handle.clone(),
        CancelSignal::new(),
        accept,
    )?;

    await_operation_terminal(
        Arc::clone(&ctx.es),
        Arc::clone(&ctx.st),
        handle,
        ctx.trace,
        ctx.await_cfg,
    )
    .await
    .map(Some)
    .map_err(|e| match e {
        AwaitError::Timeout(d) => format!("live run timed out after {d:?}"),
        AwaitError::Operation(msg) => msg,
    })
}

#[must_use]
pub fn live_run_should_auto_async_for_policy(
    policy: RunDeliveryPolicy,
    wait_live: bool,
    review: &PlanDryReview,
) -> bool {
    match policy {
        RunDeliveryPolicy::McpAwaitTerminal => false,
        RunDeliveryPolicy::HttpExecute => live_run_should_auto_async(review, wait_live),
    }
}

#[cfg(test)]
mod tests {
    use crate::plan_dry_display::PlanDryReview;

    use super::*;

    fn expensive_review() -> PlanDryReview {
        PlanDryReview {
            has_unbounded_read_root: true,
            ..Default::default()
        }
    }

    #[test]
    fn mcp_await_spawns_internally_for_expensive_plans() {
        let review = expensive_review();
        assert!(should_spawn_async_for_policy(
            RunDeliveryPolicy::McpAwaitTerminal,
            true,
            &review
        ));
        assert!(!live_run_should_auto_async_for_policy(
            RunDeliveryPolicy::McpAwaitTerminal,
            true,
            &review
        ));
    }

    #[test]
    fn http_execute_keeps_existing_async_gate() {
        let review = expensive_review();
        assert!(should_spawn_async_for_policy(
            RunDeliveryPolicy::HttpExecute,
            true,
            &review
        ));
        assert!(live_run_should_auto_async_for_policy(
            RunDeliveryPolicy::HttpExecute,
            true,
            &review
        ));
    }
}
