//! [`auth_framework`] integration: shared [`AuthStorage`] (memory or Postgres) for JWT-backed tokens
//! used by [`AuthFramework`], plus Plasm MCP API-key hashes in the same KV (`plasm_mcp_api_key_*`).
//! Postgres runs [`crate::auth_framework_postgres_schema::ensure_auth_storage_schema`] after connect,
//! then [`PostgresStorage::new`]. We do **not** call upstream [`PostgresStorage::migrate`]: in
//! **auth-framework 0.5.0-rc1** it issues one batch containing `INDEX …` inside `CREATE TABLE`, which
//! PostgreSQL rejects (MySQL-style). The embedded SQL also omits `user_profile` / `permissions` /
//! `roles` while `AuthToken` uses `sqlx::FromRow` over `SELECT *`, so the table must match the struct.
//!
//! After [`AuthFramework::initialize`], registers the upstream **`jwt`** auth method so
//! [`AuthFramework::create_auth_token`] can mint HS256 access tokens for non-MCP HTTP features.

use std::sync::Arc;
use std::time::Duration;

use auth_framework::config::{AuthConfig, SecurityConfig, StorageConfig};
use auth_framework::methods::{AuthMethodEnum, JwtMethod};
use auth_framework::storage::postgres::PostgresStorage;
use auth_framework::storage::MemoryStorage;
use auth_framework::storage::{AuthStorage, EncryptedStorage, StorageEncryption};
use auth_framework::AuthFramework;

use crate::mcp_api_key_registry::McpApiKeyRegistry;

/// Embedded dev-only signing key when env is unset or rejected. Must satisfy auth-framework JWT
/// validation (the previous default contained dictionary-like words and failed `initialize()`).
const DEV_JWT_SECRET: &str =
    "nM8kQ2wE5rT7yU1iO3pA6sD9fG4hJ0zXvC2bN5mL8qW6eR3tY7uI1oP4aS9dF2gH5jK0lZxVnBqMw";

fn jwt_secret_failed_validation(err: &auth_framework::AuthError) -> bool {
    let s = err.to_string().to_lowercase();
    s.contains("jwt secret")
        || s.contains("common words")
        || s.contains("patterns")
        || s.contains("cryptographically")
}

#[derive(Clone, Debug)]
enum AuthStorageMode {
    Memory,
    Postgres { connection_string: String },
}

/// Prefer `PLASM_AUTH_STORAGE_URL`, then `DATABASE_URL`. Without either, use in-memory storage
/// (API key hashes and framework KV are not durable across process restarts).
pub async fn init_standalone_auth_storage(
) -> Result<Arc<dyn AuthStorage>, auth_framework::AuthError> {
    let (storage, _) = create_auth_storage().await?;
    Ok(storage)
}

async fn create_auth_storage(
) -> Result<(Arc<dyn AuthStorage>, AuthStorageMode), auth_framework::AuthError> {
    let in_k8s = std::env::var("KUBERNETES_SERVICE_HOST")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .is_some();
    let allow_memory = matches!(
        std::env::var("ENV").ok().as_deref(),
        Some("test") | Some("TEST")
    );

    let url = std::env::var("PLASM_AUTH_STORAGE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("DATABASE_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
        });
    let Some(url) = url else {
        if in_k8s && !allow_memory {
            return Err(auth_framework::AuthError::configuration(
                "Kubernetes detected but auth storage URL is unset. Configure PLASM_AUTH_STORAGE_URL (or DATABASE_URL) for durable auth storage; refusing in-memory backend in k8s."
                    .to_string(),
            ));
        }
        tracing::warn!(
            in_k8s = in_k8s,
            allow_memory = allow_memory,
            "auth storage backend: memory (non-durable)"
        );
        return Ok((
            Arc::new(MemoryStorage::new()) as Arc<dyn AuthStorage>,
            AuthStorageMode::Memory,
        ));
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .map_err(|e| {
            auth_framework::AuthError::configuration(format!(
                "PLASM_AUTH_STORAGE_URL / DATABASE_URL: postgres connect failed: {e}"
            ))
        })?;
    crate::auth_framework_postgres_schema::ensure_auth_storage_schema(&pool)
        .await
        .map_err(|e| {
            auth_framework::AuthError::configuration(format!(
                "auth-framework Postgres schema (kv_store / oauth): {e}"
            ))
        })?;
    tracing::info!(in_k8s = in_k8s, "auth storage backend: postgres");
    let pg = PostgresStorage::new(pool);
    let encryption = StorageEncryption::new().map_err(|e| {
        auth_framework::AuthError::configuration(format!(
            "AUTH_STORAGE_ENCRYPTION_KEY is required for Postgres auth KV encryption: {e}"
        ))
    })?;
    let encrypted = EncryptedStorage::new(pg, encryption);
    Ok((
        Arc::new(encrypted) as Arc<dyn AuthStorage>,
        AuthStorageMode::Postgres {
            connection_string: url,
        },
    ))
}

