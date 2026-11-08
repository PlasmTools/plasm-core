//! A bounded, workflow-owned pause in an executing effect future.
//!
//! The suspended future retains its private operands and enclosing interpreter
//! frames. A pause is only available for a single catalog-classified no-effect
//! rejection. It is not a repair certificate and does not prove selection
//! coverage or completion of the enclosing plan.

use crate::{ExecutionFailure, MutationDispatchStatus};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Notify};
use uuid::Uuid;

/// A reviewed execution cannot keep an unattended checkpoint indefinitely.
pub const MAX_EFFECT_CONTINUATION_LEASE: Duration = Duration::from_secs(600);
/// Accounted payload retained by active nested selection frames in one run.
pub const MAX_EFFECT_SELECTION_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EffectContinuationError {
    #[error("continuation lease must be positive and at most ten minutes")]
    InvalidLease,
    #[error("the failure is not a single known no-effect rejection")]
    NotRetryable,
    #[error("another effect checkpoint is already pending")]
    ConcurrentCheckpoint,
    #[error("the continuation lease expired")]
    Expired,
    #[error("the continuation was abandoned")]
    Abandoned,
    #[error("the continuation was cancelled")]
    Cancelled,
    #[error("the continuation belongs to another workflow")]
    WorkflowMismatch,
    #[error("the continuation has already been resumed or expired")]
    AlreadyResolved,
    #[error("continuation selection cannot be retained within its byte budget: {0}")]
    SelectionBudget(#[source] plasm_core::value::ValueBudgetError),
}

struct State {
    workflow: Uuid,
    lease: Duration,
    deadline: Mutex<Option<Instant>>,
    pending: AtomicBool,
    cancelled: AtomicBool,
    cancel_notify: Notify,
    pauses: mpsc::Sender<EffectPause>,
    selection_bytes: Mutex<usize>,
    saw_selection: AtomicBool,
    incomplete_selection: AtomicBool,
}

/// Native owner of one execution's suspension channel and lifetime.
#[derive(Clone)]
pub struct EffectContinuation {
    state: Arc<State>,
}

/// One-use native capability to resume the suspended future. The failure is
/// presentation evidence; possession of this value is not a completion proof.
pub struct EffectPause {
    pub failure: ExecutionFailure,
    workflow: Uuid,
    resume: Option<oneshot::Sender<()>>,
}

impl EffectPause {
    /// A different workflow cannot consume the original workflow's capability.
    pub fn resume(&mut self, workflow: Uuid) -> Result<(), EffectContinuationError> {
        if workflow != self.workflow {
            return Err(EffectContinuationError::WorkflowMismatch);
        }
        self.resume
            .take()
            .ok_or(EffectContinuationError::AlreadyResolved)?
            .send(())
            .map_err(|_| EffectContinuationError::AlreadyResolved)
    }
}

/// The execution owner consumes checkpoints; dropping it abandons a pending pause.
pub struct EffectPauseReceiver {
    pauses: mpsc::Receiver<EffectPause>,
}

/// Keeps the accounting charge alive for precisely the selection frame's lifetime.
pub struct SelectionReservation {
    state: Option<Arc<State>>,
    bytes: usize,
}

impl Drop for SelectionReservation {
    fn drop(&mut self) {
        if let Some(state) = &self.state {
            let mut used = state
                .selection_bytes
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *used -= self.bytes;
        }
    }
}

impl EffectPauseReceiver {
    pub async fn recv(&mut self) -> Option<EffectPause> {
        self.pauses.recv().await
    }
}

struct PendingGuard(Arc<State>);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.0.pending.store(false, Ordering::Release);
    }
}

tokio::task_local! {
    static CURRENT: EffectContinuation;
    static SELECTIONS: Vec<crate::execution_failure::EffectSelectionEvidence>;
}

impl EffectContinuation {
    /// Construct a private native continuation identity for a single run.
    pub fn new_native(
        lease: Duration,
    ) -> Result<(Self, EffectPauseReceiver), EffectContinuationError> {
        Self::new(Uuid::new_v4(), lease)
    }

