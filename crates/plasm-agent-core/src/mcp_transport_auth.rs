//! Rust-only API for Streamable MCP transport: **API keys** via [`McpApiKeyRegistry`]
//! in shared [`SecretStore`] (encrypted for tenant data plane).
//!
//! HTTP handlers ([`crate::http_mcp_config`]) and host wiring depend on [`McpTransportAuth`], not on
//! [`SecretStore`] layout directly.

use crate::secret_store::SecretStoreError;
use async_trait::async_trait;
use uuid::Uuid;

use crate::mcp_api_key_registry::{
    McpApiKeyListItem, McpApiKeyProvisioned, McpApiKeyRegistry, McpApiKeyStatusPublic,
};

#[async_trait]
pub trait McpTransportAuth: Send + Sync {
    async fn revoke_for_config(&self, config_id: Uuid) -> Result<(), SecretStoreError>;

    /// Adds a new API key (does not revoke existing). `label` is a required display name.
    async fn provision_api_key(
        &self,
        config_id: Uuid,
        label: String,
    ) -> Result<McpApiKeyProvisioned, SecretStoreError>;

    async fn set_key_label(
        &self,
        config_id: Uuid,
        key_id: Uuid,
        label: String,
    ) -> Result<(), SecretStoreError>;

    /// Revokes all keys, then issues one new key. `new_key_label` is required.
    async fn rotate_api_key(
        &self,
        config_id: Uuid,
        new_key_label: String,
    ) -> Result<McpApiKeyProvisioned, SecretStoreError>;

    async fn list_api_keys(
        &self,
        config_id: Uuid,
    ) -> Result<Vec<McpApiKeyListItem>, SecretStoreError>;

    async fn reveal_api_key(
        &self,
        config_id: Uuid,
        key_id: Uuid,
    ) -> Result<String, SecretStoreError>;

    async fn revoke_one_api_key(
        &self,
        config_id: Uuid,
        key_id: Uuid,
    ) -> Result<(), SecretStoreError>;

    /// Revoke a single key and add a new one with a required display name.
    async fn rotate_one_api_key(
        &self,
        config_id: Uuid,
        key_id: Uuid,
        new_key_label: String,
    ) -> Result<McpApiKeyProvisioned, SecretStoreError>;

    async fn verify_api_key(&self, raw_key: &str) -> Option<Uuid>;

    async fn public_key_status(
        &self,
        config_id: Uuid,
    ) -> Result<Option<McpApiKeyStatusPublic>, SecretStoreError>;
}

#[async_trait]
impl McpTransportAuth for McpApiKeyRegistry {
    async fn revoke_for_config(&self, config_id: Uuid) -> Result<(), SecretStoreError> {
        McpApiKeyRegistry::revoke_for_config(self, config_id).await
    }

    async fn provision_api_key(
        &self,
        config_id: Uuid,
        label: String,
    ) -> Result<McpApiKeyProvisioned, SecretStoreError> {
        McpApiKeyRegistry::add_key(self, config_id, label).await
    }

    async fn set_key_label(
        &self,
        config_id: Uuid,
        key_id: Uuid,
        label: String,
    ) -> Result<(), SecretStoreError> {
        McpApiKeyRegistry::set_key_label(self, config_id, key_id, label).await
    }

    async fn rotate_api_key(
        &self,
        config_id: Uuid,
        new_key_label: String,
    ) -> Result<McpApiKeyProvisioned, SecretStoreError> {
        McpApiKeyRegistry::rotate_all_for_config(self, config_id, new_key_label).await
    }

    async fn list_api_keys(
        &self,
        config_id: Uuid,
    ) -> Result<Vec<McpApiKeyListItem>, SecretStoreError> {
        McpApiKeyRegistry::list_api_keys(self, config_id).await
    }

    async fn reveal_api_key(
        &self,
        config_id: Uuid,
        key_id: Uuid,
    ) -> Result<String, SecretStoreError> {
        McpApiKeyRegistry::reveal_api_key(self, config_id, key_id).await
    }

    async fn revoke_one_api_key(
        &self,
        config_id: Uuid,
        key_id: Uuid,
    ) -> Result<(), SecretStoreError> {
        McpApiKeyRegistry::revoke_one_api_key(self, config_id, key_id).await
    }

    async fn rotate_one_api_key(
        &self,
        config_id: Uuid,
        key_id: Uuid,
        new_key_label: String,
    ) -> Result<McpApiKeyProvisioned, SecretStoreError> {
        McpApiKeyRegistry::rotate_one_for_config(self, config_id, key_id, new_key_label).await
    }

    async fn verify_api_key(&self, raw_key: &str) -> Option<Uuid> {
        McpApiKeyRegistry::verify_api_key(self, raw_key).await
    }

    async fn public_key_status(
        &self,
        config_id: Uuid,
    ) -> Result<Option<McpApiKeyStatusPublic>, SecretStoreError> {
        McpApiKeyRegistry::public_status_for_config(self, config_id).await
    }
}
