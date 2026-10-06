//! Local receipt mirror; the server owns selection, generation pins and symbols.

use crate::terminal_state::{symbol_state_path, ExecutionBinding};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RoutedTerminalSessionError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("terminal receipt serialization failed")]
    Serialize(#[source] serde_json::Error),
    #[error(
        "terminal receipt is not valid routed-session JSON; open an intent-only context with --new"
    )]
    InvalidReceipt(#[source] serde_json::Error),
    #[error("routed terminal session version {actual} is unsupported; open context --new")]
    UnsupportedVersion { actual: u32 },
    #[error(
        "routed terminal receipt belongs to session {actual}, not requested session {expected}"
    )]
    SessionMismatch { expected: String, actual: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutedTerminalSession {
    pub version: u32,
    pub client_session_id: String,
    pub intent: String,
    pub execution: ExecutionBinding,
    pub generation: String,
}

impl RoutedTerminalSession {
    pub fn persist(&self, server: &str) -> Result<(), RoutedTerminalSessionError> {
        let path = symbol_state_path(server, &self.client_session_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes =
            serde_json::to_vec_pretty(self).map_err(RoutedTerminalSessionError::Serialize)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    pub fn load_from_disk(server: &str, id: &str) -> Result<Self, RoutedTerminalSessionError> {
        let raw = std::fs::read(symbol_state_path(server, id))?;
        let state: Self =
            serde_json::from_slice(&raw).map_err(RoutedTerminalSessionError::InvalidReceipt)?;
        if state.version != 2 {
            return Err(RoutedTerminalSessionError::UnsupportedVersion {
                actual: state.version,
            });
        }
        if state.client_session_id != id {
            return Err(RoutedTerminalSessionError::SessionMismatch {
                expected: id.to_owned(),
                actual: state.client_session_id,
            });
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routed_state_roundtrip_and_old_symbol_state_rejection() {
        let root = tempfile::tempdir().unwrap();
        crate::terminal_state::test_env::with_plasm_workspace(root.path(), || {
            let state = RoutedTerminalSession {
                version: 2,
                client_session_id: "receipt".into(),
                intent: "read".into(),
                execution: ExecutionBinding {
                    prompt_hash: "ph".into(),
                    session: "sid".into(),
                },
                generation: "pinned".into(),
            };
            state.persist("server").unwrap();
            let loaded = RoutedTerminalSession::load_from_disk("server", "receipt").unwrap();
            assert_eq!(loaded.execution, state.execution);
            assert_eq!(loaded.generation, state.generation);
            let path = symbol_state_path("server", "receipt");
            std::fs::write(
                &path,
                r#"{"version":1,"client_session_id":"receipt","capabilities":[]}"#,
            )
            .unwrap();
            assert!(RoutedTerminalSession::load_from_disk("server", "receipt").is_err());
        });
    }
}
