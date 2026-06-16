//! Structured errors for async operation wait/cancel continuations.

use crate::operation::OperationProgress;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationError {
    UnknownHandle {
        handle: String,
        hint: String,
    },
    NotOnReplica {
        handle: String,
        progress: OperationProgress,
        agent_seq: u64,
        agent_last_line: String,
    },
    ResultArtifactMissing {
        handle: String,
        run_artifact_id: String,
    },
}

impl OperationError {
    pub const CODE_UNKNOWN: &'static str = "unknown_operation_handle";
    pub const CODE_NOT_ON_REPLICA: &'static str = "operation_not_on_replica";
    pub const CODE_ARTIFACT_MISSING: &'static str = "operation_result_unavailable";

    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownHandle { .. } => Self::CODE_UNKNOWN,
            Self::NotOnReplica { .. } => Self::CODE_NOT_ON_REPLICA,
            Self::ResultArtifactMissing { .. } => Self::CODE_ARTIFACT_MISSING,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Self::UnknownHandle { handle, hint } => format!(
                "unknown operation handle `{handle}` — stale continuation or wrong logical session; use `{hint}` from the latest tool result"
            ),
            Self::NotOnReplica { handle, .. } => format!(
                "operation `{handle}` is running on another host; poll `wait({handle})` until terminal or retry after completion"
            ),
            Self::ResultArtifactMissing {
                handle,
                run_artifact_id,
            } => format!(
                "operation `{handle}` completed but run artifact `{run_artifact_id}` is unavailable"
            ),
        }
    }
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail())
    }
}
