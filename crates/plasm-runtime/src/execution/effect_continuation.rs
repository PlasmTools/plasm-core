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
}

struct State {
    workflow: Uuid,
    lease: Duration,
    deadline: Mutex<Option<Instant>>,
    pending: AtomicBool,
    cancelled: AtomicBool,
    cancel_notify: Notify,
    pauses: mpsc::Sender<EffectPause>,
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
}

impl EffectContinuation {
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
                }),
            },
            EffectPauseReceiver { pauses: receiver },
        ))
    }

    /// Install the native control for this future and its nested execution.
    pub async fn scope<T>(self, future: impl Future<Output = T>) -> T {
        CURRENT.scope(self, future).await
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
        failure: ExecutionFailure,
    ) -> Result<(), EffectContinuationError> {
        if failure.effects_unresolved
            || failure.effects.iter().any(|effect| effect.completed > 0)
            || !matches!(failure.dispatches.as_slice(), [dispatch] if dispatch.status == MutationDispatchStatus::RejectedWithoutEffect)
        {
            return Err(EffectContinuationError::NotRetryable);
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
