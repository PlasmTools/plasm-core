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
    OperationFailed {
        handle: String,
        error: String,
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

#[cfg(test)]
mod operation_error_tests {
    use super::*;

    #[test]
    fn operation_failed_detail_is_verbatim() {
        let err = OperationError::OperationFailed {
            handle: "o1".into(),
            error: "session graph changed during concurrent execute; retry the request".into(),
        };
        assert_eq!(err.code(), OperationError::CODE_OPERATION_FAILED);
        assert_eq!(
            err.detail(),
            "operation `o1` failed: session graph changed during concurrent execute; retry the request"
        );
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
