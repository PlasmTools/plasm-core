//! A bounded, workflow-owned pause in an executing effect future.
//!
//! The suspended future retains its private operands and enclosing interpreter
//! frames. A pause is only available for a single catalog-classified no-effect
//! rejection. It is not a repair certificate and does not prove selection
//! coverage or completion of the enclosing plan.

use crate::{ExecutionFailure, MutationDispatchStatus};
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Notify};
use uuid::Uuid;

/// A reviewed execution cannot keep an unattended checkpoint indefinitely.
pub const MAX_EFFECT_CONTINUATION_LEASE: Duration = Duration::from_secs(600);
/// Accounted payload retained by active nested selection frames in one run.
pub const MAX_EFFECT_SELECTION_BYTES: usize = 8 * 1024 * 1024;
/// Bound the native occurrence audit independently of row payload size.
pub const MAX_EFFECT_AUDIT_OCCURRENCES: usize = 65_536;

#[derive(Default)]
struct OccurrenceAudit {
    scopes: Vec<Vec<OccurrenceDisposition>>,
    slots: usize,
    invalid: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OccurrenceDisposition {
    Pending,
    Completed,
    PureShortCircuit,
}

#[derive(Debug, thiserror::Error)]
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
    #[error("continuation payload cannot be retained within its byte budget: {0}")]
    SelectionBudget(#[source] plasm_core::value::ValueBudgetError),
    #[error("continuation retained-state accounting failed")]
    RetainedSerialization(#[source] serde_json::Error),
    #[error("continuation execution scope has already been used")]
    ScopeAlreadyUsed,
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
    checkpoint_seen: AtomicBool,
    occurrence_audit: Mutex<OccurrenceAudit>,
    scope_kind: AtomicU8,
    scope_reentered: AtomicBool,
    completed_resumes: AtomicUsize,
    unscoped_checkpoint: AtomicBool,
    scalar_effects: Mutex<HashMap<Uuid, bool>>,
}

/// Native owner of one execution's suspension channel and lifetime.
#[derive(Clone)]
pub struct EffectContinuation {
    state: Arc<State>,
}

/// A native, single-owner witness tied to one completed continuation. It is not
/// serializable and can only be consumed by the host that owns that run.
pub struct RepairCertificate {
    state: Arc<State>,
}

impl std::fmt::Debug for RepairCertificate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RepairCertificate([native])")
    }
}

impl RepairCertificate {
    /// Consume the certificate while verifying its native run identity.
    pub fn consume_for(self, control: &EffectContinuation) -> bool {
        Arc::ptr_eq(&self.state, &control.state)
    }
}

/// One-use native capability to resume the suspended future. The failure is
/// presentation evidence; possession of this value is not a completion proof.
pub struct EffectPause {
    pub failure: ExecutionFailure,
    workflow: Uuid,
    reference: Uuid,
    resume: Option<oneshot::Sender<()>>,
}

impl EffectPause {
    pub fn native_reference(&self) -> String {
        self.reference.to_string()
    }
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

impl SelectionReservation {
    /// Charge known retained allocation capacity in addition to encoded payload.
    /// On failure the reservation and the shared budget are unchanged.
    pub fn add_capacity_bytes(&mut self, bytes: usize) -> Result<(), EffectContinuationError> {
        let Some(state) = &self.state else {
            return Ok(());
        };
        let mut used = state
            .selection_bytes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if bytes > MAX_EFFECT_SELECTION_BYTES.saturating_sub(*used) {
            return Err(EffectContinuationError::SelectionBudget(
                plasm_core::ValueBudgetError::BytesExceeded,
            ));
        }
        *used += bytes;
        self.bytes += bytes;
        Ok(())
    }
    /// Add a newly retained payload to this reservation without copying it.
    pub fn add_retained<T: serde::Serialize + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), EffectContinuationError> {
        let Some(state) = &self.state else {
            return Ok(());
        };
        struct Counter {
            remaining: usize,
            exceeded: bool,
        }
        impl std::io::Write for Counter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(remaining) = self.remaining.checked_sub(bytes.len()) {
                    self.remaining = remaining;
                    Ok(bytes.len())
                } else {
                    self.exceeded = true;
                    Err(std::io::Error::other("continuation budget exceeded"))
                }
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut used = state
            .selection_bytes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let initial = MAX_EFFECT_SELECTION_BYTES - *used;
        let mut counter = Counter {
            remaining: initial,
            exceeded: false,
        };
        if let Err(error) = serde_json::to_writer(&mut counter, value) {
            return Err(if counter.exceeded {
                EffectContinuationError::SelectionBudget(
                    plasm_core::ValueBudgetError::BytesExceeded,
                )
            } else {
                EffectContinuationError::RetainedSerialization(error)
            });
        }
        let bytes = initial - counter.remaining;
        *used += bytes;
        self.bytes += bytes;
        Ok(())
    }
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
    static AUDIT_SCOPE: Option<usize>;
}

