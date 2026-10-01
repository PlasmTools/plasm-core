//! Occurrence-addressed execution state. Live deltas are backed by a recoverable operation snapshot.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepAddress {
    pub scope_path: Vec<String>,
    pub local_step: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrencePhase {
    Running,
    Done,
    Failed,
    Cancelled,
    NotInvoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStage {
    Materializing,
    Executing,
    Validating,
    AwaitingHost,
    Resuming,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccurrenceProgress {
    pub address: StepAddress,
    pub occurrence_path: Vec<usize>,
    pub phase: OccurrencePhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<ExecutionStage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_fingerprints: Vec<String>,
    /// Acknowledged operations owned by this executing step, never rolled up
    /// again on enclosing map occurrences.
    #[serde(
        default,
        skip_serializing_if = "plasm_runtime::OperationLedger::is_empty"
    )]
    pub operations: plasm_runtime::OperationLedger,
    /// Transport evidence, including unresolved writes; not another acknowledgement count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutation_dispatches: Vec<plasm_runtime::MutationDispatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl OccurrenceProgress {
    pub fn running(
        scope_path: Vec<String>,
        local_step: String,
        occurrence_path: Vec<usize>,
    ) -> Self {
        Self {
            address: StepAddress {
                scope_path,
                local_step,
            },
            occurrence_path,
            phase: OccurrencePhase::Running,
            stage: None,
            rows: None,
            artifact_uri: None,
            request_fingerprints: Vec::new(),
            operations: plasm_runtime::OperationLedger::empty(),
            mutation_dispatches: Vec::new(),
            error: None,
        }
    }
    pub fn terminal(&self) -> bool {
        self.phase != OccurrencePhase::Running
    }
}

/// Replacement preserves insertion order. Late receipts may enrich failed or
/// cancelled occurrences, but may never resurrect their execution state.
pub(crate) fn update(snapshot: &mut Vec<OccurrenceProgress>, mut next: OccurrenceProgress) -> bool {
    if let Some(old) = snapshot
        .iter_mut()
        .find(|old| old.address == next.address && old.occurrence_path == next.occurrence_path)
    {
        if old.terminal() {
            if matches!(
                old.phase,
                OccurrencePhase::Cancelled | OccurrencePhase::Failed
            ) {
                let before = old.clone();
                // Journal snapshots are append-only with stable dispatch identities.
                if next.mutation_dispatches.len() >= old.mutation_dispatches.len()
                    && old
                        .mutation_dispatches
                        .iter()
                        .zip(&next.mutation_dispatches)
                        .all(|(a, b)| {
                            a.operation == b.operation
                                && a.request_fingerprint == b.request_fingerprint
                        })
                {
                    for (index, dispatch) in next.mutation_dispatches.into_iter().enumerate() {
                        if index == old.mutation_dispatches.len() {
                            old.mutation_dispatches.push(dispatch);
                        } else if dispatch.status
                            == plasm_runtime::MutationDispatchStatus::ResponseReceived
                        {
                            old.mutation_dispatches[index] = dispatch;
                        }
                    }
                }
                if old.operations.is_empty() {
                    old.operations = next.operations;
                }
                if old.request_fingerprints.is_empty() {
                    old.request_fingerprints = next.request_fingerprints;
                }
                old.rows = old.rows.or(next.rows);
                old.artifact_uri = old.artifact_uri.clone().or(next.artifact_uri);
                return *old != before;
            }
            return false;
        }
        if *old == next {
            return false;
        }
        if next.mutation_dispatches.is_empty() {
            next.mutation_dispatches = old.mutation_dispatches.clone();
        }
        // Stage/terminal updates cannot erase an already published host receipt.
        if next.operations.is_empty() {
            next.operations = old.operations.clone();
        }
        if next.request_fingerprints.is_empty() {
            next.request_fingerprints = old.request_fingerprints.clone();
        }
        next.rows = next.rows.or(old.rows);
        next.artifact_uri = next.artifact_uri.or_else(|| old.artifact_uri.clone());
        *old = next;
    } else {
        snapshot.push(next);
    }
    true
}

