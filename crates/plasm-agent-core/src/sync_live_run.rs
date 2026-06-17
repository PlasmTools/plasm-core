//! Bounded synchronous live execute — shared HTTP + MCP path.
//!
//! # Concurrent invariants
//!
//! | ID | Invariant |
//! |----|-----------|
//! | I1 | `sync_live_run_inflight` cleared on all exits via [`SyncLiveRunGuard`] |
//! | I2 | At most one nested sync live run per execute session |
//! | I3 | One [`CancelSignal`] per bounded sync run (scope == runtime) |
//! | I4 | Bounded sync never registers `operation_by_handle` |
//! | I5 | HTTP and MCP share this module's deadline + scope + cancel policy |
//! | I6 | Cheap plans bounded by [`crate::host_env_config::bounded_sync_run_deadline`] |
//! | I7 | Timeout cancels scope before returning |
//! | I8 | Runtime cancel via `with_plan_execute_scope` task-local |
//! | I9 | Sync may run while async op is in flight (orthogonal gates) |
//! | I10 | Telemetry slot cleared when guard drops |
//! | I11 | MCP progress via `queue_mcp_notify` only (no op finalize) |
//! | I12 | Trivial bounded GET completes under deadline (integration tests) |

use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use plasm_core::PlanCommitRef;
use plasm_runtime::{with_live_run_telemetry, CancelSignal, LiveRunTelemetry};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::execute_pipeline::{ExecutePipeline, ExecutionIntent};
use crate::execute_session::ExecuteSession;
use crate::host_env_config::bounded_sync_run_deadline;
use crate::operation::{ExecutionScope, OperationPhase, OperationProgress, SyncLiveProgressCtx};
use crate::operation_progress::OperationAgentEmitState;
use crate::plan_dry_display::{PlanDryReview, PlanDryVerdict};
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_plan_run::{DryPlasmPlanEvaluation, PlasmPlanRunHooks, PlasmPlanRunResult};
use crate::server_state::PlasmHostState;

/// RAII gate for one synchronous live run per execute session (I1, I10).
pub struct SyncLiveRunGuard {
    es: Arc<ExecuteSession>,
    cancel: CancelSignal,
    abort_on_drop: bool,
}

impl SyncLiveRunGuard {
    pub fn enter(es: Arc<ExecuteSession>) -> Result<(Self, Arc<LiveRunTelemetry>), String> {
        let telemetry = es.begin_sync_live_run()?;
        let cancel = CancelSignal::new();
        Ok((
            Self {
                es,
                cancel: cancel.clone(),
                abort_on_drop: false,
            },
            telemetry,
        ))
    }

    #[must_use]
    pub fn cancel_signal(&self) -> &CancelSignal {
        &self.cancel
    }

    pub fn mark_aborted(&mut self) {
        self.cancel.cancel();
        self.abort_on_drop = true;
    }
}

impl Drop for SyncLiveRunGuard {
    fn drop(&mut self) {
        if self.abort_on_drop {
            self.cancel.cancel();
        }
        self.es.end_sync_live_run();
    }
}

/// MCP-only progress fanout for bounded sync (I11).
pub struct SyncLiveProgress {
    pub session_ref: String,
    pub mcp_transport_key: String,
    pub plan_commit_ref: Option<PlanCommitRef>,
}

pub struct BoundedSyncLiveRunRequest<'a> {
    pub es: Arc<ExecuteSession>,
    pub st: Arc<PlasmHostState>,
    pub prompt_hash: String,
    pub session_id: String,
    pub bundle: PlasmCompBundle,
    pub dry_review: PlanDryReview,
    pub dry_verdict: Option<PlanDryVerdict>,
    pub dry_gate: Option<DryPlasmPlanEvaluation>,
    pub hooks: Option<PlasmPlanRunHooks<'a>>,
    pub progress: Option<SyncLiveProgress>,
}

struct SyncRunTicker {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

impl SyncRunTicker {
    fn spawn(
        es: Arc<ExecuteSession>,
        ctx: Arc<SyncLiveProgressCtx>,
        st: Arc<PlasmHostState>,
    ) -> Self {
        let cancel = CancellationToken::new();
        let handle = spawn_sync_live_progress_ticker(es, ctx, st, cancel.clone());
        Self { cancel, handle }
    }