impl EffectContinuation {
    /// Whether this capability may use a classified no-effect checkpoint.
    pub fn supports_capability(capability: &plasm_core::CapabilitySchema) -> bool {
        !capability.no_effect_rejections.is_empty()
            && !capability
                .output_schema
                .as_ref()
                .is_some_and(|output| output.idempotent && output.reconcile.is_some())
    }
    /// Whether this task belongs to a continuation-controlled execution.
    pub fn is_active() -> bool {
        CURRENT.try_with(|_| ()).is_ok()
    }
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

    /// Opaque run-local reference; it never contains operands or credentials.
    pub fn native_reference(&self) -> String {
        self.state.workflow.to_string()
    }

    /// Deadline established by the first checkpoint, never extended by retry.
    pub fn checkpoint_deadline(&self) -> Option<Instant> {
        *self
            .state
            .deadline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub fn has_checkpoint(&self) -> bool {
        self.state.checkpoint_seen.load(Ordering::Acquire)
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
                    checkpoint_seen: AtomicBool::new(false),
                    occurrence_audit: Mutex::new(OccurrenceAudit::default()),
                    scope_kind: AtomicU8::new(0),
                    scope_reentered: AtomicBool::new(false),
                    completed_resumes: AtomicUsize::new(0),
                    unscoped_checkpoint: AtomicBool::new(false),
                    scalar_effects: Mutex::new(HashMap::new()),
                }),
            },
            EffectPauseReceiver { pauses: receiver },
        ))
    }

    /// Install native control for this future. Independently spawned tasks must
    /// explicitly install their own scope.
    pub async fn scope<T>(self, future: impl Future<Output = T>) -> T {
        if self
            .state
            .scope_kind
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.state.scope_reentered.store(true, Ordering::Release);
        }
        CURRENT.scope(self, future).await
    }

    /// Own the original execution from start to finish. An `Ok` must mean the
    /// entire reviewed run succeeded, including native effect validation.
    /// No caller-supplied status or model-authored evidence is a certificate.
    pub async fn scope_for_repair<T, E>(
        self,
        future: impl Future<Output = Result<T, E>>,
    ) -> Result<(Result<T, E>, Option<RepairCertificate>), EffectContinuationError> {
        self.state
            .scope_kind
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| EffectContinuationError::ScopeAlreadyUsed)?;
        let control = self.clone();
        let result = CURRENT.scope(self, future).await;
        let certificate = if result.is_ok()
            && control.state.completed_resumes.load(Ordering::Acquire) > 0
            && !control.state.scope_reentered.load(Ordering::Acquire)
            && !control.state.unscoped_checkpoint.load(Ordering::Acquire)
            && !control.state.pending.load(Ordering::Acquire)
            && control.check_active().is_ok()
            && control.repair_audit_complete()
        {
            Some(RepairCertificate {
                state: control.state.clone(),
            })
        } else {
            None
        };
        Ok((result, certificate))
    }

    /// Reserve the aggregate payload of an active selection, including nested
    /// selections. A failed reservation changes no accounting and issues no
    /// checkpoint. This does not account for other state retained by the future.
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

    /// Charge a serializable retained payload to the same aggregate budget as
    /// selection rows. The counter discards all bytes and cannot reveal values.
    /// This measures encoded payload, not the entire allocation of the future.
    pub fn reserve_retained<T: serde::Serialize + ?Sized>(
        value: &T,
    ) -> Result<SelectionReservation, EffectContinuationError> {
        let mut reservation = Self::empty_retention();
        reservation.add_retained(value)?;
        Ok(reservation)
    }

    /// Start an incremental reservation for a growing interpreter frame.
    pub fn empty_retention() -> SelectionReservation {
        SelectionReservation {
            state: Self::current().map(|control| control.state),
            bytes: 0,
        }
    }

    /// Account host-only query continuation data without serializing it to a
    /// buffer or exposing its environment, template, URLs, or page digests.
    pub fn reserve_pagination(
        resume: &super::QueryPaginationResumeData,
    ) -> Result<SelectionReservation, EffectContinuationError> {
        Self::reserve_retained(&(
            &resume.query,
            &resume.capability_name,
            &resume.env,
            &resume.template,
            &resume.config,
            &resume.state,
            resume.state.progress_guard.retained_components(),
        ))
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
        let scope = {
            let mut audit = control
                .state
                .occurrence_audit
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match audit.slots.checked_add(selection.count) {
                Some(total)
                    if total <= MAX_EFFECT_AUDIT_OCCURRENCES
                        && audit.scopes.len() < MAX_EFFECT_AUDIT_OCCURRENCES =>
                {
                    let id = audit.scopes.len();
                    audit
                        .scopes
                        .push(vec![OccurrenceDisposition::Pending; selection.count]);
                    audit.slots = total;
                    Some(id)
                }
                _ => {
                    audit.invalid = true;
                    None
                }
            }
        };
        let mut selections = SELECTIONS.try_with(Clone::clone).unwrap_or_default();
        selections.push(selection);
        SELECTIONS
            .scope(selections, AUDIT_SCOPE.scope(scope, future))
            .await
    }

    /// Record a scheduler-completed occurrence in the innermost native scope.
    /// Missing, repeated, or out-of-range positions invalidate the audit.
    pub fn complete_selection_occurrence(index: usize) {
        let Some(control) = Self::current() else {
            return;
        };
        let scope = AUDIT_SCOPE.try_with(|value| *value).ok().flatten();
        let mut audit = control
            .state
            .occurrence_audit
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let completed = scope
            .and_then(|scope| audit.scopes.get_mut(scope))
            .and_then(|scope| scope.get_mut(index));
        match completed {
            Some(value) if *value == OccurrenceDisposition::Pending => {
                *value = OccurrenceDisposition::Completed
            }
            _ => audit.invalid = true,
        }
    }

    /// Record positions skipped by a statically validated pure short circuit.
    /// The scheduler may call this only for a scope whose effects cannot write.
    pub fn complete_pure_short_circuit(first_skipped: usize) {
        let Some(control) = Self::current() else {
            return;
        };
        let scope = AUDIT_SCOPE.try_with(|value| *value).ok().flatten();
        let mut audit = control
            .state
            .occurrence_audit
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let positions = scope
            .and_then(|scope| audit.scopes.get_mut(scope))
            .and_then(|scope| scope.get_mut(first_skipped..));
        match positions {
            Some(positions)
                if positions
                    .iter()
                    .all(|value| *value == OccurrenceDisposition::Pending) =>
            {
                positions.fill(OccurrenceDisposition::PureShortCircuit);
            }
            _ => audit.invalid = true,
        }
    }

    /// Check native scheduler correspondence for every entered selection.
    /// This is not a certificate: successful execution and effect outcomes
    /// must also be established by the owning run.
    pub fn selected_occurrences_completed(&self) -> bool {
        let audit = self
            .state
            .occurrence_audit
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        !audit.invalid
            && !audit.scopes.is_empty()
            && audit.scopes.iter().all(|scope| {
                scope
                    .iter()
                    .all(|done| *done != OccurrenceDisposition::Pending)
            })
    }

    /// Conservative run-wide coverage fact. This is necessary but not
    /// sufficient for a repair certificate: it contains no occurrence proof.
    pub fn selections_have_complete_coverage(&self) -> bool {
        self.state.saw_selection.load(Ordering::Acquire)
            && !self.state.incomplete_selection.load(Ordering::Acquire)
    }

    fn repair_audit_complete(&self) -> bool {
        let scalar = self
            .state
            .scalar_effects
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let has_selections = self.state.saw_selection.load(Ordering::Acquire);
        (has_selections || !scalar.is_empty())
            && (!has_selections
                || (self.selections_have_complete_coverage()
                    && self.selected_occurrences_completed()))
            && scalar.values().all(|complete| *complete)
    }

    /// The same resolved scalar invocation received a successful response.
    /// The enclosing reviewed run must still finish successfully to certify it.
    pub(crate) fn scalar_dispatch_completed(&self) {
        let Some(effect) = super::resolved_effect::current_effect() else {
            return;
        };
        self.record_scalar_success(effect.invocation);
    }

    fn record_scalar_success(&self, invocation: Uuid) {
        let mut scalar = self
            .state
            .scalar_effects
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(done) = scalar.get_mut(&invocation) {
            *done = true;
        }
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
        let selections = SELECTIONS.try_with(Clone::clone).unwrap_or_default();
        if !selections.is_empty() {
            for selection in selections {
                failure = failure.with_effect_selection(selection);
            }
        } else {
            let scalar = failure
                .dispatches
                .first()
                .and_then(|dispatch| dispatch.resolved_effect.as_ref())
                .filter(|effect| {
                    effect.origin.as_ref().is_some_and(|origin| {
                        origin.selection.is_none()
                            && origin.occurrence_path.is_empty()
                            && origin.inputs.is_empty()
                    })
                });
            let mut audit = self
                .state
                .scalar_effects
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match scalar {
                Some(effect)
                    if audit.len() < MAX_EFFECT_AUDIT_OCCURRENCES
                        || audit.contains_key(&effect.invocation) =>
                {
                    if audit.get(&effect.invocation) == Some(&true) {
                        self.state
                            .unscoped_checkpoint
                            .store(true, Ordering::Release);
                    }
                    audit.entry(effect.invocation).or_insert(false);
                }
                _ => self
                    .state
                    .unscoped_checkpoint
                    .store(true, Ordering::Release),
            }
        }
        // Failure evidence is held by the pause channel even after the caller
        // has returned. Include private provider evidence without exposing it.
        let mut retained_failure = SelectionReservation {
            state: Some(self.state.clone()),
            bytes: 0,
        };
        retained_failure.add_retained(&failure.retention_view())?;
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
        self.state.checkpoint_seen.store(true, Ordering::Release);
        let wait = async {
            self.state
                .pauses
                .send(EffectPause {
                    failure,
                    workflow: self.state.workflow,
                    reference: Uuid::new_v4(),
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
            if result.is_ok() {
                self.state.completed_resumes.fetch_add(1, Ordering::AcqRel);
            }
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
    async fn occurrence_audit_requires_each_original_position_once() {
        use plasm_core::collection_codec::{CollectionIdentity, ResultCoverage};
        let selection = crate::execution_failure::EffectSelectionEvidence {
            collection: CollectionIdentity {
                catalog: [1; 32],
                expression: [2; 32],
                epoch: 0,
            },
            coverage: ResultCoverage::Complete,
            count: 3,
            scope_path: vec!["map".into()],
            occurrence_path: vec![],
        };
        let (control, _pauses) = EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
        control
            .clone()
            .scope(EffectContinuation::selection_scope(
                selection.clone(),
                async {
                    EffectContinuation::complete_selection_occurrence(0);
                    EffectContinuation::complete_selection_occurrence(1);
                    assert!(!control.selected_occurrences_completed());
                    EffectContinuation::complete_selection_occurrence(2);
                },
            ))
            .await;
        assert!(control.selected_occurrences_completed());

        let (duplicate, _pauses) = EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
        duplicate
            .clone()
            .scope(EffectContinuation::selection_scope(selection, async {
                EffectContinuation::complete_selection_occurrence(0);
                EffectContinuation::complete_selection_occurrence(0);
                EffectContinuation::complete_selection_occurrence(1);
                EffectContinuation::complete_selection_occurrence(2);
            }))
            .await;
        assert!(!duplicate.selected_occurrences_completed());

        let (short_circuit, _pauses) =
            EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
        let short_selection = crate::execution_failure::EffectSelectionEvidence {
            collection: CollectionIdentity {
                catalog: [1; 32],
                expression: [6; 32],
                epoch: 0,
            },
            coverage: ResultCoverage::Complete,
            count: 3,
            scope_path: vec!["pure".into()],
            occurrence_path: vec![],
        };
        short_circuit
            .clone()
            .scope(EffectContinuation::selection_scope(
                short_selection,
                async {
                    EffectContinuation::complete_selection_occurrence(0);
                    EffectContinuation::complete_pure_short_circuit(1);
                },
            ))
            .await;
        assert!(short_circuit.selected_occurrences_completed());

        let (nested, _pauses) = EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
        let outer = crate::execution_failure::EffectSelectionEvidence {
            collection: CollectionIdentity {
                catalog: [1; 32],
                expression: [3; 32],
                epoch: 0,
            },
            coverage: ResultCoverage::Complete,
            count: 1,
            scope_path: vec!["outer".into()],
            occurrence_path: vec![],
        };
        let inner = crate::execution_failure::EffectSelectionEvidence {
            collection: CollectionIdentity {
                catalog: [1; 32],
                expression: [4; 32],
                epoch: 0,
            },
            coverage: ResultCoverage::Complete,
            count: 2,
            scope_path: vec!["outer".into(), "inner".into()],
            occurrence_path: vec![0],
        };
        nested
            .clone()
            .scope(EffectContinuation::selection_scope(outer, async {
                EffectContinuation::selection_scope(inner, async {
                    EffectContinuation::complete_selection_occurrence(0);
                    EffectContinuation::complete_selection_occurrence(1);
                })
                .await;
                EffectContinuation::complete_selection_occurrence(0);
            }))
            .await;
        assert!(nested.selected_occurrences_completed());

        let (oversized, _pauses) = EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
        let too_many = crate::execution_failure::EffectSelectionEvidence {
            collection: CollectionIdentity {
                catalog: [1; 32],
                expression: [5; 32],
                epoch: 0,
            },
            coverage: ResultCoverage::Complete,
            count: MAX_EFFECT_AUDIT_OCCURRENCES + 1,
            scope_path: vec!["oversized".into()],
            occurrence_path: vec![],
        };
        oversized
            .clone()
            .scope(EffectContinuation::selection_scope(too_many, async {}))
            .await;
        assert!(!oversized.selected_occurrences_completed());
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
    async fn oversized_checkpoint_failure_never_enters_the_pause_channel() {
        let (control, mut pauses) = EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
        let mut failure = failure(MutationDispatchStatus::RejectedWithoutEffect);
        failure = failure.at("x".repeat(MAX_EFFECT_SELECTION_BYTES), Vec::new());
        assert!(matches!(
            control.checkpoint(failure).await,
            Err(EffectContinuationError::SelectionBudget(
                plasm_core::ValueBudgetError::BytesExceeded
            ))
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), pauses.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn materialized_payload_and_selection_share_the_retention_budget() {
        let (control, _pauses) = EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
        let payload = "x".repeat(MAX_EFFECT_SELECTION_BYTES / 2);
        let mut row = plasm_core::ValueRow::new();
        row.insert("payload".into(), plasm_core::Value::String(payload.clone()));
        control
            .scope(async {
                let retained = EffectContinuation::reserve_retained(&payload).unwrap();
                assert!(matches!(
                    EffectContinuation::reserve_selection(&[row.clone()]),
                    Err(EffectContinuationError::SelectionBudget(
                        plasm_core::ValueBudgetError::BytesExceeded
                    ))
                ));
                drop(retained);
                let selection = EffectContinuation::reserve_selection(&[row]).unwrap();
                drop(selection);
                assert!(EffectContinuation::reserve_retained(&payload).is_ok());
            })
            .await;
    }

    #[tokio::test]
    async fn host_only_pagination_environment_is_charged_without_wire_serialization() {
        let (control, _pauses) = EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
        let mut resume = super::super::QueryPaginationResumeData {
            query: plasm_core::QueryExpr::all("Item"),
            capability_name: "items".into(),
            env: Default::default(),
            template: plasm_compile::CapabilityTemplate::View(plasm_compile::ViewTemplate {
                view: "items".into(),
            }),
            config: serde_json::from_value(serde_json::json!({})).unwrap(),
            state: super::super::QueryPaginationState {
                progress_guard: Default::default(),
                mcp_collection: Default::default(),
                param_values: Vec::new(),
                pages_observed: 0,
                next_absolute_url: None,
                last_requested_limit: 1,
                from_block: None,
                final_to_block: None,
                last_requested_to_block: None,
            },
        };
        resume.env.insert(
            "private".into(),
            plasm_core::Value::String("x".repeat(MAX_EFFECT_SELECTION_BYTES)),
        );
        control
            .scope(async {
                assert!(matches!(
                    EffectContinuation::reserve_pagination(&resume),
                    Err(EffectContinuationError::SelectionBudget(
                        plasm_core::ValueBudgetError::BytesExceeded
                    ))
                ));
            })
            .await;
    }

    #[tokio::test]
    async fn retained_collection_charges_unselected_backing_rows_and_delivery_source() {
        use plasm_core::collection_codec::{CollectionIdentity, Observation};
        let row = |id: &str| {
            crate::CachedEntity::from_decoded(
                plasm_core::Ref::new("Row", id),
                Default::default(),
                Default::default(),
                0,
                crate::EntityCompleteness::Complete,
            )
        };
        let huge = "z".repeat(MAX_EFFECT_SELECTION_BYTES);
        let collection = super::super::ExecutionCollection::observe(
            CollectionIdentity {
                catalog: [1; 32],
                expression: [2; 32],
                epoch: 0,
            },
            vec![row("small"), row(&huge)],
            Observation::ExactOutput { decoded: 2 },
        )
        .unwrap();
        let capture = collection.capture(0).unwrap();
        let delivery = collection.delivery(0..1).unwrap();
        assert!(serde_json::to_vec(&capture).unwrap().len() < 4096);
        assert!(serde_json::to_vec(&delivery).unwrap().len() < 4096);
        let (control, _pauses) = EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
        control
            .scope(async {
                for collection in [&capture, &delivery] {
                    assert!(matches!(
                        EffectContinuation::reserve_retained(&collection.retention_view()),
                        Err(EffectContinuationError::SelectionBudget(
                            plasm_core::ValueBudgetError::BytesExceeded
                        ))
                    ));
                }
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
        assert!(matches!(
            pause.resume(Uuid::new_v4()),
            Err(EffectContinuationError::WorkflowMismatch)
        ));
        assert!(pause.resume(workflow).is_ok());
        assert!(matches!(
            pause.resume(workflow),
            Err(EffectContinuationError::AlreadyResolved)
        ));
        assert!(pending.await.is_ok());
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
        assert!(matches!(
            other.resume(&mut pause),
            Err(EffectContinuationError::WorkflowMismatch)
        ));
        owner.resume(&mut pause).unwrap();
        assert!(pending.await.is_ok());
    }

    #[tokio::test]
    async fn native_certificate_requires_the_original_complete_resumed_scope() {
        use plasm_core::collection_codec::{CollectionIdentity, ResultCoverage};
        for coverage in [
            ResultCoverage::Complete,
            ResultCoverage::Partial,
            ResultCoverage::Unknown,
        ] {
            let (owner, mut pauses) =
                EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
            let (other, _other_pauses) =
                EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
            let selection = crate::execution_failure::EffectSelectionEvidence {
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
            let worker = owner.clone();
            let pending = owner
                .clone()
                .scope_for_repair(EffectContinuation::selection_scope(selection, async move {
                    worker
                        .checkpoint(failure(MutationDispatchStatus::RejectedWithoutEffect))
                        .await?;
                    EffectContinuation::complete_selection_occurrence(0);
                    Ok::<_, EffectContinuationError>(())
                }));
            tokio::pin!(pending);
            let mut pause = tokio::select! {
                pause = pauses.recv() => pause.unwrap(),
                _ = &mut pending => panic!("scope returned before resume"),
            };
            owner.resume(&mut pause).unwrap();
            let (outcome, certificate) = pending.await.unwrap();
            assert!(outcome.is_ok());
            if coverage == ResultCoverage::Complete {
                let certificate = certificate.expect("complete original scope has a certificate");
                assert!(!certificate.consume_for(&other));
            } else {
                assert!(certificate.is_none());
            }
            assert!(matches!(
                owner
                    .clone()
                    .scope_for_repair(async { Ok::<_, ()>(()) })
                    .await,
                Err(EffectContinuationError::ScopeAlreadyUsed)
            ));
        }
    }

    #[tokio::test]
    async fn certificate_cannot_be_created_by_a_second_scope() {
        let (control, _pauses) = EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
        control.clone().scope(async {}).await;
        assert!(matches!(
            control.scope_for_repair(async { Ok::<_, ()>(()) }).await,
            Err(EffectContinuationError::ScopeAlreadyUsed)
        ));
    }

    #[tokio::test]
    async fn certificate_requires_success_and_every_occurrence() {
        use plasm_core::collection_codec::{CollectionIdentity, ResultCoverage};
        for (finish_occurrence, succeed) in [(true, false), (false, true)] {
            let (control, mut pauses) =
                EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
            let selection = crate::execution_failure::EffectSelectionEvidence {
                collection: CollectionIdentity {
                    catalog: [1; 32],
                    expression: [2; 32],
                    epoch: 0,
                },
                coverage: ResultCoverage::Complete,
                count: 1,
                scope_path: vec!["map".into()],
                occurrence_path: vec![],
            };
            let worker = control.clone();
            let pending = control
                .clone()
                .scope_for_repair(EffectContinuation::selection_scope(selection, async move {
                    worker
                        .checkpoint(failure(MutationDispatchStatus::RejectedWithoutEffect))
                        .await
                        .unwrap();
                    if finish_occurrence {
                        EffectContinuation::complete_selection_occurrence(0);
                    }
                    if succeed {
                        Ok(())
                    } else {
                        Err(())
                    }
                }));
            tokio::pin!(pending);
            let mut pause = tokio::select! {
                pause = pauses.recv() => pause.unwrap(),
                _ = &mut pending => panic!("scope returned before resume"),
            };
            control.resume(&mut pause).unwrap();
            let (_result, certificate) = pending.await.unwrap();
            assert!(certificate.is_none());
        }
    }

    #[tokio::test]
    async fn unrelated_selection_cannot_certify_an_unscoped_checkpoint() {
        use plasm_core::collection_codec::{CollectionIdentity, ResultCoverage};
        let (control, mut pauses) = EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
        let selection = crate::execution_failure::EffectSelectionEvidence {
            collection: CollectionIdentity {
                catalog: [1; 32],
                expression: [2; 32],
                epoch: 0,
            },
            coverage: ResultCoverage::Complete,
            count: 1,
            scope_path: vec!["unrelated".into()],
            occurrence_path: vec![],
        };
        let worker = control.clone();
        let pending = control.clone().scope_for_repair(async move {
            EffectContinuation::selection_scope(selection, async {
                EffectContinuation::complete_selection_occurrence(0);
            })
            .await;
            worker
                .checkpoint(failure(MutationDispatchStatus::RejectedWithoutEffect))
                .await
        });
        tokio::pin!(pending);
        let mut pause = tokio::select! {
            pause = pauses.recv() => pause.unwrap(),
            _ = &mut pending => panic!("scope returned before resume"),
        };
        control.resume(&mut pause).unwrap();
        let (result, certificate) = pending.await.unwrap();
        assert!(result.is_ok());
        assert!(certificate.is_none());
    }

    #[tokio::test]
    async fn scalar_certificate_requires_an_attributed_root_invocation() {
        use crate::execution::{
            EffectInputReference, EffectOrigin, EffectTargetKind, ResolvedEffectEvidence,
        };
        use plasm_core::collection_codec::CollectionIdentity;
        for (nested, input, expected) in [
            (false, false, true),
            (true, false, false),
            (false, true, false),
        ] {
            let (control, mut pauses) =
                EffectContinuation::new_native(Duration::from_secs(2)).unwrap();
            let invocation = Uuid::new_v4();
            let mut rejected = failure(MutationDispatchStatus::RejectedWithoutEffect);
            let operation = rejected.dispatches[0].operation.clone();
            rejected.dispatches[0].resolved_effect = Some(ResolvedEffectEvidence {
                invocation,
                operation,
                entity: "Item".into(),
                target_kind: EffectTargetKind::Create,
                origin: Some(EffectOrigin {
                    execution: Uuid::new_v4(),
                    scope_path: vec!["write".into()],
                    occurrence_path: if nested { vec![0] } else { vec![] },
                    selection: None,
                    inputs: if input {
                        vec![EffectInputReference {
                            binding: "input".into(),
                            producer: "source".into(),
                            collection: CollectionIdentity {
                                catalog: [1; 32],
                                expression: [2; 32],
                                epoch: 0,
                            },
                        }]
                    } else {
                        vec![]
                    },
                }),
            });
            let worker = control.clone();
            let pending = control.clone().scope_for_repair(async move {
                worker.checkpoint(rejected).await.unwrap();
                worker.record_scalar_success(invocation);
                Ok::<_, ()>(())
            });
            tokio::pin!(pending);
            let mut pause = tokio::select! {
                pause = pauses.recv() => pause.unwrap(),
                _ = &mut pending => panic!("scope returned before resume"),
            };
            control.resume(&mut pause).unwrap();
            let (outcome, certificate) = pending.await.unwrap();
            assert!(outcome.is_ok());
            assert_eq!(certificate.is_some(), expected);
        }
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
        assert!(pending.await.is_ok());
    }

    #[tokio::test]
    async fn unknown_or_ambiguous_dispatches_are_not_resumable() {
        let (control, mut pauses) =
            EffectContinuation::new(Uuid::new_v4(), Duration::from_secs(1)).unwrap();
        assert!(matches!(
            control
                .checkpoint(failure(MutationDispatchStatus::Unresolved))
                .await,
            Err(EffectContinuationError::NotRetryable)
        ));
        let mut ambiguous = failure(MutationDispatchStatus::RejectedWithoutEffect);
        let second = ambiguous.dispatches[0].clone();
        ambiguous.dispatches.push(second);
        assert!(matches!(
            control.checkpoint(ambiguous).await,
            Err(EffectContinuationError::NotRetryable)
        ));
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
        assert!(matches!(
            pending.await,
            Err(EffectContinuationError::Cancelled)
        ));
        assert!(matches!(
            pause.resume(workflow),
            Err(EffectContinuationError::AlreadyResolved)
        ));
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
        assert!(matches!(
            pending.await,
            Err(EffectContinuationError::Expired)
        ));
        assert!(matches!(
            pause.resume(workflow),
            Err(EffectContinuationError::AlreadyResolved)
        ));
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
        assert!(first.await.is_ok());
        tokio::time::sleep(Duration::from_millis(220)).await;
        assert!(matches!(
            control
                .checkpoint(failure(MutationDispatchStatus::RejectedWithoutEffect))
                .await,
            Err(EffectContinuationError::Expired)
        ));
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
