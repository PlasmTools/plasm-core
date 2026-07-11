//! Local appliance persistence for semantic auto-seed (OpenRouter key + runtime toggle).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const OPENROUTER_KEY_RELATIVE_PATH: &str = "bootstrap-secrets/OPENROUTER_API_KEY";
pub const SEMANTIC_AUTO_SEED_FLAG_RELATIVE_PATH: &str =
    "bootstrap-secrets/PLASM_DISCOVERY_SEMANTIC_AUTO_SEED";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryBootstrapState {
    pub semantic_auto_seed_enabled: bool,
    pub openrouter_key_configured: bool,
    pub model: String,
}

fn env_str_nonempty(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .is_some()
}

fn semantic_auto_seed_enabled_from_env() -> bool {
    std::env::var("PLASM_DISCOVERY_SEMANTIC_AUTO_SEED")
        .map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn discovery_model_from_env() -> String {
    std::env::var("PLASM_DISCOVERY_AUTO_SEED_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "openai/gpt-4.1-mini".into())
}

fn bootstrap_root() -> Option<PathBuf> {
    plasm_agent_core::oss_local_state::resolve_local_state_root()
}

pub fn openrouter_key_path() -> Option<PathBuf> {
    bootstrap_root().map(|root| root.join(OPENROUTER_KEY_RELATIVE_PATH))
}

pub fn semantic_auto_seed_flag_path() -> Option<PathBuf> {
    bootstrap_root().map(|root| root.join(SEMANTIC_AUTO_SEED_FLAG_RELATIVE_PATH))
}

fn open_local_secret_file_for_write(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

fn write_secret_file(path: &Path, contents: &str) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err(format!(
            "discovery bootstrap path has no parent directory: {}",
            path.display()
        ));
    };
    std::fs::create_dir_all(parent).map_err(|e| {
        format!(
            "discovery bootstrap directory create failed: {}: {e}",
            parent.display()
        )
    })?;
    let mut file = open_local_secret_file_for_write(path).map_err(|e| {
        format!(
            "discovery bootstrap file write failed: {}: {e}",
            path.display()
        )
    })?;
    file.write_all(contents.as_bytes()).map_err(|e| {
        format!(
            "discovery bootstrap file write failed: {}: {e}",
            path.display()
        )
    })
}

fn read_trimmed_file(path: &Path) -> Result<String, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "discovery bootstrap file read failed: {}: {e}",
            path.display()
        )
    })?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err(format!(
            "discovery bootstrap file is empty: {}",
            path.display()
        ));
    }
    Ok(trimmed)
}

pub fn current_state() -> DiscoveryBootstrapState {
    DiscoveryBootstrapState {
        semantic_auto_seed_enabled: semantic_auto_seed_enabled_from_env(),
        openrouter_key_configured: env_str_nonempty("OPENROUTER_API_KEY"),
        model: discovery_model_from_env(),
    }
}

/// Load persisted discovery settings into the process environment (explicit env wins).
pub fn ensure_discovery_bootstrap_at_boot() -> Result<DiscoveryBootstrapState, String> {
    if !env_str_nonempty("OPENROUTER_API_KEY") {
        if let Some(path) = openrouter_key_path() {
            if path.exists() {
                let key = read_trimmed_file(&path)?;
                std::env::set_var("OPENROUTER_API_KEY", key);
            }
        }
    }
    if !env_str_nonempty("PLASM_DISCOVERY_SEMANTIC_AUTO_SEED") {
        if let Some(path) = semantic_auto_seed_flag_path() {
            if path.exists() {
                let flag = read_trimmed_file(&path)?;
                std::env::set_var("PLASM_DISCOVERY_SEMANTIC_AUTO_SEED", flag);
            }
        }
    }
    Ok(current_state())
}

pub fn set_semantic_auto_seed_enabled(enabled: bool) -> Result<(), String> {
    let value = if enabled { "1" } else { "0" };
    std::env::set_var("PLASM_DISCOVERY_SEMANTIC_AUTO_SEED", value);
    let Some(path) = semantic_auto_seed_flag_path() else {
        return Err(
            "discovery bootstrap path unavailable; set PLASM_LOCAL_STATE_DIR or HOME".into(),
        );
    };
    write_secret_file(&path, value)
}

pub fn set_openrouter_api_key(key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("OpenRouter API key must not be empty".into());
    }
    std::env::set_var("OPENROUTER_API_KEY", key);
    let Some(path) = openrouter_key_path() else {
        return Err(
            "discovery bootstrap path unavailable; set PLASM_LOCAL_STATE_DIR or HOME".into(),
        );
    };
    write_secret_file(&path, key)
}

pub fn clear_openrouter_api_key() -> Result<(), String> {
    std::env::remove_var("OPENROUTER_API_KEY");
    if let Some(path) = openrouter_key_path() {
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| {
                format!(
                    "failed removing OpenRouter key file {}: {e}",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}

pub fn status_lines(state: &DiscoveryBootstrapState) -> Vec<String> {
    vec![
        format!(
            "Semantic auto-seed: {}",
            if state.semantic_auto_seed_enabled {
                "enabled"
            } else {
                "disabled"
            }
        ),
        format!(
            "OpenRouter key: {}",
            if state.openrouter_key_configured {
                "configured"
            } else {
                "missing"
            }
        ),
        format!("Model: {}", state.model),
        "Intent-only plasm_context (session_mode: new, no seeds) uses the LLM seed selector when enabled and keyed.".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_file_round_trip() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("PLASM_LOCAL_STATE_DIR", temp.path());
        std::env::remove_var("PLASM_DISCOVERY_SEMANTIC_AUTO_SEED");
        set_semantic_auto_seed_enabled(true).expect("enable");
        std::env::remove_var("PLASM_DISCOVERY_SEMANTIC_AUTO_SEED");
        let state = ensure_discovery_bootstrap_at_boot().expect("boot");
        assert!(state.semantic_auto_seed_enabled);
        std::env::remove_var("PLASM_LOCAL_STATE_DIR");
    }
}