/// A dropped in-flight future must leave a terminal occurrence, including host cancellation.
pub(crate) struct OccurrenceGuard {
    scope: Option<crate::operation::ExecutionScope>,
    event: OccurrenceProgress,
    journal: plasm_runtime::MutationJournal,
}
impl OccurrenceGuard {
    pub fn new(
        scope: Option<&crate::operation::ExecutionScope>,
        event: OccurrenceProgress,
    ) -> Self {
        if let Some(scope) = scope {
            scope.report_occurrence(event.clone());
        }
        let progress_scope = scope.cloned();
        let progress_event = event.clone();
        let journal = plasm_runtime::MutationJournal::observed(move |dispatches| {
            if let Some(scope) = &progress_scope {
                let mut event = progress_event.clone();
                event.mutation_dispatches = dispatches;
                scope.report_occurrence(event);
            }
        });
        Self {
            scope: scope.cloned(),
            event,
            journal,
        }
    }
    pub fn mutation_journal(&self) -> plasm_runtime::MutationJournal {
        self.journal.clone()
    }
    pub fn finish(
        &mut self,
        phase: OccurrencePhase,
        rows: Option<usize>,
        artifact_uri: Option<String>,
        fingerprints: Vec<String>,
        error: Option<String>,
    ) {
        self.event.mutation_dispatches = self.journal.snapshot();
        self.event.phase = phase;
        self.event.stage = None;
        self.event.rows = rows;
        self.event.artifact_uri = artifact_uri;
        self.event.request_fingerprints = fingerprints;
        self.event.error = error;
        if let Some(scope) = &self.scope {
            scope.report_occurrence(self.event.clone());
        }
    }
    pub fn record_result(
        &mut self,
        rows: usize,
        artifact_uri: Option<String>,
        fingerprints: Vec<String>,
        operations: plasm_runtime::OperationLedger,
    ) {
        self.event.rows = Some(rows);
        self.event.artifact_uri = artifact_uri;
        self.event.request_fingerprints = fingerprints;
        self.event.operations = operations;
    }
    pub fn fail(&mut self, error: String) {
        let phase = if self.scope.as_ref().is_some_and(|s| s.check().is_err()) {
            OccurrencePhase::Cancelled
        } else {
            OccurrencePhase::Failed
        };
        self.finish(
            phase,
            self.event.rows,
            self.event.artifact_uri.clone(),
            self.event.request_fingerprints.clone(),
            Some(error),
        );
    }
}
impl Drop for OccurrenceGuard {
    fn drop(&mut self) {
        if !self.event.terminal() {
            self.fail("execution interrupted".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_runtime::{MutationDispatch, MutationDispatchStatus, OperationIdentity};

    #[test]
    fn terminal_receipts_enrich_without_resurrection_or_regression() {
        for phase in [OccurrencePhase::Cancelled, OccurrencePhase::Failed] {
            let mut event = OccurrenceProgress::running(vec![], "write".into(), vec![1]);
            event.mutation_dispatches.push(MutationDispatch {
                operation: OperationIdentity {
                    entry_id: "fixture".into(),
                    capability: "create".into(),
                },
                request_fingerprint: "request".into(),
                status: MutationDispatchStatus::Unresolved,
            });
            let mut snapshot = vec![event.clone()];
            snapshot[0].phase = phase;
            snapshot[0].error = Some("stopped".into());
            let stale = event.clone();
            event.mutation_dispatches[0].status = MutationDispatchStatus::ResponseReceived;
            event.rows = Some(1);
            assert!(update(&mut snapshot, event.clone()));
            assert_eq!(snapshot[0].phase, phase);
            assert_eq!(snapshot[0].error.as_deref(), Some("stopped"));
            assert_eq!(snapshot[0].rows, Some(1));
            assert!(!update(&mut snapshot, stale));
            assert!(!update(&mut snapshot, event));
            assert_eq!(
                snapshot[0].mutation_dispatches[0].status,
                MutationDispatchStatus::ResponseReceived
            );
        }
    }
}
