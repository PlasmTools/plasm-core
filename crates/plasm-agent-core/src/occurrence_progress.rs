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
            error: None,
        }
    }
    pub fn terminal(&self) -> bool {
        self.phase != OccurrencePhase::Running
    }
}

/// Replacement preserves insertion order; terminal states cannot be resurrected by late workers.
pub(crate) fn update(snapshot: &mut Vec<OccurrenceProgress>, next: OccurrenceProgress) -> bool {
    if let Some(old) = snapshot
        .iter_mut()
        .find(|old| old.address == next.address && old.occurrence_path == next.occurrence_path)
    {
        if old.terminal() || *old == next {
            return false;
        }
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
}
impl OccurrenceGuard {
    pub fn new(
        scope: Option<&crate::operation::ExecutionScope>,
        event: OccurrenceProgress,
    ) -> Self {
        if let Some(scope) = scope {
            scope.report_occurrence(event.clone());
        }
        Self {
            scope: scope.cloned(),
            event,
        }
    }
    pub fn finish(
        &mut self,
        phase: OccurrencePhase,
        rows: Option<usize>,
        artifact_uri: Option<String>,
        fingerprints: Vec<String>,
        error: Option<String>,
    ) {
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
    pub fn fail(&mut self, error: String) {
        let phase = if self.scope.as_ref().is_some_and(|s| s.check().is_err()) {
            OccurrencePhase::Cancelled
        } else {
            OccurrencePhase::Failed
        };
        self.finish(phase, None, None, Vec::new(), Some(error));
    }
}
impl Drop for OccurrenceGuard {
    fn drop(&mut self) {
        if !self.event.terminal() {
            self.fail("execution interrupted".into());
        }
    }
}
