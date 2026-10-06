//! Structured errors for async operation wait/cancel continuations.
//!
//! **CEP-7:** terminal plan failures use [`OperationFailed`]; [`UnknownHandle`] is only for
//! absent or unrecognized handles while the operation may still be in flight.

use crate::operation::OperationProgress;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationError {
    UnknownHandle {
        handle: String,
        hint: String,
        open_handles: Vec<String>,
    },
    HandleNamespaceFailure {
        handle: String,
        error: crate::operation::OperationHandleResolutionError,
    },
    OperationFailed {
        handle: String,
        error: plasm_runtime::ExecutionFailure,
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
    pub const CODE_OPERATION_FAILED: &'static str = "operation_failed";
    pub const CODE_NOT_ON_REPLICA: &'static str = "operation_not_on_replica";
    pub const CODE_ARTIFACT_MISSING: &'static str = "operation_result_unavailable";

    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownHandle { .. } => Self::CODE_UNKNOWN,
            Self::OperationFailed { .. } => Self::CODE_OPERATION_FAILED,
            Self::HandleNamespaceFailure { .. } => Self::CODE_UNKNOWN,
            Self::NotOnReplica { .. } => Self::CODE_NOT_ON_REPLICA,
            Self::ResultArtifactMissing { .. } => Self::CODE_ARTIFACT_MISSING,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Self::UnknownHandle {
                handle,
                hint,
                open_handles,
            } => {
                let mut msg = format!(
                    "unknown operation handle `{handle}` — stale continuation or wrong logical session; use `{hint}` from the latest tool result"
                );
                if !open_handles.is_empty() {
                    const MAX_LIST: usize = 8;
                    let listed: Vec<&str> = open_handles.iter().take(MAX_LIST).map(String::as_str).collect();
                    let mut list = listed.join(", ");
                    if open_handles.len() > MAX_LIST {
                        list.push_str(", …");
                    }
                    msg.push_str(&format!("; open in this session: {list}"));
                }
                msg
            }
            Self::OperationFailed { handle, error } => {
                format!("operation `{handle}` failed: {error}")
            }
            Self::HandleNamespaceFailure { handle, error } => {
                format!("invalid operation handle `{handle}`: {error}")
            }
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

impl std::error::Error for OperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HandleNamespaceFailure { error, .. } => Some(error),
            Self::OperationFailed { error, .. } => Some(error),
            _ => None,
        }
    }
}

impl From<OperationError> for plasm_runtime::ExecutionFailure {
    fn from(error: OperationError) -> Self {
        match error {
            OperationError::OperationFailed { error, .. } => error,
            other @ (OperationError::UnknownHandle { .. }
            | OperationError::HandleNamespaceFailure { .. }) => Self::new(
                plasm_runtime::FailureCause::Program,
                other.code(),
                other.detail(),
            ),
            other => Self::new(
                plasm_runtime::FailureCause::Runtime,
                other.code(),
                other.detail(),
            ),
        }
    }
}

#[cfg(test)]
mod operation_error_tests {
    use super::*;

    #[test]
    fn unknown_handle_is_a_program_fault_not_an_internal_runtime_failure() {
        let error = OperationError::UnknownHandle {
            handle: "o999".into(),
            hint: "wait(o1)".into(),
            open_handles: vec!["o1".into()],
        };
        let diagnostic = error.detail();
        let failure = plasm_runtime::ExecutionFailure::from(error);
        assert_eq!(failure.cause, plasm_runtime::FailureCause::Program);
        assert_eq!(failure.recovery, plasm_runtime::RecoveryDisposition::RepairProgram);
        assert_eq!(failure.code, OperationError::CODE_UNKNOWN);
        assert_eq!(failure.diagnostic(), diagnostic);
    }

    #[test]
    fn operation_failed_detail_omits_private_diagnostic() {
        let err = OperationError::OperationFailed {
            handle: "o1".into(),
            error: plasm_runtime::ExecutionFailure::new(
                plasm_runtime::FailureCause::Runtime,
                "concurrent_execute_conflict",
                "session graph changed during concurrent execute; retry the request",
            ),
        };
        assert_eq!(err.code(), OperationError::CODE_OPERATION_FAILED);
        let detail = err.detail();
        assert_eq!(
            detail,
            "operation `o1` failed: concurrent_execute_conflict: Stop"
        );
        assert!(!detail.contains("session graph changed"));
        assert!(!detail.contains("retry the request"));
    }

    #[test]
    fn unknown_handle_lists_open_handles_in_detail() {
        let err = OperationError::UnknownHandle {
            handle: "l_test_o9".into(),
            hint: "wait(l_test_oN)".into(),
            open_handles: vec!["l_test_o1".into(), "l_test_o2".into()],
        };
        let detail = err.detail();
        assert!(detail.contains("l_test_o9"));
        assert!(detail.contains("open in this session: l_test_o1, l_test_o2"));
    }
}