async fn build_framework_on_storage(
    storage: Arc<dyn AuthStorage>,
    storage_mode: AuthStorageMode,
    secret: String,
) -> Result<Arc<tokio::sync::Mutex<AuthFramework>>, auth_framework::AuthError> {
    let security = SecurityConfig {
        secret_key: Some(secret),
        csrf_protection: false,
        ..Default::default()
    };

    let storage_config = match storage_mode {
        AuthStorageMode::Memory => StorageConfig::Memory,
        AuthStorageMode::Postgres { connection_string } => StorageConfig::Postgres {
            connection_string,
            table_prefix: "".to_string(),
        },
    };

    let config = AuthConfig::new()
        .storage(storage_config)
        .issuer("plasm-agent")
        .audience("plasm")
        .token_lifetime(Duration::from_secs(3600))
        .refresh_token_lifetime(Duration::from_secs(86400 * 7))
        .security(security);

    let mut framework = AuthFramework::new_with_storage(config, storage);
    framework.initialize().await?;
    let tm = Arc::new(framework.token_manager().clone());
    framework.register_method(
        "jwt",
        AuthMethodEnum::Jwt(JwtMethod::with_token_manager(tm)),
    );
    Ok(Arc::new(tokio::sync::Mutex::new(framework)))
}

/// Shared storage for [`AuthFramework`] and [`McpApiKeyRegistry`] (see [`crate::mcp_transport_auth::McpTransportAuth`]).
pub async fn init_plasm_http_auth_bundle() -> Result<
    (
        Arc<tokio::sync::Mutex<AuthFramework>>,
        Arc<McpApiKeyRegistry>,
        Arc<dyn AuthStorage>,
    ),
    auth_framework::AuthError,
> {
    let (storage, storage_mode) = create_auth_storage().await?;
    let mcp_api_keys = Arc::new(McpApiKeyRegistry::new(storage.clone()));
    let in_k8s = std::env::var("KUBERNETES_SERVICE_HOST")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .is_some();
    let allow_dev_secret = matches!(
        std::env::var("ENV").ok().as_deref(),
        Some("test") | Some("TEST")
    ) && !in_k8s;

    let from_env = std::env::var("PLASM_AUTH_JWT_SECRET").ok();
    let secret = match from_env.clone() {
        Some(v) => v,
        None if allow_dev_secret => {
            tracing::warn!(
                "PLASM_AUTH_JWT_SECRET is unset; using an insecure development JWT signing key"
            );
            DEV_JWT_SECRET.to_string()
        }
        None => {
            return Err(auth_framework::AuthError::configuration(
                "PLASM_AUTH_JWT_SECRET is required outside explicit local test mode".to_string(),
            ));
        }
    };

    match build_framework_on_storage(storage.clone(), storage_mode.clone(), secret).await {
        Ok(fw) => Ok((fw, mcp_api_keys, storage)),
        Err(e) if from_env.is_some() && jwt_secret_failed_validation(&e) && allow_dev_secret => {
            tracing::warn!(
                "PLASM_AUTH_JWT_SECRET failed validation ({}); using insecure development JWT signing key. \
                 Set a long random `PLASM_AUTH_JWT_SECRET` for production.",
                e
            );
            let fw = build_framework_on_storage(
                storage.clone(),
                storage_mode,
                DEV_JWT_SECRET.to_string(),
            )
            .await?;
            Ok((fw, mcp_api_keys, storage))
        }
        Err(e) => Err(e),
    }
}

/// Non-durable MCP API keys — used when Postgres auth KV cannot be opened but `project_mcp_*` DB is configured.
pub fn mcp_api_key_registry_memory_only() -> Arc<McpApiKeyRegistry> {
    let storage = Arc::new(MemoryStorage::new()) as Arc<dyn AuthStorage>;
    Arc::new(McpApiKeyRegistry::new(storage))
}

/// In-memory shared storage for tests (same `Arc` for framework + MCP API key registry).
pub async fn init_plasm_http_auth_bundle_memory(
    jwt_secret: String,
) -> Result<
    (
        Arc<tokio::sync::Mutex<AuthFramework>>,
        Arc<McpApiKeyRegistry>,
        Arc<dyn AuthStorage>,
    ),
    auth_framework::AuthError,
> {
    let storage = Arc::new(MemoryStorage::new()) as Arc<dyn AuthStorage>;
    let mcp_api_keys = Arc::new(McpApiKeyRegistry::new(storage.clone()));
    let fw =
        build_framework_on_storage(storage.clone(), AuthStorageMode::Memory, jwt_secret).await?;
    Ok((fw, mcp_api_keys, storage))
}
