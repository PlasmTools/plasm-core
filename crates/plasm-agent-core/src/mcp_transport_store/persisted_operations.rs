//! Cross-pod durable projection for async operation handles (metadata only; executors stay pod-local).

use std::collections::HashMap;

use plasm_core::OperationHandle;

use crate::operation::{OperationPhase, OperationProgress, OperationState};
use crate::plan_dry_display::PlanDryVerdict;

/// Max terminal operation rows retained in the session descriptor JSON.
pub const MAX_TERMINAL_OPS_PERSIST: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistedOperationPhase {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl From<OperationPhase> for PersistedOperationPhase {
    fn from(p: OperationPhase) -> Self {
        match p {
            OperationPhase::Running => Self::Running,
            OperationPhase::Succeeded => Self::Succeeded,
            OperationPhase::Failed => Self::Failed,
            OperationPhase::Cancelled => Self::Cancelled,
        }
    }
}

impl PersistedOperationPhase {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PersistedOperationProgress {
    pub step: u32,
    pub step_total: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub rows_materialized: u64,
}

impl From<&OperationProgress> for PersistedOperationProgress {
    fn from(p: &OperationProgress) -> Self {
        Self {
            step: p.step,
            step_total: p.step_total,
            label: p.label.clone(),
            rows_materialized: p.rows_materialized,
        }
    }
}

impl From<PersistedOperationProgress> for OperationProgress {
    fn from(p: PersistedOperationProgress) -> Self {
        Self {
            step: p.step,
            step_total: p.step_total,
            label: p.label,
            rows_materialized: p.rows_materialized,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PersistedOperationDescriptor {
    pub handle: String,
    pub phase: PersistedOperationPhase,
    pub progress: PersistedOperationProgress,
    pub started_at_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_commit_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_verdict: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub display_map: HashMap<String, String>,
    #[serde(default)]
    pub agent_seq: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_last_line: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OperationPersistSnapshot {
    pub operations: Vec<PersistedOperationDescriptor>,
    pub operation_handle_next: u64,
}

#[derive(Clone, Debug)]
pub enum OperationPersistPatch {
    Upsert(PersistedOperationDescriptor),
    Progress {
        handle: String,
        progress: PersistedOperationProgress,
        agent_seq: u64,
        agent_last_line: String,
    },
}

pub fn operation_seq_from_wire(handle: &str) -> Option<u64> {
    let rest = if let Some((_, suffix)) = handle.rsplit_once("_o") {
        suffix
    } else {
        handle.strip_prefix('o')?
    };
    rest.parse().ok()
}

pub fn max_operation_seq(handles: &[PersistedOperationDescriptor]) -> u64 {
    handles
        .iter()
        .filter_map(|d| operation_seq_from_wire(&d.handle))
        .max()
        .unwrap_or(0)
}

pub fn descriptor_from_operation_state(
    handle: &OperationHandle,
    op: &OperationState,
    started_at_unix: u64,
) -> PersistedOperationDescriptor {
    PersistedOperationDescriptor {
        handle: handle.as_str().to_string(),
        phase: op.phase.into(),
        progress: PersistedOperationProgress::from(&op.progress),
        started_at_unix,
        error: op.error.clone(),
        run_artifact_id: op.run_artifact_id.clone(),
        plan_commit_ref: op.plan_commit_ref.as_ref().map(|r| r.as_str().to_string()),
        dry_verdict: op.dry_verdict.map(|v| match v {
            PlanDryVerdict::Ok => "ok".to_string(),
            PlanDryVerdict::Review => "review".to_string(),
            PlanDryVerdict::Deny => "deny".to_string(),
        }),
        display_map: op.display_map.clone(),
        agent_seq: op.agent_emit.seq,
        agent_last_line: op.agent_emit.last_line.clone(),
    }
}

pub fn prune_terminal_operations(
    mut ops: Vec<PersistedOperationDescriptor>,
) -> Vec<PersistedOperationDescriptor> {
    let terminal_count = ops.iter().filter(|o| o.phase.is_terminal()).count();
    if terminal_count <= MAX_TERMINAL_OPS_PERSIST {
        return ops;
    }
    let drop = terminal_count - MAX_TERMINAL_OPS_PERSIST;
    let mut dropped = 0usize;
    ops.retain(|o| {
        if o.phase.is_terminal() && dropped < drop {
            dropped += 1;
            return false;
        }
        true
    });
    ops
}

pub fn merge_operation_patch(
    operations: &mut Vec<PersistedOperationDescriptor>,
    operation_handle_next: &mut u64,
    patch: OperationPersistPatch,
) {
    match patch {
        OperationPersistPatch::Upsert(desc) => {
            if let Some(seq) = operation_seq_from_wire(&desc.handle) {
                *operation_handle_next = (*operation_handle_next).max(seq.saturating_add(1));
            }
            if let Some(existing) = operations.iter_mut().find(|o| o.handle == desc.handle) {
                *existing = desc;
            } else {
                operations.push(desc);
            }
        }
        OperationPersistPatch::Progress {
            handle,
            progress,
            agent_seq,
            agent_last_line,
        } => {
            if let Some(existing) = operations.iter_mut().find(|o| o.handle == handle) {
                existing.progress = progress;
                existing.agent_seq = agent_seq;
                existing.agent_last_line = agent_last_line;
            }
        }
    }
    *operations = prune_terminal_operations(std::mem::take(operations));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_upsert_and_prune_terminal() {
        let mut ops = Vec::new();
        let mut next = 1u64;
        for i in 1..=40 {
            merge_operation_patch(
                &mut ops,
                &mut next,
                OperationPersistPatch::Upsert(PersistedOperationDescriptor {
                    handle: format!("o{i}"),
                    phase: PersistedOperationPhase::Succeeded,
                    progress: PersistedOperationProgress::default(),
                    started_at_unix: 0,
                    error: None,
                    run_artifact_id: None,
                    plan_commit_ref: None,
                    dry_verdict: None,
                    display_map: HashMap::new(),
                    agent_seq: 0,
                    agent_last_line: String::new(),
                }),
            );
        }
        assert!(ops.len() <= MAX_TERMINAL_OPS_PERSIST + 1);
        assert_eq!(next, 41);
    }
}
