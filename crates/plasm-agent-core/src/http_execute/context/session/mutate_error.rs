//! Typed errors for execute-session mutation (extend / federate / intent / ranked caps).

use crate::mcp_transport_store::execute_session_registry::ExecuteSessionPersistError;

/// Failures while mutating a live execute session (durable persist or validation).
#[derive(Debug)]
pub enum SessionMutateError {
    Persist(ExecuteSessionPersistError),
    Message(String),
}

impl std::fmt::Display for SessionMutateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Persist(e) => write!(f, "{e}"),
            Self::Message(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for SessionMutateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Persist(e) => Some(e),
            Self::Message(_) => None,
        }
    }
}

impl From<ExecuteSessionPersistError> for SessionMutateError {
    fn from(e: ExecuteSessionPersistError) -> Self {
        Self::Persist(e)
    }
}

impl From<String> for SessionMutateError {
    fn from(m: String) -> Self {
        Self::Message(m)
    }
}

impl From<&str> for SessionMutateError {
    fn from(m: &str) -> Self {
        Self::Message(m.to_string())
    }
}
