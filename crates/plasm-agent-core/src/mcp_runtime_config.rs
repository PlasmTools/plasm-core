//! Tenant MCP configuration payloads (`McpRuntimeConfig`) and control-plane upsert JSON.

use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Runtime snapshot for one MCP configuration (mirrors Phoenix `ProjectMcp.payload_for_agent/1`).
#[derive(Debug, Clone)]
pub struct McpRuntimeConfig {
    pub id: Uuid,
    pub tenant_id: String,
    pub workspace_slug: String,
    pub project_slug: String,
    pub space_type: String,
    pub owner_subject: Option<String>,
    pub version: u64,
    pub endpoint_secret_hash: [u8; 32],
    /// SHA-256(raw_credential_secret) for active long-lived MCP credentials.
    pub credential_secret_hashes: HashSet<[u8; 32]>,
    pub allowed_entry_ids: HashSet<String>,
    /// Per entry_id: empty set means "all capabilities" in that graph.
    pub capabilities_by_entry: HashMap<String, HashSet<String>>,
    /// Outbound auth config id per registry `entry_id` (Phoenix `ProjectMcpAuthBinding`).
    pub auth_config_by_entry: HashMap<String, Uuid>,
}

impl McpRuntimeConfig {
    pub fn entry_allowed(&self, entry_id: &str) -> bool {
        self.allowed_entry_ids.contains(entry_id)
    }