    async fn stop(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

/// Whether bounded sync skips the wall-clock deadline (expensive plans on sync path).
#[must_use]
pub fn bounded_sync_skips_deadline(review: &PlanDryReview) -> bool {
    review.execution_is_expensive()
}

/// Apply deadline policy for bounded sync (testable wrapper around `tokio::time::timeout`).
pub(crate) async fn await_bounded_sync_live<F, T>(
    review: &PlanDryReview,
    deadline: Duration,
    scope: &ExecutionScope,
    guard: &mut SyncLiveRunGuard,
    fut: F,
) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    if bounded_sync_skips_deadline(review) {
        fut.await
    } else {
        match tokio::time::timeout(deadline, fut).await {
            Ok(result) => result,
            Err(_) => {
                scope.cancel();
                guard.mark_aborted();
                Err(format!(
                    "sync plasm_run deadline exceeded ({:.0}s); outbound HTTP or auth may be stalled",
                    deadline.as_secs_f64()
                ))
            }
        }
    }
}

/// Single entry for bounded synchronous live execute (HTTP + MCP).
pub async fn run_bounded_sync_live_run<'a>(
    req: BoundedSyncLiveRunRequest<'a>,
) -> Result<PlasmPlanRunResult, String> {
    let (mut guard, telemetry) = SyncLiveRunGuard::enter(Arc::clone(&req.es))?;
    let cancel = guard.cancel_signal().clone();

    let (scope, ticker, progress_ctx) = if let Some(progress) = req.progress {
        let handle = req.es.mint_operation_handle(progress.session_ref.as_str());
        let ctx = Arc::new(SyncLiveProgressCtx {
            handle,
            mcp_transport_key: progress.mcp_transport_key,
            plan_commit_ref: progress.plan_commit_ref,
            progress: Arc::new(StdMutex::new(OperationProgress::default())),
            emit_state: Arc::new(StdMutex::new(OperationAgentEmitState::default())),
            host: Arc::downgrade(&req.st),
        });
        req.es
            .emit_sync_live_accept(ctx.as_ref(), req.st.as_ref(), req.dry_verdict)?;
        let scope = ExecutionScope::for_sync_live(Arc::clone(&req.es), Arc::clone(&ctx), cancel);
        let ticker =
            SyncRunTicker::spawn(Arc::clone(&req.es), Arc::clone(&ctx), Arc::clone(&req.st));
        (scope, Some(ticker), Some(ctx))
    } else {
        (ExecutionScope::for_bounded_sync(cancel), None, None)
    };

    let deadline = bounded_sync_run_deadline();

    let sync_future = with_live_run_telemetry(telemetry, async {
        ExecutePipeline::run_program(
            req.es.as_ref(),
            req.st.as_ref(),
            req.prompt_hash.as_str(),
            req.session_id.as_str(),
            &req.bundle,
            ExecutionIntent::Live,
            req.hooks,
            Some(&scope),
            req.dry_gate,
        )
        .await
    });

    let result =
        await_bounded_sync_live(&req.dry_review, deadline, &scope, &mut guard, sync_future).await;

    if let Some(ticker) = ticker {
        ticker.stop().await;
    }

    if let Some(ctx) = progress_ctx {
        match &result {
            Ok(_) => req.es.emit_sync_live_terminal(
                ctx.as_ref(),
                OperationPhase::Succeeded,
                None,
                req.st.as_ref(),
            ),
            Err(e) => req.es.emit_sync_live_terminal(
                ctx.as_ref(),
                OperationPhase::Failed,
                Some(e.as_str()),
                req.st.as_ref(),
            ),
        }
    }

    result
}

/// Periodic MCP op notifications while a bounded sync live run is in flight.
pub fn spawn_sync_live_progress_ticker(
    es: Arc<ExecuteSession>,
    ctx: Arc<SyncLiveProgressCtx>,
    st: Arc<PlasmHostState>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = interval.tick() => {
                    es.try_emit_sync_live_progress_coalesced(ctx.as_ref(), st.as_ref());
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use indexmap::IndexMap;
    use plasm_core::CGS;

    use super::*;
    use crate::execute_session::ExecuteSession;

    fn test_session() -> Arc<ExecuteSession> {
        let cgs = Arc::new(CGS::new());
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(plasm_core::CgsContext::entry("default", Arc::clone(&cgs))),
        );
        Arc::new(ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs,
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            "hash".into(),
            None,
            None,
        ))
    }

    #[test]
    fn sync_live_run_guard_drop_clears_inflight() {
        let es = test_session();
        {
            let (_guard, _tel) = SyncLiveRunGuard::enter(Arc::clone(&es)).expect("enter");
            assert!(es.sync_live_run_inflight_for_test());
        }
        assert!(!es.sync_live_run_inflight_for_test());
        SyncLiveRunGuard::enter(es).expect("second enter");
    }

