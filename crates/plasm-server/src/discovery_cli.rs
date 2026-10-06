//! Non-interactive `plasm-server discovery …` commands.

use clap::Subcommand;

use crate::discovery_bootstrap::{
    clear_openrouter_api_key, current_state, ensure_discovery_bootstrap_at_boot,
    set_openrouter_api_key, status_lines,
};

#[derive(Debug, clap::Args)]
pub struct DiscoveryCliRoot {
    #[command(subcommand)]
    pub command: DiscoveryCmd,
}

#[derive(Debug, Subcommand)]
pub enum DiscoveryCmd {
    /// Show compiled discovery configuration (`--json` for machine output).
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Set the OpenRouter API key (pass `--key` or pipe on stdin).
    SetOpenrouterKey {
        #[arg(long)]
        key: Option<String>,
    },
    /// Remove the persisted OpenRouter API key file.
    ClearOpenrouterKey,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryCliError {
    #[error(transparent)]
    Bootstrap(#[from] crate::discovery_bootstrap::DiscoveryBootstrapError),
    #[error("read stdin: {0}")]
    ReadStdin(#[source] std::io::Error),
}

pub fn run(cmd: DiscoveryCmd) -> Result<(), DiscoveryCliError> {
    let _ = ensure_discovery_bootstrap_at_boot()?;
    match cmd {
        DiscoveryCmd::Status { json } => {
            let state = current_state();
            if json {
                let payload = serde_json::json!({
                    "backend": "postgresql",
                    "openrouter_key_configured": state.openrouter_key_configured,
                    "model": state.model,
                });
                println!("{}", serde_json::to_string_pretty(&payload).expect("json"));
            } else {
                for line in status_lines(&state) {
                    println!("{line}");
                }
            }
            Ok(())
        }
        DiscoveryCmd::SetOpenrouterKey { key } => {
            let key = match key {
                Some(k) => k,
                None => {
                    use std::io::Read;
                    let mut buf = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buf)
                        .map_err(DiscoveryCliError::ReadStdin)?;
                    buf
                }
            };
            set_openrouter_api_key(&key)?;
            println!("OpenRouter API key saved");
            Ok(())
        }
        DiscoveryCmd::ClearOpenrouterKey => {
            clear_openrouter_api_key()?;
            println!("OpenRouter API key cleared");
            Ok(())
        }
    }
}
