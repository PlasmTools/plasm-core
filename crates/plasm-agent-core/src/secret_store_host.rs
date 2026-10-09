//! Host assembly for Plasm-owned credential storage.
use crate::mcp_api_key_registry::McpApiKeyRegistry;
use crate::secret_store::{
    EncryptedSecretStore, MemorySecretStore, PostgresSecretStore, SecretEncryption, SecretStore,
    SecretStoreError,
};
use std::sync::Arc;

pub fn resolve_jwt_signing_secret() -> Result<String, SecretStoreError> {
    let secret = std::env::var("PLASM_AUTH_JWT_SECRET").map_err(|_| {
        SecretStoreError::Configuration("PLASM_AUTH_JWT_SECRET is required for OAuth")
    })?;
    if secret.len() < 32 {
        return Err(SecretStoreError::Configuration(
            "JWT signing secret must contain at least 32 bytes",
        ));
    }
    Ok(secret)
}
fn database_url() -> Option<String> {
    [
        "PLASM_AUTH_STORAGE_URL",
        "DATABASE_URL",
        "PLASM_MCP_CONFIG_DATABASE_URL",
    ]
    .into_iter()
    .find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}
pub async fn init_standalone_auth_storage() -> Result<Arc<dyn SecretStore>, SecretStoreError> {
    let Some(url) = database_url() else {
        if std::env::var("KUBERNETES_SERVICE_HOST").is_ok()
            && !std::env::var("ENV").is_ok_and(|value| value.eq_ignore_ascii_case("test"))
        {
            return Err(SecretStoreError::Configuration(
                "durable secret storage is required in Kubernetes",
            ));
        }
        tracing::warn!("credential storage: non-durable memory backend");
        return Ok(Arc::new(MemorySecretStore::new()));
    };
    let encryption = SecretEncryption::from_environment()?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await?;
    init_postgres_secret_store(pool, encryption).await
}

/// Assemble durable storage and its bounded expiry lifecycle on an existing pool.
pub async fn init_postgres_secret_store(
    pool: sqlx::PgPool,
    encryption: SecretEncryption,
) -> Result<Arc<dyn SecretStore>, SecretStoreError> {
    sqlx::query("CREATE TABLE IF NOT EXISTS kv_store (key VARCHAR(255) PRIMARY KEY, value BYTEA NOT NULL, expires_at TIMESTAMPTZ, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW())").execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_kv_store_expires_at ON kv_store (expires_at)")
        .execute(&pool)
        .await?;
    let store: Arc<dyn SecretStore> = Arc::new(EncryptedSecretStore::new(
        PostgresSecretStore::new(pool),
        encryption,
    ));
    store.purge_expired(1000).await?;
    let weak = Arc::downgrade(&store);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(store) = weak.upgrade() else {
                break;
            };
            if let Err(error) = store.purge_expired(1000).await {
                tracing::warn!(error = %error, "credential expiry reclamation failed");
            }
        }
    });
    Ok(store)
}
pub async fn init_plasm_http_auth_bundle(
) -> Result<(Arc<McpApiKeyRegistry>, Arc<dyn SecretStore>), SecretStoreError> {
    let storage = init_standalone_auth_storage().await?;
    Ok((Arc::new(McpApiKeyRegistry::new(storage.clone())), storage))
}
pub fn memory_auth_bundle() -> (Arc<McpApiKeyRegistry>, Arc<dyn SecretStore>) {
    let storage: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    (Arc::new(McpApiKeyRegistry::new(storage.clone())), storage)
}
