use serde::{Deserialize, Serialize};

/// Session-scoped fields shared by every segment in a bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceScope {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tenant_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_session_id: Option<String>,
    pub prompt_hash: String,
    pub execute_session_id: String,
    pub catalog_cgs_hash: String,
    pub domain_revision: u32,
    pub entry_id: String,
}

impl EvidenceScope {
    pub fn new_v1(
        prompt_hash: impl Into<String>,
        execute_session_id: impl Into<String>,
        catalog_cgs_hash: impl Into<String>,
        domain_revision: u32,
        entry_id: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: 1,
            tenant_id: String::new(),
            logical_session_id: None,
            prompt_hash: prompt_hash.into(),
            execute_session_id: execute_session_id.into(),
            catalog_cgs_hash: catalog_cgs_hash.into(),
            domain_revision,
            entry_id: entry_id.into(),
        }
    }
}
