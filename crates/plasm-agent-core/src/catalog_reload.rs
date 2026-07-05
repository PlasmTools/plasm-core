//! Catalog-dir hot reload: load, diff, publish, and invalidate derived caches.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use indexmap::IndexMap;
use plasm_core::discovery::{CgsCatalog, InMemoryCgsRegistry};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::catalog_data::load_registry_from_catalog_dir;
use crate::server_state::PlasmHostState;

#[derive(Debug, Error)]
pub enum CatalogReloadError {
    #[error("catalog reload requires --catalog-dir bootstrap")]
    NotCatalogDir,
    #[error(transparent)]
    Pool(#[from] crate::blocking_compute::ComputePoolError),
    #[error("catalog load failed: {0}")]
    Load(String),
}

#[derive(Debug, Clone)]
pub struct CatalogReloadReport {
    pub generation: u64,
    pub entry_count: usize,
    pub entry_ids: Vec<String>,
    pub added_entry_ids: Vec<String>,
    pub removed_entry_ids: Vec<String>,
    pub changed_entry_ids: Vec<String>,
    pub catalog_changed: bool,
    pub session_keys_purged: u64,
    pub logical_keys_purged: u64,
}

fn entry_hash_map(reg: &InMemoryCgsRegistry) -> IndexMap<String, String> {
    let mut m = IndexMap::new();
    for meta in reg.list_entries() {
        if !meta.catalog_cgs_hash.is_empty() {
            m.insert(meta.entry_id.clone(), meta.catalog_cgs_hash.clone());
        } else if let Ok(ctx) = reg.load_context(&meta.entry_id) {
            m.insert(meta.entry_id.clone(), ctx.cgs.catalog_cgs_hash_hex());
        }
    }
    m
}

fn diff_registry_maps(
    old: &IndexMap<String, String>,
    new: &IndexMap<String, String>,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let old_ids: HashSet<_> = old.keys().cloned().collect();
    let new_ids: HashSet<_> = new.keys().cloned().collect();
    let mut added: Vec<String> = new_ids.difference(&old_ids).cloned().collect();
    added.sort();
    let mut removed: Vec<String> = old_ids.difference(&new_ids).cloned().collect();
    removed.sort();
    let mut changed = Vec::new();
    for id in old_ids.intersection(&new_ids) {
        if old.get(id) != new.get(id) {
            changed.push(id.clone());
        }
    }
    changed.sort();
    (added, removed, changed)
}

impl PlasmHostState {
    /// Single-flight catalog-dir reload with all derived-cache invalidation.
    pub async fn reload_catalog_registry(&self) -> Result<CatalogReloadReport, CatalogReloadError> {
        let _guard = self.catalog_reload_lock().lock().await;
        let path = self
            .catalog
            .catalog_dir_path()
            .ok_or(CatalogReloadError::NotCatalogDir)?;
        self.reload_catalog_registry_at(path).await
    }

    async fn reload_catalog_registry_at(
        &self,
        path: &Path,
    ) -> Result<CatalogReloadReport, CatalogReloadError> {
        let prev = self.catalog.snapshot();
        let old_hashes = entry_hash_map(prev.as_ref());

        let path_buf = path.to_path_buf();
        let new_reg = self
            .blocking_compute()
            .run("load_registry_from_catalog_dir", move || {
                load_registry_from_catalog_dir(&path_buf)
            })
            .await?
            .map_err(CatalogReloadError::Load)?;

        let new_hashes = entry_hash_map(&new_reg);
        let (added_entry_ids, removed_entry_ids, changed_entry_ids) =
            diff_registry_maps(&old_hashes, &new_hashes);

        let entry_ids: Vec<String> = new_reg
            .list_entries()
            .into_iter()
            .map(|m| m.entry_id)
            .collect();
        let entry_count = entry_ids.len();
        let catalog_changed = !added_entry_ids.is_empty()
            || !removed_entry_ids.is_empty()
            || !changed_entry_ids.is_empty();

        self.catalog.publish_catalog(Arc::new(new_reg));
        self.invalidate_catalog_derived_caches();

        let generation = self.catalog.bump_reload_generation();
        let (session_keys_purged, logical_keys_purged) = if catalog_changed {
            self.sessions.invalidate_cgs_derived_caches();
            self.purge_persisted_execute_state().await
        } else {
            (0, 0)
        };

        Ok(CatalogReloadReport {
            generation,
            entry_count,
            entry_ids,
            added_entry_ids,
            removed_entry_ids,
            changed_entry_ids,
            catalog_changed,
            session_keys_purged,
            logical_keys_purged,
        })
    }

    /// Tool-model memo + typed-discovery index caches (after catalog digest rotation).
    pub fn invalidate_catalog_derived_caches(&self) {
        self.tool_model_service().clear_cache();
        self.discovery_index_cache().clear();
    }

    pub(crate) fn catalog_reload_lock(&self) -> &Arc<Mutex<()>> {
        &self.oss.catalog_reload_lock
    }
}
