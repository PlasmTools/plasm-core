//! Local appliance persistence for the OpenRouter discovery key.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const OPENROUTER_KEY_RELATIVE_PATH: &str = "bootstrap-secrets/OPENROUTER_API_KEY";

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryBootstrapError {
    #[error("discovery bootstrap path has no parent directory: {}", .path.display())]
    MissingParent { path: PathBuf },
    #[error("discovery bootstrap directory create failed: {}: {source}", .path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("discovery bootstrap file open failed: {}: {source}", .path.display())]
    OpenFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("discovery bootstrap file write failed: {}: {source}", .path.display())]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("discovery bootstrap file read failed: {}: {source}", .path.display())]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("discovery bootstrap file is empty: {}", .path.display())]
    EmptyFile { path: PathBuf },
    #[error("OpenRouter API key must not be empty")]
    EmptyApiKey,
    #[error("discovery bootstrap path unavailable; set PLASM_LOCAL_STATE_DIR or HOME")]
    StatePathUnavailable,
    #[error("failed removing OpenRouter key file {}: {source}", .path.display())]
    RemoveFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryBootstrapState {
    pub openrouter_key_configured: bool,
    pub model: String,
}

fn env_str_nonempty(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .is_some()
}

fn discovery_model_from_env() -> String {
    std::env::var("PLASM_DISCOVERY_CAPABILITY_MATCH_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "typesafe/jev-1.13".into())
}

fn bootstrap_root() -> Option<PathBuf> {
    plasm_agent_core::oss_local_state::resolve_local_state_root()
}

pub fn openrouter_key_path() -> Option<PathBuf> {
    bootstrap_root().map(|root| root.join(OPENROUTER_KEY_RELATIVE_PATH))
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

fn write_secret_file(path: &Path, contents: &str) -> Result<(), DiscoveryBootstrapError> {
    let Some(parent) = path.parent() else {
        return Err(DiscoveryBootstrapError::MissingParent {
            path: path.to_owned(),
        });
    };
    std::fs::create_dir_all(parent).map_err(|source| DiscoveryBootstrapError::CreateDirectory {
        path: parent.to_owned(),
        source,
    })?;
    let mut file = open_local_secret_file_for_write(path).map_err(|source| {
        DiscoveryBootstrapError::OpenFile {
            path: path.to_owned(),
            source,
        }
    })?;
    file.write_all(contents.as_bytes())
        .map_err(|source| DiscoveryBootstrapError::WriteFile {
            path: path.to_owned(),
            source,
        })
}

fn read_trimmed_file(path: &Path) -> Result<String, DiscoveryBootstrapError> {
    let raw =
        std::fs::read_to_string(path).map_err(|source| DiscoveryBootstrapError::ReadFile {
            path: path.to_owned(),
            source,
        })?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err(DiscoveryBootstrapError::EmptyFile {
            path: path.to_owned(),
        });
    }
    Ok(trimmed)
}

pub fn current_state() -> DiscoveryBootstrapState {
    DiscoveryBootstrapState {
        openrouter_key_configured: env_str_nonempty("OPENROUTER_API_KEY"),
        model: discovery_model_from_env(),
    }
}

/// Load persisted discovery settings into the process environment (explicit env wins).
pub fn ensure_discovery_bootstrap_at_boot(
) -> Result<DiscoveryBootstrapState, DiscoveryBootstrapError> {
    if !env_str_nonempty("OPENROUTER_API_KEY") {
        if let Some(path) = openrouter_key_path() {
            if path.exists() {
                let key = read_trimmed_file(&path)?;
                std::env::set_var("OPENROUTER_API_KEY", key);
            }
        }
    }
    Ok(current_state())
}

pub fn set_openrouter_api_key(key: &str) -> Result<(), DiscoveryBootstrapError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(DiscoveryBootstrapError::EmptyApiKey);
    }
    std::env::set_var("OPENROUTER_API_KEY", key);
    let Some(path) = openrouter_key_path() else {
        return Err(DiscoveryBootstrapError::StatePathUnavailable);
    };
    write_secret_file(&path, key)
}

pub fn clear_openrouter_api_key() -> Result<(), DiscoveryBootstrapError> {
    std::env::remove_var("OPENROUTER_API_KEY");
    if let Some(path) = openrouter_key_path() {
        if path.exists() {
            std::fs::remove_file(&path).map_err(|source| DiscoveryBootstrapError::RemoveFile {
                path: path.clone(),
                source,
            })?;
        }
    }
    Ok(())
}

pub fn status_lines(state: &DiscoveryBootstrapState) -> Vec<String> {
    vec![
        "Discovery: PostgreSQL lexical + vector retrieval".into(),
        format!(
            "OpenRouter key: {}",
            if state.openrouter_key_configured {
                "configured"
            } else {
                "missing"
            }
        ),
        format!("Model: {}", state.model),
        "Intent-only new and extend use one capability selector and declared prerequisite closure."
            .into(),
    ]
}
