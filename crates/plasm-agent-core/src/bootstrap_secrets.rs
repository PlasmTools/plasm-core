//! MCP bootstrap secrets: **load** material (files or policy), then **install** into `std::env`
//! for upstream crates that still read `std::env::var` (`auth-framework`, `sqlx`, etc.).
//!
//! ## Ordering vs `dotenv_safe`
//!
//! [`crate::dotenv_safe::load_from_cwd_parents`] runs in [`crate::init_agent_runtime`] **before**
//! materialization. When `PLASM_SECRETS_DIR` is set, [`install_mcp_bootstrap_process_env`] **always**
//! overwrites those keys in the process environment — **mounted / synced secrets win** over `.env`.
//! When there is no file-backed material (`Option::None` on local builds with
//! `local_env_bootstrap_secrets` only), nothing is installed and dotenv / shell values remain.
//!
//! ## Kubernetes vs local
//!
//! In Kubernetes (`KUBERNETES_SERVICE_HOST` set), `PLASM_SECRETS_DIR` is **required**. Outside the
//! cluster, env-only bootstrap is allowed only with the **`local_env_bootstrap_secrets`** Cargo
//! feature (`just local-agent`).
//!
//! ## `McpBootstrapMaterializer`
//!
//! Narrow hook for future Infisical SDK loaders: swap the loader (`load_mcp_bootstrap_material`)
//! while reusing [`install_mcp_bootstrap_process_env`].

use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Optional bootstrap env keys (mounted when present; omit for local dev without LLM).
pub const MCP_OPTIONAL_BOOTSTRAP_ENV_KEYS: &[&str] = &["OPENROUTER_API_KEY"];

/// Canonical env keys for MCP bootstrap material (aligned with `scripts/k8s/ensure-plasm-secrets.sh`
/// and Helm projected Secret keys).
pub const MCP_BOOTSTRAP_ENV_KEYS: &[&str] = &[
    "DATABASE_URL",
    "PLASM_AUTH_STORAGE_URL",
    "PLASM_AUTH_JWT_SECRET",
    "AUTH_STORAGE_ENCRYPTION_KEY",
    "PLASM_MCP_CONTROL_PLANE_SECRET",
];

#[derive(Error, Debug)]
pub enum BootstrapSecretsError {
    #[error(
        "PLASM_SECRETS_DIR is unset in Kubernetes; Helm must mount bootstrap secrets as files \
         (see deploy/charts/plasm-mcp and deploy/docs/infisical-cloud.md)"
    )]
    MissingSecretsDirInKubernetes,

    #[error(
        "bootstrap secrets: set PLASM_SECRETS_DIR to a directory of secret files, or rebuild \
         with --features local_env_bootstrap_secrets for local env-based development"
    )]
    LocalEnvBootstrapDisabled,

    #[error("bootstrap secret {key}: failed reading {path}: {source}")]
    ReadFailed {
        key: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("bootstrap secret {key}: empty file at {path}")]
    EmptySecretFile { key: &'static str, path: PathBuf },
}

/// Non-empty MCP bootstrap values loaded from disk (or reserved for future SDK sources).
#[derive(Debug, Clone)]
pub struct McpBootstrapMaterial {
    pub database_url: String,
    pub plasm_auth_storage_url: String,
    pub plasm_auth_jwt_secret: String,
    pub auth_storage_encryption_key: String,
    pub plasm_mcp_control_plane_secret: String,
    pub openrouter_api_key: Option<String>,
}

/// Heuristic policy gate: set in Kubernetes pods; **not** a security boundary.
pub fn running_inside_kubernetes() -> bool {
    std::env::var("KUBERNETES_SERVICE_HOST")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .is_some()
}

fn read_secret_file(dir: &Path, key: &'static str) -> Result<String, BootstrapSecretsError> {
    let path = dir.join(key);
    let raw = fs::read_to_string(&path).map_err(|source| BootstrapSecretsError::ReadFailed {
        key,
        path: path.clone(),
        source,
    })?;
    let t = raw.trim().to_string();
    if t.is_empty() {
        return Err(BootstrapSecretsError::EmptySecretFile { key, path });
    }
    Ok(t)
}

fn read_optional_secret_file(
    dir: &Path,
    key: &'static str,
) -> Result<Option<String>, BootstrapSecretsError> {
    let path = dir.join(key);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(read_secret_file(dir, key)?))
}

/// Load bootstrap material from a directory whose files are named exactly like `MCP_BOOTSTRAP_ENV_KEYS`.
///
/// Injected `dir` keeps unit tests free of global `PLASM_SECRETS_DIR` mutation.
pub fn load_mcp_bootstrap_material_from_dir(
    dir: &Path,
) -> Result<McpBootstrapMaterial, BootstrapSecretsError> {
    Ok(McpBootstrapMaterial {
        database_url: read_secret_file(dir, "DATABASE_URL")?,
        plasm_auth_storage_url: read_secret_file(dir, "PLASM_AUTH_STORAGE_URL")?,
        plasm_auth_jwt_secret: read_secret_file(dir, "PLASM_AUTH_JWT_SECRET")?,
        auth_storage_encryption_key: read_secret_file(dir, "AUTH_STORAGE_ENCRYPTION_KEY")?,
        plasm_mcp_control_plane_secret: read_secret_file(dir, "PLASM_MCP_CONTROL_PLANE_SECRET")?,
        openrouter_api_key: read_optional_secret_file(dir, "OPENROUTER_API_KEY")?,
    })
}

