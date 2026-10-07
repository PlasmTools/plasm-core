//! Tenant MCP Streamable HTTP API keys: **N keys per** [`Uuid`] `config_id`, each with a
//! stable `key_id`. Verifier uses `plasm_mcp_api2_hash:*` → `key_id` → `plasm_mcp_api2_key:*`
//! (raw key material in encrypted [`SecretStore`], per ownership doc).
//!
//! **Cutover** from the prior single-key model: old `plasm_mcp_api_key_*` keys are not read;

use std::sync::Arc;

use crate::secret_store::SecretStore;
use crate::secret_store::SecretStoreError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const KV_SET: &str = "plasm_mcp_api2_set:";
const KV_KEY: &str = "plasm_mcp_api2_key:";
const KV_HASH: &str = "plasm_mcp_api2_hash:";

fn set_key(config_id: Uuid) -> String {
    format!("{KV_SET}{config_id}")
}

fn key_rec_key(key_id: Uuid) -> String {
    format!("{KV_KEY}{key_id}")
}

fn hash_to_key_id_kv(hash_hex: &str) -> String {
    format!("{KV_HASH}{hash_hex}")
}

fn sha256_hex(data: &[u8]) -> String {
    let d = Sha256::digest(data);
    hex::encode(d)
}

/// Returned by provision/rotate; safe to return to the control plane once per operation.
#[derive(Debug, Clone, Serialize)]
pub struct McpApiKeyProvisioned {
    pub key_id: Uuid,
    pub api_key: String,
}

/// Fingerprint of one key, safe for public listing (no raw secret).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpApiKeyListItem {
    pub key_id: Uuid,
    /// First 8 hex characters of the stored SHA-256 (hash of raw key).
    pub key_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Safe to expose to the control plane (fingerprint of newest key, for status rows).