    /// Consume a checkpoint only when it belongs to this native continuation.
    pub fn resume(&self, pause: &mut EffectPause) -> Result<(), EffectContinuationError> {
        pause.resume(self.state.workflow)
    }

    pub fn new(
        workflow: Uuid,
        lease: Duration,
    ) -> Result<(Self, EffectPauseReceiver), EffectContinuationError> {
        if lease.is_zero() || lease > MAX_EFFECT_CONTINUATION_LEASE {
            return Err(EffectContinuationError::InvalidLease);
        }
        let (pauses, receiver) = mpsc::channel(1);
        Ok((
            Self {
                state: Arc::new(State {
                    workflow,
                    lease,
                    deadline: Mutex::new(None),
                    pending: AtomicBool::new(false),
                    cancelled: AtomicBool::new(false),
                    cancel_notify: Notify::new(),
                    pauses,
                    selection_bytes: Mutex::new(0),
                    saw_selection: AtomicBool::new(false),
                    incomplete_selection: AtomicBool::new(false),
                }),
            },
            EffectPauseReceiver { pauses: receiver },
        ))
    }

    /// Install native control for this future. Independently spawned tasks must
    /// explicitly install their own scope.
    pub async fn scope<T>(self, future: impl Future<Output = T>) -> T {
        CURRENT.scope(self, future).await
    }

    /// Reserve the aggregate payload of an active selection, including nested
    /// selections. A failed reservation changes no accounting and issues no
    /// checkpoint. Other retained execution state is accounted separately.
    pub fn reserve_selection(
        rows: &[plasm_core::ValueRow],
    ) -> Result<SelectionReservation, EffectContinuationError> {
        let Some(control) = Self::current() else {
            return Ok(SelectionReservation {
                state: None,
                bytes: 0,
            });
        };
        let mut used = control
            .state
            .selection_bytes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut remaining = MAX_EFFECT_SELECTION_BYTES - *used;
        let initial = remaining;
        for row in rows {
            plasm_core::charge_value_budget(row, &mut remaining)
                .map_err(EffectContinuationError::SelectionBudget)?;
        }
        let bytes = initial - remaining;
        *used += bytes;
        drop(used);
        Ok(SelectionReservation {
            state: Some(control.state),
            bytes,
        })
    }

    /// Carry public selection accounting into a checkpoint while the owning
    /// execution future retains the actual rows. This is not a membership proof.
    pub async fn selection_scope<T>(
        selection: crate::execution_failure::EffectSelectionEvidence,
        future: impl Future<Output = T>,
    ) -> T {
        let Some(control) = Self::current() else {
            return future.await;
        };
        control.state.saw_selection.store(true, Ordering::Release);
        if selection.coverage != plasm_core::collection_codec::ResultCoverage::Complete {
            control
                .state
                .incomplete_selection
                .store(true, Ordering::Release);
        }
        let mut selections = SELECTIONS.try_with(Clone::clone).unwrap_or_default();
        selections.push(selection);
        SELECTIONS.scope(selections, future).await
    }

    /// Conservative run-wide coverage fact. This is necessary but not
    /// sufficient for a repair certificate: it contains no occurrence proof.
    pub fn selections_have_complete_coverage(&self) -> bool {
        self.state.saw_selection.load(Ordering::Acquire)
            && !self.state.incomplete_selection.load(Ordering::Acquire)
    }

    pub(crate) fn current() -> Option<Self> {
        CURRENT.try_with(Clone::clone).ok()
    }