/// Read `PLASM_SECRETS_DIR` from the environment and load file-backed material when set.
///
/// Returns `Ok(None)` when not in Kubernetes, `PLASM_SECRETS_DIR` is unset, and this build includes
/// `local_env_bootstrap_secrets` (local env / dotenv supplies values; nothing to install here).
pub fn load_mcp_bootstrap_material() -> Result<Option<McpBootstrapMaterial>, BootstrapSecretsError>
{
    let dir = std::env::var("PLASM_SECRETS_DIR")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if let Some(ref d) = dir {
        return Ok(Some(load_mcp_bootstrap_material_from_dir(Path::new(d))?));
    }

    if running_inside_kubernetes() {
        return Err(BootstrapSecretsError::MissingSecretsDirInKubernetes);
    }

    #[cfg(feature = "local_env_bootstrap_secrets")]
    {
        Ok(None)
    }
    #[cfg(not(feature = "local_env_bootstrap_secrets"))]
    {
        Err(BootstrapSecretsError::LocalEnvBootstrapDisabled)
    }
}

/// Install loaded material into the process environment (overwrites existing values for these keys).
pub fn install_mcp_bootstrap_process_env(m: &McpBootstrapMaterial) {
    std::env::set_var("DATABASE_URL", &m.database_url);
    std::env::set_var("PLASM_AUTH_STORAGE_URL", &m.plasm_auth_storage_url);
    std::env::set_var("PLASM_AUTH_JWT_SECRET", &m.plasm_auth_jwt_secret);
    std::env::set_var(
        "AUTH_STORAGE_ENCRYPTION_KEY",
        &m.auth_storage_encryption_key,
    );
    std::env::set_var(
        "PLASM_MCP_CONTROL_PLANE_SECRET",
        &m.plasm_mcp_control_plane_secret,
    );
    if let Some(key) = &m.openrouter_api_key {
        std::env::set_var("OPENROUTER_API_KEY", key);
    }
}

/// MCP process bootstrap: load then install. Used by [`DefaultMcpBootstrapSecrets`].
pub fn materialize_mcp_bootstrap_process_env() -> Result<(), BootstrapSecretsError> {
    if let Some(m) = load_mcp_bootstrap_material()? {
        install_mcp_bootstrap_process_env(&m);
    }
    Ok(())
}

/// Opaque hook for alternate loaders (e.g. Infisical SDK) while reusing [`install_mcp_bootstrap_process_env`].
pub trait McpBootstrapMaterializer {
    fn materialize_mcp_process_env(&self) -> Result<(), BootstrapSecretsError>;
}

/// Default: file-first via [`load_mcp_bootstrap_material`] + [`install_mcp_bootstrap_process_env`].
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultMcpBootstrapSecrets;

impl McpBootstrapMaterializer for DefaultMcpBootstrapSecrets {
    fn materialize_mcp_process_env(&self) -> Result<(), BootstrapSecretsError> {
        materialize_mcp_bootstrap_process_env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_secret_file_trims_via_loader() {
        let d = tempfile::tempdir().expect("tempdir");
        let jwt = "x".repeat(64);
        let ctrl = "y".repeat(44);
        for (k, v) in [
            ("DATABASE_URL", "postgresql://u:p@localhost/db"),
            ("PLASM_AUTH_STORAGE_URL", "postgresql://u:p@localhost/db"),
            ("PLASM_AUTH_JWT_SECRET", jwt.as_str()),
            (
                "AUTH_STORAGE_ENCRYPTION_KEY",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            ),
            ("PLASM_MCP_CONTROL_PLANE_SECRET", ctrl.as_str()),
        ] {
            std::fs::write(d.path().join(k), format!("{v}\n")).expect("write");
        }
        let m = load_mcp_bootstrap_material_from_dir(d.path()).expect("load");
        assert_eq!(m.database_url, "postgresql://u:p@localhost/db");
        assert_eq!(m.plasm_auth_jwt_secret, jwt);
    }

    #[test]
    fn load_fails_on_empty_file() {
        let d = tempfile::tempdir().expect("tempdir");
        for (k, v) in [
            ("DATABASE_URL", "postgresql://localhost/db"),
            ("PLASM_AUTH_STORAGE_URL", "postgresql://localhost/db"),
            (
                "PLASM_AUTH_JWT_SECRET",
                "secretsecretsecretsecretsecretsecretsecretsecretsecretsecret01",
            ),
            (
                "AUTH_STORAGE_ENCRYPTION_KEY",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            ),
            (
                "PLASM_MCP_CONTROL_PLANE_SECRET",
                "controlplanecontrolplanecontrolplane01",
            ),
        ] {
            std::fs::write(d.path().join(k), v).expect("write");
        }
        std::fs::write(d.path().join("PLASM_AUTH_JWT_SECRET"), "  \n").expect("overwrite empty");
        let err = load_mcp_bootstrap_material_from_dir(d.path()).unwrap_err();
        assert!(matches!(err, BootstrapSecretsError::EmptySecretFile { .. }));
    }

    #[test]
    fn load_fails_on_missing_file() {
        let d = tempfile::tempdir().expect("tempdir");
        std::fs::write(d.path().join("DATABASE_URL"), "postgresql://localhost/db").expect("write");
        let err = load_mcp_bootstrap_material_from_dir(d.path()).unwrap_err();
        assert!(matches!(err, BootstrapSecretsError::ReadFailed { .. }));
    }
}