#[derive(Debug, Clone, Serialize)]
pub struct McpApiKeyStatusPublic {
    /// First 8 hex chars of the hash (stable display / reconcile).
    pub key_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyRecordV2 {
    config_id: Uuid,
    hash_hex: String,
    api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct McpApiKeyRegistry {
    storage: Arc<dyn SecretStore>,
}

impl McpApiKeyRegistry {
    pub fn new(storage: Arc<dyn SecretStore>) -> Self {
        Self { storage }
    }

    fn fingerprint_from_hash_hex(full_hex: &str) -> String {
        full_hex.chars().take(8).collect()
    }

    /// Every key has a non-empty display name; trim, max 128 UTF-8 scalars.
    fn normalize_required_key_name(label: String) -> Result<String, SecretStoreError> {
        let t = label.trim();
        if t.is_empty() {
            return Err(SecretStoreError::InvalidInput(
                "MCP API key name is required".to_string(),
            ));
        }
        Ok(t.chars().take(128).collect())
    }

    async fn read_id_list(&self, config_id: Uuid) -> Result<Vec<Uuid>, SecretStoreError> {
        let raw = self.storage.get_kv(&set_key(config_id)).await?;
        let Some(bytes) = raw else {
            return Ok(Vec::new());
        };
        let s = String::from_utf8_lossy(&bytes);
        if s.trim().is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Uuid> = serde_json::from_str(s.trim()).map_err(SecretStoreError::from)?;
        Ok(ids)
    }

    async fn write_id_list(&self, config_id: Uuid, ids: &[Uuid]) -> Result<(), SecretStoreError> {
        let s = serde_json::to_string(ids).map_err(SecretStoreError::from)?;
        self.storage
            .store_kv(&set_key(config_id), s.as_bytes(), None)
            .await?;
        Ok(())
    }

    /// Removes every key for this config (MCP config revoke / rotate-all / disable).
    pub async fn revoke_for_config(&self, config_id: Uuid) -> Result<(), SecretStoreError> {
        let ids = self.read_id_list(config_id).await?;
        for k in ids {
            self.remove_one_key(k).await?;
        }
        self.storage.delete_kv(&set_key(config_id)).await?;
        Ok(())
    }

    /// Deletes one key: hash index, record, and membership in the config set.
    async fn remove_one_key(&self, key_id: Uuid) -> Result<(), SecretStoreError> {
        let rkey = key_rec_key(key_id);
        let Some(bytes) = self.storage.get_kv(&rkey).await? else {
            return Ok(());
        };
        let rec: KeyRecordV2 = serde_json::from_slice(&bytes).map_err(SecretStoreError::from)?;
        self.storage
            .delete_kv(&hash_to_key_id_kv(&rec.hash_hex))
            .await?;
        self.storage.delete_kv(&rkey).await?;

        let mut list = self.read_id_list(rec.config_id).await?;
        list.retain(|x| *x != key_id);
        if list.is_empty() {
            self.storage.delete_kv(&set_key(rec.config_id)).await?;
        } else {
            self.write_id_list(rec.config_id, &list).await?;
        }
        Ok(())
    }

    /// Append a new API key. Does not revoke existing keys. `label` is required (non-empty after trim).
    pub async fn add_key(
        &self,
        config_id: Uuid,
        label: String,
    ) -> Result<McpApiKeyProvisioned, SecretStoreError> {
        let name = Self::normalize_required_key_name(label)?;
        let key_id = Uuid::new_v4();
        let api_key = format!("plasm_mcp_{}", Uuid::new_v4().simple());
        let hash_hex = sha256_hex(api_key.as_bytes());
        let created_at = Utc::now();
        let rec = KeyRecordV2 {
            config_id,
            hash_hex: hash_hex.clone(),
            api_key: api_key.clone(),
            label: Some(name),
            created_at,
        };
        let json = serde_json::to_vec(&rec).map_err(SecretStoreError::from)?;

        self.storage
            .store_kv(
                &hash_to_key_id_kv(&hash_hex),
                key_id.to_string().as_bytes(),
                None,
            )
            .await?;
        self.storage
            .store_kv(&key_rec_key(key_id), &json, None)
            .await?;

        let mut list = self.read_id_list(config_id).await?;
        list.push(key_id);
        self.write_id_list(config_id, &list).await?;

        Ok(McpApiKeyProvisioned { key_id, api_key })
    }

    /// Rename a key. `label` is required (non-empty after trim); key material is unchanged.
    pub async fn set_key_label(
        &self,
        config_id: Uuid,
        key_id: Uuid,
        label: String,
    ) -> Result<(), SecretStoreError> {
        let name = Self::normalize_required_key_name(label)?;
        let rbytes = self
            .storage
            .get_kv(&key_rec_key(key_id))
            .await?
            .ok_or(SecretStoreError::NotFound)?;
        let mut rec: KeyRecordV2 =
            serde_json::from_slice(&rbytes).map_err(SecretStoreError::from)?;
        if rec.config_id != config_id {
            return Err(SecretStoreError::InvalidInput(
                "MCP key does not belong to this configuration".to_string(),
            ));
        }
        rec.label = Some(name);
        let json = serde_json::to_vec(&rec).map_err(SecretStoreError::from)?;
        self.storage
            .store_kv(&key_rec_key(key_id), &json, None)
            .await?;
        Ok(())
    }

    /// Revoke all keys for the config, then add one (nuclear “replace all” rotation). New key name is required.
    pub async fn rotate_all_for_config(
        &self,
        config_id: Uuid,
        new_key_label: String,
    ) -> Result<McpApiKeyProvisioned, SecretStoreError> {
        self.revoke_for_config(config_id).await?;
        self.add_key(config_id, new_key_label).await
    }

    /// Revoke one key, then add a new key (per-key “rotate”). New key name is required.
    /// If `add_key` fails after `revoke_one`, that key slot is already removed.
    pub async fn rotate_one_for_config(
        &self,
        config_id: Uuid,
        key_id: Uuid,
        new_key_label: String,
    ) -> Result<McpApiKeyProvisioned, SecretStoreError> {
        self.revoke_one_api_key(config_id, key_id).await?;
        self.add_key(config_id, new_key_label).await
    }

    /// Resolve `config_id` when the presented key matches a stored hash.
    pub async fn verify_api_key(&self, raw: &str) -> Option<Uuid> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let hash_hex = sha256_hex(trimmed.as_bytes());
        let key_id_bytes = self
            .storage
            .get_kv(&hash_to_key_id_kv(&hash_hex))
            .await
            .ok()??;
        let key_id: Uuid = Uuid::parse_str(std::str::from_utf8(&key_id_bytes).ok()?.trim()).ok()?;
        let rbytes = self.storage.get_kv(&key_rec_key(key_id)).await.ok()??;
        let rec: KeyRecordV2 = serde_json::from_slice(&rbytes).ok()?;
        if rec.api_key == trimmed {
            return Some(rec.config_id);
        }
        None
    }

    async fn list_items_for_config(
        &self,
        config_id: Uuid,
    ) -> Result<Vec<McpApiKeyListItem>, SecretStoreError> {
        let mut out = Vec::new();
        for id in self.read_id_list(config_id).await? {
            let rkey = key_rec_key(id);
            let rbytes = self.storage.get_kv(&rkey).await?.ok_or_else(|| {
                SecretStoreError::InvalidInput("missing MCP API key record".to_string())
            })?;
            let mut rec: KeyRecordV2 =
                serde_json::from_slice(&rbytes).map_err(SecretStoreError::from)?;
            if rec.config_id != config_id {
                return Err(SecretStoreError::InvalidInput(
                    "MCP key record / set mismatch".to_string(),
                ));
            }
            let fp = Self::fingerprint_from_hash_hex(&rec.hash_hex);
            let label = match rec.label.as_deref() {
                Some(s) if !s.trim().is_empty() => s.trim().chars().take(128).collect::<String>(),
                _ => {
                    let d = format!("Key {fp}");
                    rec.label = Some(d.clone());
                    let json = serde_json::to_vec(&rec).map_err(SecretStoreError::from)?;
                    self.storage.store_kv(&rkey, &json, None).await?;
                    d
                }
            };
            out.push(McpApiKeyListItem {
                key_id: id,
                key_fingerprint: fp,
                label: Some(label),
                created_at: rec.created_at,
            });
        }
        Ok(out)
    }

    /// Public rows for a config (no raw secrets).
    pub async fn list_api_keys(
        &self,
        config_id: Uuid,
    ) -> Result<Vec<McpApiKeyListItem>, SecretStoreError> {
        self.list_items_for_config(config_id).await
    }

    /// Re-read raw key material (control plane only; audit at HTTP layer).
    pub async fn reveal_api_key(
        &self,
        config_id: Uuid,
        key_id: Uuid,
    ) -> Result<String, SecretStoreError> {
        let rbytes = self
            .storage
            .get_kv(&key_rec_key(key_id))
            .await?
            .ok_or(SecretStoreError::NotFound)?;
        let rec: KeyRecordV2 = serde_json::from_slice(&rbytes).map_err(SecretStoreError::from)?;
        if rec.config_id != config_id {
            return Err(SecretStoreError::InvalidInput(
                "MCP key does not belong to this configuration".to_string(),
            ));
        }
        Ok(rec.api_key)
    }

    /// Revoke a single key.
    pub async fn revoke_one_api_key(
        &self,
        config_id: Uuid,
        key_id: Uuid,
    ) -> Result<(), SecretStoreError> {
        let rbytes = self
            .storage
            .get_kv(&key_rec_key(key_id))
            .await?
            .ok_or(SecretStoreError::NotFound)?;
        let rec: KeyRecordV2 = serde_json::from_slice(&rbytes).map_err(SecretStoreError::from)?;
        if rec.config_id != config_id {
            return Err(SecretStoreError::InvalidInput(
                "MCP key does not belong to this configuration".to_string(),
            ));
        }
        self.remove_one_key(key_id).await
    }

    /// Newest key by append order, or `None` if there are no keys.
    pub async fn public_status_for_config(
        &self,
        config_id: Uuid,
    ) -> Result<Option<McpApiKeyStatusPublic>, SecretStoreError> {
        let ids = self.read_id_list(config_id).await?;
        let Some(&last_id) = ids.last() else {
            return Ok(None);
        };
        let rbytes = self
            .storage
            .get_kv(&key_rec_key(last_id))
            .await?
            .ok_or_else(|| {
                SecretStoreError::InvalidInput("missing key record for status".to_string())
            })?;
        let rec: KeyRecordV2 = serde_json::from_slice(&rbytes).map_err(SecretStoreError::from)?;
        Ok(Some(McpApiKeyStatusPublic {
            key_fingerprint: Self::fingerprint_from_hash_hex(&rec.hash_hex),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_store::MemorySecretStore;

    #[tokio::test]
    async fn rotate_one_replaces_key_others_unchanged() {
        let storage = Arc::new(MemorySecretStore::new()) as Arc<dyn SecretStore>;
        let reg = McpApiKeyRegistry::new(storage);
        let cid = Uuid::new_v4();
        let a = reg.add_key(cid, "A".to_string()).await.expect("a");
        let b = reg.add_key(cid, "B".to_string()).await.expect("b");
        let out = reg
            .rotate_one_for_config(cid, a.key_id, "A2".to_string())
            .await
            .expect("rot");
        assert_ne!(out.api_key, a.api_key);
        assert_eq!(reg.verify_api_key(&a.api_key).await, None);
        assert_eq!(reg.verify_api_key(&b.api_key).await, Some(cid));
        assert_eq!(reg.verify_api_key(&out.api_key).await, Some(cid));
        let list = reg.list_api_keys(cid).await.expect("list");
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn add_two_keys_both_verify_list_reveal() {
        let storage = Arc::new(MemorySecretStore::new()) as Arc<dyn SecretStore>;
        let reg = McpApiKeyRegistry::new(storage);
        let cid = Uuid::new_v4();
        let a = reg.add_key(cid, "A".to_string()).await.expect("a");
        let b = reg.add_key(cid, "B".to_string()).await.expect("b");
        assert_ne!(a.api_key, b.api_key);
        assert_eq!(reg.verify_api_key(&a.api_key).await, Some(cid));
        assert_eq!(reg.verify_api_key(&b.api_key).await, Some(cid));
        let list = reg.list_api_keys(cid).await.expect("list");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].label.as_deref(), Some("A"));
        assert_eq!(list[1].label.as_deref(), Some("B"));
        assert_eq!(
            reg.reveal_api_key(cid, a.key_id).await.expect("r"),
            a.api_key
        );
    }

    #[tokio::test]
    async fn add_key_rejects_empty_name() {
        let storage = Arc::new(MemorySecretStore::new()) as Arc<dyn SecretStore>;
        let reg = McpApiKeyRegistry::new(storage);
        let cid = Uuid::new_v4();
        assert!(reg.add_key(cid, "  \n".to_string()).await.is_err());
    }

    #[tokio::test]
    async fn rotate_all_invalidates_all_prior() {
        let storage = Arc::new(MemorySecretStore::new()) as Arc<dyn SecretStore>;
        let reg = McpApiKeyRegistry::new(storage);
        let cid = Uuid::new_v4();
        let a = reg.add_key(cid, "A".to_string()).await.expect("a");
        let c = reg
            .rotate_all_for_config(cid, "After rotation".to_string())
            .await
            .expect("r");
        assert_eq!(reg.verify_api_key(&a.api_key).await, None);
        assert_eq!(reg.verify_api_key(&c.api_key).await, Some(cid));
        let list = reg.list_api_keys(cid).await.expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].label.as_deref(), Some("After rotation"));
    }

    #[tokio::test]
    async fn revoke_for_config() {
        let storage = Arc::new(MemorySecretStore::new()) as Arc<dyn SecretStore>;
        let reg = McpApiKeyRegistry::new(storage);
        let cid = Uuid::new_v4();
        let a = reg.add_key(cid, "A".to_string()).await.expect("a");
        reg.revoke_for_config(cid).await.expect("v");
        assert_eq!(reg.verify_api_key(&a.api_key).await, None);
        assert_eq!(reg.list_api_keys(cid).await.expect("e").len(), 0);
    }

    #[tokio::test]
    async fn revoke_one_of_two() {
        let storage = Arc::new(MemorySecretStore::new()) as Arc<dyn SecretStore>;
        let reg = McpApiKeyRegistry::new(storage);
        let cid = Uuid::new_v4();
        let a = reg.add_key(cid, "A".to_string()).await.expect("a");
        let b = reg.add_key(cid, "B".to_string()).await.expect("b");
        reg.revoke_one_api_key(cid, a.key_id).await.expect("r");
        assert_eq!(reg.verify_api_key(&a.api_key).await, None);
        assert_eq!(reg.verify_api_key(&b.api_key).await, Some(cid));
    }

    #[tokio::test]
    async fn set_key_label_updates_list() {
        let storage = Arc::new(MemorySecretStore::new()) as Arc<dyn SecretStore>;
        let reg = McpApiKeyRegistry::new(storage);
        let cid = Uuid::new_v4();
        let a = reg.add_key(cid, "A".to_string()).await.expect("a");
        reg.set_key_label(cid, a.key_id, "Work laptop".to_string())
            .await
            .expect("label");
        let list = reg.list_api_keys(cid).await.expect("list");
        assert_eq!(list[0].label.as_deref(), Some("Work laptop"));
        assert!(reg
            .set_key_label(cid, a.key_id, "".to_string())
            .await
            .is_err());
    }
}