    #[test]
    fn sync_live_run_guard_drop_on_early_err() {
        let es = test_session();
        let err: Result<(), String> = (|| {
            let (_guard, _tel) = SyncLiveRunGuard::enter(Arc::clone(&es))?;
            Err("early exit".to_string())
        })();
        assert!(err.is_err());
        assert!(!es.sync_live_run_inflight_for_test());
    }

    #[test]
    fn sync_live_telemetry_cleared_after_guard_drop() {
        let es = test_session();
        {
            let (_guard, tel) = SyncLiveRunGuard::enter(Arc::clone(&es)).expect("enter");
            assert!(es.sync_live_telemetry_active_for_test());
            tel.record_http_completion(Duration::from_millis(1));
        }
        assert!(!es.sync_live_telemetry_active_for_test());
    }

    #[test]
    fn nested_sync_live_run_rejected() {
        let es = test_session();
        let (_g1, _) = SyncLiveRunGuard::enter(Arc::clone(&es)).expect("first");
        let err = SyncLiveRunGuard::enter(es).err().expect("nested");
        assert!(err.contains("operation_in_flight"));
    }

    #[test]
    fn bounded_sync_timeout_cancels_scope() {
        let cancel = CancelSignal::new();
        let scope = ExecutionScope::for_bounded_sync(cancel.clone());
        scope.cancel();
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn bounded_sync_single_cancel_wires_task_local() {
        let cancel = CancelSignal::new();
        let observed = crate::operation::with_plan_execute_cancel(Some(cancel.clone()), async {
            let slot = crate::operation::plan_execute_cancel_signal();
            assert!(slot.is_some());
            assert!(!slot.unwrap().is_cancelled());
            cancel.cancel();
            crate::operation::plan_execute_cancel_signal()
                .map(|c| c.is_cancelled())
                .unwrap_or(false)
        })
        .await;
        assert!(observed);
    }

    #[test]
    fn cheap_plan_sync_deadline_gate() {
        let cheap = PlanDryReview::default();
        assert!(!bounded_sync_skips_deadline(&cheap));
    }

    #[test]
    fn expensive_sync_skips_deadline() {
        let expensive = PlanDryReview {
            has_unbounded_read_root: true,
            ..Default::default()
        };
        assert!(bounded_sync_skips_deadline(&expensive));
    }

    #[tokio::test]
    async fn cheap_plan_sync_deadline_enforced() {
        let es = test_session();
        let (mut guard, _) = SyncLiveRunGuard::enter(es).expect("enter");
        let scope = ExecutionScope::for_bounded_sync(guard.cancel_signal().clone());
        let cheap = PlanDryReview::default();
        let result = await_bounded_sync_live(
            &cheap,
            Duration::from_millis(25),
            &scope,
            &mut guard,
            async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(42)
            },
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("deadline exceeded"));
        assert!(scope.cancel.is_cancelled());
    }

    #[tokio::test]
    async fn expensive_sync_skips_deadline_wait() {
        let es = test_session();
        let (mut guard, _) = SyncLiveRunGuard::enter(es).expect("enter");
        let scope = ExecutionScope::for_bounded_sync(guard.cancel_signal().clone());
        let expensive = PlanDryReview {
            has_unbounded_read_root: true,
            ..Default::default()
        };
        let result = await_bounded_sync_live(
            &expensive,
            Duration::from_millis(25),
            &scope,
            &mut guard,
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok("ok")
            },
        )
        .await;
        assert_eq!(result, Ok("ok"));
    }

    #[tokio::test]
    async fn bounded_sync_stall_returns_deadline_error() {
        let es = test_session();
        let cancel_was_set = {
            let (mut guard, _) = SyncLiveRunGuard::enter(Arc::clone(&es)).expect("enter");
            let scope = ExecutionScope::for_bounded_sync(guard.cancel_signal().clone());
            let err = await_bounded_sync_live(
                &PlanDryReview::default(),
                Duration::from_millis(10),
                &scope,
                &mut guard,
                async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    Ok(())
                },
            )
            .await
            .expect_err("stall");
            assert!(err.contains("deadline exceeded"));
            scope.cancel.is_cancelled()
        };
        assert!(cancel_was_set);
        assert!(!es.sync_live_run_inflight_for_test());
    }
}