    /// Cancel a pending checkpoint without issuing a resume capability.
    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::Release);
        self.state.cancel_notify.notify_one();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn check_active(&self) -> Result<(), EffectContinuationError> {
        if self.state.cancelled.load(Ordering::Acquire) {
            return Err(EffectContinuationError::Cancelled);
        }
        let deadline = self
            .state
            .deadline
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            Err(EffectContinuationError::Expired)
        } else {
            Ok(())
        }
    }

    /// Pause only after the single dispatch has been classified by its owning
    /// catalog as rejected without effect. Unknown or multiple dispatches may
    /// not be retried by this mechanism.
    pub(crate) async fn checkpoint(
        &self,
        mut failure: ExecutionFailure,
    ) -> Result<(), EffectContinuationError> {
        if failure.effects_unresolved
            || failure.effects.iter().any(|effect| effect.completed > 0)
            || !matches!(failure.dispatches.as_slice(), [dispatch] if dispatch.status == MutationDispatchStatus::RejectedWithoutEffect)
        {
            return Err(EffectContinuationError::NotRetryable);
        }
        if let Ok(selections) = SELECTIONS.try_with(Clone::clone) {
            for selection in selections {
                failure = failure.with_effect_selection(selection);
            }
        }
        if self.state.cancelled.load(Ordering::Acquire) {
            return Err(EffectContinuationError::Cancelled);
        }
        if self
            .state
            .pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(EffectContinuationError::ConcurrentCheckpoint);
        }
        let _guard = PendingGuard(Arc::clone(&self.state));
        let deadline = {
            let mut value = self
                .state
                .deadline
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *value.get_or_insert_with(|| Instant::now() + self.state.lease)
        };
        if Instant::now() >= deadline {
            return Err(EffectContinuationError::Expired);
        }
        let (sender, receiver) = oneshot::channel();
        let wait = async {
            self.state
                .pauses
                .send(EffectPause {
                    failure,
                    workflow: self.state.workflow,
                    resume: Some(sender),
                })
                .await
                .map_err(|_| EffectContinuationError::Abandoned)?;
            receiver
                .await
                .map_err(|_| EffectContinuationError::Abandoned)
        };
        let cancelled = self.state.cancel_notify.notified();
        tokio::pin!(cancelled);
        if self.state.cancelled.load(Ordering::Acquire) {
            return Err(EffectContinuationError::Cancelled);
        }
        let result = tokio::select! {
            biased;
            _ = &mut cancelled => Err(EffectContinuationError::Cancelled),
            result = tokio::time::timeout_at(deadline.into(), wait) => {
                match result {
                    Ok(result) => result,
                    Err(_) => Err(EffectContinuationError::Expired),
                }
            }
        };
        if self.state.cancelled.load(Ordering::Acquire) {
            Err(EffectContinuationError::Cancelled)
        } else if Instant::now() >= deadline {
            Err(EffectContinuationError::Expired)
        } else {
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::OperationIdentity;
    use crate::{FailureCause, MutationDispatch};

    fn failure(status: MutationDispatchStatus) -> ExecutionFailure {
        let mut failure = ExecutionFailure::new(FailureCause::Upstream, "fixture", "rejected");
        failure.dispatches.push(MutationDispatch {
            resolved_effect: None,
            operation: OperationIdentity {
                entry_id: "matrix".into(),
                capability: "write".into(),
            },
            request_fingerprint: "fixture".into(),
            status,
        });
        failure
    }

    #[tokio::test]
    async fn selection_coverage_is_conservative_across_the_whole_run() {
        use plasm_core::collection_codec::{CollectionIdentity, ResultCoverage};
        let (control, _pauses) =
            EffectContinuation::new(Uuid::new_v4(), Duration::from_secs(2)).unwrap();
        let selection = |coverage| crate::execution_failure::EffectSelectionEvidence {
            collection: CollectionIdentity {
                catalog: [1; 32],
                expression: [2; 32],
                epoch: 0,
            },
            coverage,
            count: 1,
            scope_path: vec!["map".into()],
            occurrence_path: vec![],
        };
        assert!(!control.selections_have_complete_coverage());
        control
            .clone()
            .scope(async {
                EffectContinuation::selection_scope(selection(ResultCoverage::Complete), async {})
                    .await;
                assert!(control.selections_have_complete_coverage());
                EffectContinuation::selection_scope(selection(ResultCoverage::Partial), async {})
                    .await;
                EffectContinuation::selection_scope(selection(ResultCoverage::Complete), async {})
                    .await;
            })
            .await;
        assert!(!control.selections_have_complete_coverage());
        let (unknown, _pauses) =
            EffectContinuation::new(Uuid::new_v4(), Duration::from_secs(2)).unwrap();
        unknown
            .clone()
            .scope(async {
                EffectContinuation::selection_scope(selection(ResultCoverage::Unknown), async {})
                    .await;
            })
            .await;
        assert!(!unknown.selections_have_complete_coverage());
    }

    #[tokio::test]
    async fn nested_selection_reservations_share_a_bounded_budget() {
        let (control, _pauses) =
            EffectContinuation::new(Uuid::new_v4(), Duration::from_secs(2)).unwrap();
        let mut row = plasm_core::ValueRow::new();
        row.insert(
            "payload".into(),
            plasm_core::Value::String("x".repeat(MAX_EFFECT_SELECTION_BYTES / 2)),
        );
        control
            .scope(async {
                let first =
                    EffectContinuation::reserve_selection(std::slice::from_ref(&row)).unwrap();
                assert!(matches!(
                    EffectContinuation::reserve_selection(std::slice::from_ref(&row)),
                    Err(EffectContinuationError::SelectionBudget(
                        plasm_core::ValueBudgetError::BytesExceeded
                    ))
                ));
                drop(first);
                let _after_release =
                    EffectContinuation::reserve_selection(std::slice::from_ref(&row)).unwrap();
            })
            .await;
    }

    #[tokio::test]
    async fn only_the_owning_workflow_can_resume_once() {
        let workflow = Uuid::new_v4();
        let (control, mut pauses) =
            EffectContinuation::new(workflow, Duration::from_secs(2)).unwrap();
        let pending = control.checkpoint(failure(MutationDispatchStatus::RejectedWithoutEffect));
        tokio::pin!(pending);
        let mut pause = tokio::select! {
            pause = pauses.recv() => pause.unwrap(),
            _ = &mut pending => panic!("checkpoint returned before resume"),
        };
        assert_eq!(
            pause.resume(Uuid::new_v4()),
            Err(EffectContinuationError::WorkflowMismatch)
        );
        assert_eq!(pause.resume(workflow), Ok(()));
        assert_eq!(
            pause.resume(workflow),
            Err(EffectContinuationError::AlreadyResolved)
        );
        assert_eq!(pending.await, Ok(()));
    }

    #[tokio::test]
    async fn native_control_cannot_resume_another_runs_checkpoint() {
        let (owner, mut pauses) = EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
        let (other, _other_pauses) =
            EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
        let pending = owner.checkpoint(failure(MutationDispatchStatus::RejectedWithoutEffect));
        tokio::pin!(pending);
        let mut pause = tokio::select! {
            pause = pauses.recv() => pause.unwrap(),
            _ = &mut pending => panic!("checkpoint returned before resume"),
        };
        assert_eq!(
            other.resume(&mut pause),
            Err(EffectContinuationError::WorkflowMismatch)
        );
        owner.resume(&mut pause).unwrap();
        assert_eq!(pending.await, Ok(()));
    }

    #[tokio::test]
    async fn checkpoint_reports_nested_selection_accounting() {
        use plasm_core::collection_codec::{CollectionIdentity, ResultCoverage};

        let workflow = Uuid::new_v4();
        let (control, mut pauses) =
            EffectContinuation::new(workflow, Duration::from_secs(2)).unwrap();
        let selection = |count| crate::execution_failure::EffectSelectionEvidence {
            collection: CollectionIdentity {
                catalog: [1; 32],
                expression: [count as u8; 32],
                epoch: 0,
            },
            coverage: ResultCoverage::Complete,
            count,
            scope_path: vec![format!("map-{count}")],
            occurrence_path: vec![],
        };
        let outer = selection(2);
        let inner = selection(3);
        let pending = control.clone().scope(EffectContinuation::selection_scope(
            outer.clone(),
            EffectContinuation::selection_scope(
                inner.clone(),
                control.checkpoint(failure(MutationDispatchStatus::RejectedWithoutEffect)),
            ),
        ));
        tokio::pin!(pending);
        let mut pause = tokio::select! {
            pause = pauses.recv() => pause.unwrap(),
            _ = &mut pending => panic!("checkpoint returned before resume"),
        };
        assert_eq!(pause.failure.effect_selections, vec![outer, inner]);
        pause.resume(workflow).unwrap();
        assert_eq!(pending.await, Ok(()));
    }

    #[tokio::test]
    async fn unknown_or_ambiguous_dispatches_are_not_resumable() {
        let (control, mut pauses) =
            EffectContinuation::new(Uuid::new_v4(), Duration::from_secs(1)).unwrap();
        assert_eq!(
            control
                .checkpoint(failure(MutationDispatchStatus::Unresolved))
                .await,
            Err(EffectContinuationError::NotRetryable)
        );
        let mut ambiguous = failure(MutationDispatchStatus::RejectedWithoutEffect);
        let second = ambiguous.dispatches[0].clone();
        ambiguous.dispatches.push(second);
        assert_eq!(
            control.checkpoint(ambiguous).await,
            Err(EffectContinuationError::NotRetryable)
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), pauses.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn cancellation_abandons_a_pending_checkpoint() {
        let workflow = Uuid::new_v4();
        let (control, mut pauses) =
            EffectContinuation::new(workflow, Duration::from_secs(1)).unwrap();
        let pending = control.checkpoint(failure(MutationDispatchStatus::RejectedWithoutEffect));
        tokio::pin!(pending);
        let mut pause = tokio::select! {
            pause = pauses.recv() => pause.unwrap(),
            _ = &mut pending => panic!("checkpoint returned before cancellation"),
        };
        control.cancel();
        assert_eq!(pending.await, Err(EffectContinuationError::Cancelled));
        assert_eq!(
            pause.resume(workflow),
            Err(EffectContinuationError::AlreadyResolved)
        );
    }

    #[tokio::test]
    async fn expired_checkpoint_cannot_resume() {
        let workflow = Uuid::new_v4();
        let (control, mut pauses) =
            EffectContinuation::new(workflow, Duration::from_millis(20)).unwrap();
        let pending = control.checkpoint(failure(MutationDispatchStatus::RejectedWithoutEffect));
        tokio::pin!(pending);
        let mut pause = tokio::select! {
            pause = pauses.recv() => pause.unwrap(),
            _ = &mut pending => panic!("checkpoint expired before its event was received"),
        };
        assert_eq!(pending.await, Err(EffectContinuationError::Expired));
        assert_eq!(
            pause.resume(workflow),
            Err(EffectContinuationError::AlreadyResolved)
        );
    }

    #[tokio::test]
    async fn repeated_checkpoints_do_not_extend_the_lease() {
        let workflow = Uuid::new_v4();
        let (control, mut pauses) =
            EffectContinuation::new(workflow, Duration::from_millis(200)).unwrap();
        let first = control.checkpoint(failure(MutationDispatchStatus::RejectedWithoutEffect));
        tokio::pin!(first);
        let mut pause = tokio::select! {
            pause = pauses.recv() => pause.unwrap(),
            _ = &mut first => panic!("checkpoint returned before resume"),
        };
        pause.resume(workflow).unwrap();
        assert_eq!(first.await, Ok(()));
        tokio::time::sleep(Duration::from_millis(220)).await;
        assert_eq!(
            control
                .checkpoint(failure(MutationDispatchStatus::RejectedWithoutEffect))
                .await,
            Err(EffectContinuationError::Expired)
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), pauses.recv())
                .await
                .is_err()
        );
    }

    #[test]
    fn lease_is_bounded() {
        assert!(matches!(
            EffectContinuation::new(Uuid::new_v4(), Duration::ZERO),
            Err(EffectContinuationError::InvalidLease)
        ));
        assert!(matches!(
            EffectContinuation::new(
                Uuid::new_v4(),
                MAX_EFFECT_CONTINUATION_LEASE + Duration::from_secs(1)
            ),
            Err(EffectContinuationError::InvalidLease)
        ));
    }
}
