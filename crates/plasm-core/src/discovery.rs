//! Catalog storage for compilation and explicit execution. Discovery is PostgreSQL-owned.

use crate::{CgsContext, CGS};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("unknown catalog entry: {0}")]
    UnknownEntry(String),
    #[error("intent must be non-empty")]
    EmptyQuery,
}

/// Metadata for one catalog row (no full [`CGS`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntryMeta {
    pub entry_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Stable digest of the loaded CGS (`CGS::catalog_cgs_hash_hex`); bumps when the graph changes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub catalog_cgs_hash: String,
}

pub trait CgsCatalog: Send + Sync {
    fn list_entries(&self) -> Vec<CatalogEntryMeta>;
    fn load_context(&self, entry_id: &str) -> Result<CgsContext, DiscoveryError>;
    /// Metadata for one catalog row without building the full [`list_entries`](Self::list_entries) vec.
    fn lookup_entry_meta(&self, entry_id: &str) -> Option<CatalogEntryMeta>;
}

struct RegistryRow {
    label: String,
    tags: Vec<String>,
    aliases: Vec<String>,
    cgs: Arc<CGS>,
    catalog_cgs_hash: String,
}

pub type RegistryEntryPair = (String, String, Vec<String>, Arc<CGS>);

/// Loaded catalog graphs; contains no retrieval index or semantic selection policy.
pub struct CgsRegistry {
    entries: IndexMap<String, RegistryRow>,
}

impl CgsRegistry {
    pub fn from_pairs(pairs: Vec<RegistryEntryPair>) -> Self {
        Self {
            entries: pairs
                .into_iter()
                .map(|(id, label, tags, cgs)| {
                    let row = RegistryRow {
                        label,
                        tags,
                        aliases: cgs.registry_aliases.clone(),
                        catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
                        cgs,
                    };
                    (id, row)
                })
                .collect(),
        }
    }

    pub fn entry_ids(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    pub fn catalog_arcs(&self) -> IndexMap<String, Arc<CGS>> {
        self.entries
            .iter()
            .map(|(id, row)| (id.clone(), row.cgs.clone()))
            .collect()
    }

    pub fn cgs_arc(&self, entry_id: &str) -> Option<Arc<CGS>> {
        self.entries.get(entry_id).map(|row| row.cgs.clone())
    }

    /// Resolve explicit identifiers only. Search aliases do not grant catalog admission.
    pub fn resolve_entry_id(
        &self,
        raw: &str,
        allowed: Option<&[String]>,
    ) -> Result<String, DiscoveryError> {
        if self.entries.contains_key(raw)
            && allowed.is_none_or(|ids| ids.iter().any(|id| id == raw))
        {
            Ok(raw.to_string())
        } else {
            Err(DiscoveryError::UnknownEntry(raw.to_string()))
        }
    }

    pub fn first_cgs(&self) -> Option<Arc<CGS>> {
        self.entries.first().map(|(_, row)| row.cgs.clone())
    }
}

impl CgsCatalog for CgsRegistry {
    fn list_entries(&self) -> Vec<CatalogEntryMeta> {
        self.entries
            .iter()
            .map(|(id, row)| CatalogEntryMeta {
                entry_id: id.clone(),
                label: row.label.clone(),
                tags: row.tags.clone(),
                aliases: row.aliases.clone(),
                catalog_cgs_hash: row.catalog_cgs_hash.clone(),
            })
            .collect()
    }

    fn lookup_entry_meta(&self, entry_id: &str) -> Option<CatalogEntryMeta> {
        self.entries.get(entry_id).map(|row| CatalogEntryMeta {
            entry_id: entry_id.to_string(),
            label: row.label.clone(),
            tags: row.tags.clone(),
            aliases: row.aliases.clone(),
            catalog_cgs_hash: row.catalog_cgs_hash.clone(),
        })
    }

    fn load_context(&self, entry_id: &str) -> Result<CgsContext, DiscoveryError> {
        let row = self
            .entries
            .get(entry_id)
            .ok_or_else(|| DiscoveryError::UnknownEntry(entry_id.to_string()))?;
        Ok(CgsContext::entry(entry_id, row.cgs.clone()))
    }
}
