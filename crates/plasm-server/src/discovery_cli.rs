//! Non-interactive `plasm-server discovery …` commands.

use clap::Subcommand;

use crate::discovery_bootstrap::{
    clear_openrouter_api_key, current_state, ensure_discovery_bootstrap_at_boot,
    set_openrouter_api_key, set_semantic_auto_seed_enabled, status_lines,
};

#[derive(Debug, clap::Args)]
pub struct DiscoveryCliRoot {
    #[command(subcommand)]
    pub command: DiscoveryCmd,
}

#[derive(Debug, Subcommand)]
pub enum DiscoveryCmd {
    /// Show semantic auto-seed configuration (`--json` for machine output).
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Enable intent-only semantic auto-seed (persists under bootstrap-secrets/).
    Enable,
    /// Disable semantic auto-seed (persists under bootstrap-secrets/).
    Disable,
    /// Set the OpenRouter API key (pass `--key` or pipe on stdin).
    SetOpenrouterKey {
        #[arg(long)]
        key: Option<String>,
    },
    /// Remove the persisted OpenRouter API key file.
    ClearOpenrouterKey,
}

pub fn run(cmd: DiscoveryCmd) -> Result<(), String> {
    let _ = ensure_discovery_bootstrap_at_boot()?;
    match cmd {
        DiscoveryCmd::Status { json } => {
            let state = current_state();
            if json {
                let payload = serde_json::json!({
                    "semantic_auto_seed_enabled": state.semantic_auto_seed_enabled,
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
        DiscoveryCmd::Enable => {
            set_semantic_auto_seed_enabled(true)?;
            println!("semantic auto-seed enabled");
            Ok(())
        }
        DiscoveryCmd::Disable => {
            set_semantic_auto_seed_enabled(false)?;
            println!("semantic auto-seed disabled");
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
                        .map_err(|e| format!("read stdin: {e}"))?;
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
