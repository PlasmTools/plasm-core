//! Local receipt mirror; the server owns selection, generation pins and symbols.

use crate::terminal_state::{symbol_state_path, ExecutionBinding};
use anyhow::{anyhow, ensure, Result};
use serde::{Deserialize, Serialize};

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
    pub fn persist(&self, server: &str) -> Result<()> {
        let path = symbol_state_path(server, &self.client_session_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn load_from_disk(server: &str, id: &str) -> Result<Self> {
        let raw = std::fs::read(symbol_state_path(server, id))?;
        let state: Self = serde_json::from_slice(&raw).map_err(|_| {
            anyhow!("terminal state requires cutover; open an intent-only context with --new")
        })?;
        ensure!(
            state.version == 2 && state.client_session_id == id,
            "invalid routed terminal state; open context --new"
        );
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
