//! Live-run delivery: policy, spawn + terminal await (MCP and HTTP).

use std::sync::Arc;
use std::time::Duration;

use plasm_core::OperationHandle;
use plasm_core::PlanCommitRef;
use plasm_runtime::CancelSignal;

use crate::execute_session::ExecuteSession;
use crate::mcp_run_await::{
    await_operation_terminal, AwaitConfig, AwaitError, TerminalAwaitContext,
};
use crate::operation::{
    async_live_run_accept_parts, op_accept_context_from_executable, spawn_async_plan_run,
};
use crate::plan_dry_display::{PlanDryReview, PlanDryVerdict};
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_plan_run::{DryPlasmPlanEvaluation, PlasmPlanRunResult};
use crate::run_explorer_meta::{build_run_explorer_accept_payload, RunExplorerAcceptPayload};
use crate::server_state::PlasmHostState;
use crate::trace_sink_emit::PlasmTraceContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunDeliveryPolicy {
    /// MCP: always spawn async and await terminal server-side when `wait_live`.
    McpAwaitTerminal,
    /// HTTP execute: `wait=false` returns accept; expensive + `wait=true` returns accept; cheap + `wait=true` awaits.
    HttpExecute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunDeliveryDecision {
    /// Spawn async and block until terminal rows.
    ServerAwait,
    /// Spawn async and return accept wire (`+ oN`) without server-await.
    ReturnAccept { auto_async: bool },
}

#[must_use]
pub fn decide_delivery(
    policy: RunDeliveryPolicy,
    wait_live: bool,
    review: &PlanDryReview,
) -> RunDeliveryDecision {
    match policy {
        RunDeliveryPolicy::McpAwaitTerminal => {
            if wait_live {
                RunDeliveryDecision::ServerAwait
            } else {
                RunDeliveryDecision::ReturnAccept { auto_async: false }
            }
        }
        RunDeliveryPolicy::HttpExecute => {
            if !wait_live || review.execution_is_expensive() {
                RunDeliveryDecision::ReturnAccept {
                    auto_async: wait_live && review.execution_is_expensive(),
                }
            } else {
                RunDeliveryDecision::ServerAwait
            }
        }
    }
}

/// HTTP live run returns `+ oN` accept without server-await (agent polls `wait(oN)`).
#[must_use]
pub fn http_live_run_returns_accept_immediately(wait_live: bool, review: &PlanDryReview) -> bool {
    matches!(
        decide_delivery(RunDeliveryPolicy::HttpExecute, wait_live, review),
        RunDeliveryDecision::ReturnAccept { .. }
    )
}

#[must_use]
pub fn should_spawn_async_for_policy(
    policy: RunDeliveryPolicy,
    wait_live: bool,
    review: &PlanDryReview,
) -> bool {
    match policy {
        RunDeliveryPolicy::McpAwaitTerminal => wait_live,
        RunDeliveryPolicy::HttpExecute => {
            http_live_run_returns_accept_immediately(wait_live, review)
        }
    }
}

#[must_use]
pub fn live_run_should_auto_async_for_policy(
    policy: RunDeliveryPolicy,
    wait_live: bool,
    review: &PlanDryReview,
) -> bool {
    matches!(
        decide_delivery(policy, wait_live, review),
        RunDeliveryDecision::ReturnAccept { auto_async: true }
    )
}

#[derive(Debug, Clone)]
pub enum OperationWire {
    Mcp {
        session_ref: String,
        transport_key: String,
    },
    HttpPlain,
}

impl OperationWire {
    fn mint_handle(&self, es: &ExecuteSession) -> OperationHandle {
        match self {
            Self::Mcp { session_ref, .. } => es.mint_operation_handle(session_ref.as_str()),
            Self::HttpPlain => es.mint_operation_handle_plain(),
        }
    }

    fn mcp_transport_key(&self) -> Option<String> {
        match self {
            Self::Mcp { transport_key, .. } => Some(transport_key.clone()),
            Self::HttpPlain => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LiveRunError {
    #[error("live run timed out after {0:?}")]
    Timeout(Duration),
    #[error("{0}")]
    Failed(String),
}

impl From<AwaitError> for LiveRunError {
    fn from(e: AwaitError) -> Self {
        match e {
            AwaitError::Timeout(d) => Self::Timeout(d),
            AwaitError::Operation(msg) => Self::Failed(msg),
        }
    }
}

pub struct LiveRunSpawn {
    pub es: Arc<ExecuteSession>,
    pub st: Arc<PlasmHostState>,
    pub prompt_hash: String,
    pub session_id: String,
    pub bundle: PlasmCompBundle,
    pub wire: OperationWire,
    pub plan_commit_ref: Option<PlanCommitRef>,
    pub dry_verdict: PlanDryVerdict,
    pub auto_async: bool,
    pub accept_payload: RunExplorerAcceptPayload,
    pub dry: Option<DryPlasmPlanEvaluation>,
    pub evidence_anchors: plasm_evidence::EvidenceAnchors,
}

/// Optional spawn-time hooks not carried on [`LiveRunAwaitContext`].
#[derive(Clone, Default)]
pub struct LiveRunSpawnOpts {
    pub plan_trace: Option<crate::trace_hub::PlanRunTraceHooks>,
    pub mcp_result_policy: Option<crate::mcp_run_markdown::McpResultTransportPolicy>,
}

fn spawn_live_plan_run(
    spawn: LiveRunSpawn,
    opts: LiveRunSpawnOpts,
) -> Result<OperationHandle, LiveRunError> {
    let handle = spawn.wire.mint_handle(spawn.es.as_ref());
    let mut accept = op_accept_context_from_executable(
        spawn.plan_commit_ref.clone(),
        Some(spawn.dry_verdict),
        spawn.auto_async,
        spawn.wire.mcp_transport_key(),
        spawn.bundle.executable(),
        &spawn.bundle.artifact().comp,
    )
    .with_run_explorer(&spawn.accept_payload);
    if let Some(plan_trace) = opts.plan_trace {
        accept = accept.with_plan_trace(plan_trace);
    }
    if let Some(policy) = opts.mcp_result_policy {
        accept = accept.with_mcp_result_policy(policy);
    }
    accept = accept.with_evidence_anchors(spawn.evidence_anchors.clone());
    spawn_async_plan_run(
        Arc::clone(&spawn.es),
        Arc::clone(&spawn.st),
        spawn.prompt_hash,
        spawn.session_id,
        spawn.bundle,
        handle.clone(),
        CancelSignal::new(),
        accept,
        spawn.dry,
    )
    .map_err(LiveRunError::Failed)?;
    Ok(handle)
}

pub struct LiveRunAwaitContext {
    pub es: Arc<ExecuteSession>,
    pub st: Arc<PlasmHostState>,
    pub prompt_hash: String,
    pub session_id: String,
    pub wire: OperationWire,
    pub bundle: PlasmCompBundle,
    pub accept_payload: RunExplorerAcceptPayload,
    pub dry_verdict: PlanDryVerdict,
    pub plan_commit_ref: Option<PlanCommitRef>,
    pub trace: PlasmTraceContext,
    pub await_cfg: AwaitConfig,
    pub dry: Option<DryPlasmPlanEvaluation>,
}

impl LiveRunAwaitContext {
    #[allow(clippy::too_many_arguments)]
    pub fn for_mcp_plasm_run(
        es: Arc<ExecuteSession>,
        st: Arc<PlasmHostState>,
        prompt_hash: String,
        session_id: String,
        session_ref: String,
        mcp_session_key: String,
        bundle: PlasmCompBundle,
        accept_payload: RunExplorerAcceptPayload,
        dry_verdict: PlanDryVerdict,
        plan_commit_ref: Option<PlanCommitRef>,
        trace: PlasmTraceContext,
        dry: DryPlasmPlanEvaluation,
    ) -> Self {
        Self {
            es,
            st,
            prompt_hash,
            session_id,
            wire: OperationWire::Mcp {
                session_ref,
                transport_key: mcp_session_key,
            },
            bundle,
            accept_payload,
            dry_verdict,
            plan_commit_ref,
            trace,
            await_cfg: AwaitConfig::default(),
            dry: Some(dry),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn for_http_wait_true(
        es: Arc<ExecuteSession>,
        st: Arc<PlasmHostState>,
        prompt_hash: String,
        session_id: String,
        bundle: PlasmCompBundle,
        accept_payload: RunExplorerAcceptPayload,
        dry_verdict: PlanDryVerdict,
        plan_commit_ref: Option<PlanCommitRef>,
        dry: DryPlasmPlanEvaluation,
    ) -> Self {
        Self {
            es,
            st,
            prompt_hash,
            session_id,
            wire: OperationWire::HttpPlain,
            bundle,
            accept_payload,
            dry_verdict,
            plan_commit_ref,
            trace: crate::http_execute::http_operation_trace(),
            await_cfg: AwaitConfig::default(),
            dry: Some(dry),
        }
    }
}

pub struct HttpLiveRunRequest {
    pub es: Arc<ExecuteSession>,
    pub st: Arc<PlasmHostState>,
    pub prompt_hash: String,
    pub session_id: String,
    pub bundle: PlasmCompBundle,
    pub dry: DryPlasmPlanEvaluation,
    pub wait_live: bool,
    pub review: PlanDryReview,
    pub verdict_for_gate: PlanDryVerdict,
    pub plan_commit_ref: Option<PlanCommitRef>,
}

/// HTTP live execute: accept immediately or server-await terminal rows.
pub enum HttpLiveRunOutcome {
    Accept {
        handle: OperationHandle,
        result: PlasmPlanRunResult,
    },
    Completed(PlasmPlanRunResult),
}

pub async fn deliver_http_live_run(
    req: HttpLiveRunRequest,
) -> Result<HttpLiveRunOutcome, LiveRunError> {
    let accept_payload = build_run_explorer_accept_payload(&req.dry, Some(req.es.as_ref()));
    match decide_delivery(RunDeliveryPolicy::HttpExecute, req.wait_live, &req.review) {
        RunDeliveryDecision::ReturnAccept { auto_async } => {
            let handle = spawn_live_plan_run(
                LiveRunSpawn {
                    es: Arc::clone(&req.es),
                    st: Arc::clone(&req.st),
                    prompt_hash: req.prompt_hash,
                    session_id: req.session_id,
                    bundle: req.bundle,
                    wire: OperationWire::HttpPlain,
                    plan_commit_ref: req.plan_commit_ref.clone(),
                    dry_verdict: req.verdict_for_gate,
                    auto_async,
                    accept_payload: accept_payload.clone(),
                    dry: Some(req.dry),
                    evidence_anchors: crate::evidence_chain::evidence_anchors(
                        req.plan_commit_ref.as_ref(),
                        None,
                        None,
                    ),
                },
                LiveRunSpawnOpts::default(),
            )?;
            let (markdown, mut meta) = async_live_run_accept_parts(
                &handle,
                req.plan_commit_ref.as_ref(),
                req.verdict_for_gate,
                auto_async,
            );
            crate::run_explorer_meta::merge_accept_payload_into_meta(
                &mut meta,
                "oN",
                &accept_payload,
            );
            Ok(HttpLiveRunOutcome::Accept {
                handle,
                result: PlasmPlanRunResult {
                    version: serde_json::json!({}),
                    node_results: Vec::new(),
                    graph_summary: serde_json::json!({}),
                    comp: Some(accept_payload.comp_wire.clone()),
                    code_plan_run_artifacts: Vec::new(),
                    run_markdown: Some(markdown),
                    run_plasm_meta: Some(meta),
                    return_steps: Vec::new(),
                },
            })
        }
        RunDeliveryDecision::ServerAwait => {
            let completed = deliver_live_run_await(
                LiveRunAwaitContext::for_http_wait_true(
                    req.es,
                    req.st,
                    req.prompt_hash,
                    req.session_id,
                    req.bundle,
                    accept_payload,
                    req.verdict_for_gate,
                    req.plan_commit_ref,
                    req.dry,
                ),
                LiveRunSpawnOpts::default(),
            )
            .await?;
            Ok(HttpLiveRunOutcome::Completed(completed))
        }
    }
}

/// Spawn one async live plan run and block until terminal (`!` / `?` / `x`).
pub async fn deliver_live_run_await(
    ctx: LiveRunAwaitContext,
    spawn_opts: LiveRunSpawnOpts,
) -> Result<PlasmPlanRunResult, LiveRunError> {
    let evidence_anchors = crate::evidence_chain::evidence_anchors(
        ctx.plan_commit_ref.as_ref(),
        Some(ctx.trace.trace_id),
        ctx.trace.call_index.map(|i| i as u64),
    );

    let handle = spawn_live_plan_run(
        LiveRunSpawn {
            es: Arc::clone(&ctx.es),
            st: Arc::clone(&ctx.st),
            prompt_hash: ctx.prompt_hash,
            session_id: ctx.session_id,
            bundle: ctx.bundle,
            wire: ctx.wire,
            plan_commit_ref: ctx.plan_commit_ref,
            dry_verdict: ctx.dry_verdict,
            auto_async: false,
            accept_payload: ctx.accept_payload,
            dry: ctx.dry,
            evidence_anchors,
        },
        spawn_opts,
    )?;

    await_operation_terminal(TerminalAwaitContext {
        es: Arc::clone(&ctx.es),
        st: Arc::clone(&ctx.st),
        handle,
        trace: ctx.trace,
        cfg: ctx.await_cfg,
    })
    .await
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expensive_review() -> PlanDryReview {
        PlanDryReview {
            has_unbounded_read_root: true,
            ..Default::default()
        }
    }

    fn cheap_review() -> PlanDryReview {
        PlanDryReview::default()
    }

    #[test]
    fn mcp_always_spawns_when_wait_live() {
        let expensive = expensive_review();
        let cheap = cheap_review();
        assert!(should_spawn_async_for_policy(
            RunDeliveryPolicy::McpAwaitTerminal,
            true,
            &expensive
        ));
        assert!(should_spawn_async_for_policy(
            RunDeliveryPolicy::McpAwaitTerminal,
            true,
            &cheap
        ));
        assert!(!should_spawn_async_for_policy(
            RunDeliveryPolicy::McpAwaitTerminal,
            false,
            &cheap
        ));
    }

    #[test]
    fn http_cheap_wait_server_awaits() {
        let cheap = cheap_review();
        assert!(!http_live_run_returns_accept_immediately(true, &cheap));
        assert!(!should_spawn_async_for_policy(
            RunDeliveryPolicy::HttpExecute,
            true,
            &cheap
        ));
    }

    #[test]
    fn http_expensive_wait_returns_accept() {
        let expensive = expensive_review();
        assert!(http_live_run_returns_accept_immediately(true, &expensive));
        assert!(should_spawn_async_for_policy(
            RunDeliveryPolicy::HttpExecute,
            true,
            &expensive
        ));
        assert!(live_run_should_auto_async_for_policy(
            RunDeliveryPolicy::HttpExecute,
            true,
            &expensive
        ));
    }

    #[test]
    fn http_wait_false_returns_accept() {
        let cheap = cheap_review();
        assert!(http_live_run_returns_accept_immediately(false, &cheap));
    }

    #[test]
    fn http_auto_async_only_for_expensive_with_wait() {
        let expensive = expensive_review();
        let advisory = PlanDryReview {
            has_full_collection_compute: true,
            ..PlanDryReview::default()
        };
        assert!(live_run_should_auto_async_for_policy(
            RunDeliveryPolicy::HttpExecute,
            true,
            &expensive
        ));
        assert!(!live_run_should_auto_async_for_policy(
            RunDeliveryPolicy::HttpExecute,
            false,
            &expensive
        ));
        assert!(!live_run_should_auto_async_for_policy(
            RunDeliveryPolicy::HttpExecute,
            true,
            &advisory
        ));
        assert!(should_spawn_async_for_policy(
            RunDeliveryPolicy::HttpExecute,
            false,
            &advisory
        ));
        assert!(should_spawn_async_for_policy(
            RunDeliveryPolicy::HttpExecute,
            true,
            &expensive
        ));
        assert!(!should_spawn_async_for_policy(
            RunDeliveryPolicy::HttpExecute,
            true,
            &advisory
        ));
    }
}