    pub fn capability_allowed(&self, entry_id: &str, capability_name: &str) -> bool {
        match self.capabilities_by_entry.get(entry_id) {
            None => true,
            Some(set) if set.is_empty() => true,
            Some(set) => set.contains(capability_name),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct McpConfigUpsertJson {
    pub id: Uuid,
    pub tenant_id: String,
    #[serde(default = "default_space_type")]
    pub space_type: String,
    #[serde(default)]
    pub owner_subject: Option<String>,
    pub version: u64,
    pub endpoint_secret_hash_hex: String,
    #[serde(default)]
    pub credential_secret_hashes_hex: Vec<String>,
    pub allowed_entry_ids: Vec<String>,
    pub capabilities_by_entry: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub auth_config_by_entry: HashMap<String, String>,
    /// Required for new configs; may be omitted in tests (defaults to `"default"`).
    #[serde(default)]
    pub workspace_slug: String,
    #[serde(default)]
    pub project_slug: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_status_active")]
    pub status: String,
    #[serde(default)]
    pub auth_optional_entry_ids: Vec<String>,
}

impl McpConfigUpsertJson {
    pub fn workspace_slug_resolved(&self) -> &str {
        let t = self.workspace_slug.trim();
        if t.is_empty() {
            "default"
        } else {
            t
        }
    }

    pub fn project_slug_resolved(&self) -> &str {
        let t = self.project_slug.trim();
        if t.is_empty() {
            "default"
        } else {
            t
        }
    }

    pub fn name_resolved(&self) -> &str {
        let t = self.name.trim();
        if t.is_empty() {
            "MCP config"
        } else {
            t
        }
    }

    pub fn status_normalized(&self) -> &str {
        let t = self.status.trim();
        if t.is_empty() {
            "active"
        } else {
            t
        }
    }

    pub fn auth_optional_ids_clean(&self) -> Vec<String> {
        self.auth_optional_entry_ids
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

fn default_space_type() -> String {
    "organization".to_string()
}

fn default_status_active() -> String {
    "active".to_string()
}

impl TryFrom<McpConfigUpsertJson> for McpRuntimeConfig {
    type Error = String;

    fn try_from(j: McpConfigUpsertJson) -> Result<Self, Self::Error> {
        let workspace_slug = j.workspace_slug_resolved().to_string();
        let project_slug = j.project_slug_resolved().to_string();
        let bytes = hex::decode(j.endpoint_secret_hash_hex.trim())
            .map_err(|e| format!("endpoint_secret_hash_hex: {e}"))?;
        if bytes.len() != 32 {
            return Err("endpoint_secret_hash_hex must be 32 bytes".into());
        }
        let mut endpoint_secret_hash = [0u8; 32];
        endpoint_secret_hash.copy_from_slice(&bytes);
        let mut allowed_entry_ids: HashSet<String> = j.allowed_entry_ids.into_iter().collect();
        allowed_entry_ids.retain(|s| !s.is_empty());
        let mut capabilities_by_entry: HashMap<String, HashSet<String>> = HashMap::new();
        for (k, v) in j.capabilities_by_entry {
            let set: HashSet<String> = v.into_iter().filter(|s| !s.is_empty()).collect();
            capabilities_by_entry.insert(k, set);
        }
        let mut credential_secret_hashes: HashSet<[u8; 32]> = HashSet::new();
        for hex_s in j.credential_secret_hashes_hex {
            let bytes = hex::decode(hex_s.trim()).map_err(|e| format!("credential hash: {e}"))?;
            if bytes.len() != 32 {
                return Err("credential hash must be 32 bytes".into());
            }
            let mut h = [0u8; 32];
            h.copy_from_slice(&bytes);
            credential_secret_hashes.insert(h);
        }
        let mut auth_config_by_entry: HashMap<String, Uuid> = HashMap::new();
        for (entry_id, uuid_s) in j.auth_config_by_entry {
            if entry_id.is_empty() {
                continue;
            }
            let u = Uuid::parse_str(uuid_s.trim())
                .map_err(|e| format!("auth_config_by_entry[{entry_id}]: {e}"))?;
            auth_config_by_entry.insert(entry_id, u);
        }
        Ok(McpRuntimeConfig {
            id: j.id,
            tenant_id: j.tenant_id,
            workspace_slug,
            project_slug,
            space_type: match j.space_type.trim() {
                "personal" => "personal".to_string(),
                _ => "organization".to_string(),
            },
            owner_subject: j.owner_subject.and_then(|s| {
                let trimmed = s.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }),
            version: j.version,
            endpoint_secret_hash,
            credential_secret_hashes,
            allowed_entry_ids,
            capabilities_by_entry,
            auth_config_by_entry,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_json_default_empty_credentials() {
        let j = McpConfigUpsertJson {
            id: Uuid::nil(),
            tenant_id: "t".into(),
            space_type: "organization".into(),
            owner_subject: None,
            version: 1,
            endpoint_secret_hash_hex: "ab".repeat(32),
            credential_secret_hashes_hex: vec![],
            allowed_entry_ids: vec![],
            capabilities_by_entry: HashMap::new(),
            auth_config_by_entry: HashMap::new(),
            workspace_slug: String::new(),
            project_slug: String::new(),
            name: String::new(),
            status: String::new(),
            auth_optional_entry_ids: vec![],
        };
        let cfg: McpRuntimeConfig = j.try_into().expect("ok");
        assert!(cfg.credential_secret_hashes.is_empty());
        assert!(cfg.auth_config_by_entry.is_empty());
        assert_eq!(cfg.space_type, "organization");
    }

    #[test]
    fn upsert_json_parses_auth_config_by_entry() {
        let ac = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let mut auth = HashMap::new();
        auth.insert("github".into(), ac.to_string());
        let j = McpConfigUpsertJson {
            id: Uuid::nil(),
            tenant_id: "t".into(),
            space_type: "organization".into(),
            owner_subject: None,
            version: 1,
            endpoint_secret_hash_hex: "ab".repeat(32),
            credential_secret_hashes_hex: vec![],
            allowed_entry_ids: vec![],
            capabilities_by_entry: HashMap::new(),
            auth_config_by_entry: auth,
            workspace_slug: String::new(),
            project_slug: String::new(),
            name: String::new(),
            status: String::new(),
            auth_optional_entry_ids: vec![],
        };
        let cfg: McpRuntimeConfig = j.try_into().expect("ok");
        assert_eq!(cfg.auth_config_by_entry.get("github"), Some(&ac));
    }
}
