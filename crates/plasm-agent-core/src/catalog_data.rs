//! Build an [`plasm_core::discovery::CgsRegistry`] from compiled JSON catalog artifacts.

use plasm_core::catalog_il::{
    load_catalog_artifact, read_catalog_manifest, read_catalog_set, CatalogManifest,
};
use plasm_core::discovery::CgsRegistry;
use plasm_core::schema::CGS;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::blocking_compute::catalog_materialize_workers;

/// One catalog entry materialized from compiled JSON IL on disk.
#[derive(Debug, Clone)]
pub struct LoadedCatalogEntry {
    pub entry_id: String,
    pub label: String,
    pub tags: Vec<String>,
    pub cgs: Arc<CGS>,
    pub compiled: Arc<plasm_compile::CompiledCatalog>,
}

impl LoadedCatalogEntry {
    fn into_registry_pair(self) -> (String, String, Vec<String>, Arc<CGS>) {
        (self.entry_id, self.label, self.tags, self.cgs)
    }
}

/// One validated, immutable catalog generation loaded from a format-3 manifest set.
#[derive(Clone)]
pub struct LoadedCatalogSet {
    pub registry: Arc<CgsRegistry>,
    pub compiled_by_entry: Arc<HashMap<String, Arc<plasm_compile::CompiledCatalog>>>,
}

fn ingest_manifest_candidate(
    path: &Path,
    best_by_entry: &mut HashMap<String, (u64, CatalogManifest, PathBuf)>,
) -> Result<(), String> {
    let manifest = read_catalog_manifest(path)?;
    let ver = manifest.version;
    let eid = manifest.entry_id.clone();

    if best_by_entry.contains_key(&eid) {
        return Err(format!(
            "catalog set declares multiple revisions for entry {eid}"
        ));
    }
    best_by_entry.insert(eid, (ver, manifest, path.to_path_buf()));
    Ok(())
}

/// Scan `dir` for catalog manifests, load the exact declared revision per `entry_id`, validate
/// capability templates, and build a registry. Fails on the first invalid artifact.
pub fn load_registry_from_catalog_dir(dir: &Path) -> Result<Arc<CgsRegistry>, String> {
    let loaded = load_catalog_set_from_dir_with_progress(dir, &mut |_: &str| {})?;
    Ok(loaded.registry)
}

/// Like [`load_registry_from_catalog_dir`], with progress callbacks.
pub fn load_registry_from_catalog_dir_with_progress<P: FnMut(&str)>(
    dir: &Path,
    progress: &mut P,
) -> Result<Arc<CgsRegistry>, String> {
    let loaded = load_catalog_set_from_dir_with_progress(dir, progress)?;
    Ok(loaded.registry)
}

/// Load CGS and precompiled request recipes as one validated generation.
pub fn load_catalog_set_from_dir_with_progress<P: FnMut(&str)>(
    dir: &Path,
    progress: &mut P,
) -> Result<LoadedCatalogSet, String> {
    progress(&format!("scanning catalog-dir {}", dir.display()));
    let manifest_paths = read_catalog_set(dir)?;

    let mut best_by_entry: HashMap<String, (u64, CatalogManifest, PathBuf)> = HashMap::new();
    let mut manifest_count = 0usize;

    for path in manifest_paths {
        manifest_count += 1;
        ingest_manifest_candidate(&path, &mut best_by_entry)?;
    }

    progress(&format!(
        "found {manifest_count} catalog manifest(s); {} entry id(s) in the declared catalog set",
        best_by_entry.len()
    ));

    if best_by_entry.is_empty() {
        return Err(format!("no loadable catalogs in `{}`", dir.display()));
    }

    progress("materializing CGS entries from compiled JSON IL…");

    let mut ids: Vec<String> = best_by_entry.keys().cloned().collect();
    ids.sort();

    let entries = if ids.len() <= 1 {
        materialize_entries_sequential(dir, &mut best_by_entry, &ids)?
    } else {
        materialize_entries_parallel(dir, best_by_entry, &ids)?
    };

    let compiled_by_entry = entries
        .iter()
        .map(|entry| (entry.entry_id.clone(), entry.compiled.clone()))
        .collect();
    let reg = Arc::new(CgsRegistry::from_pairs(
        entries
            .into_iter()
            .map(LoadedCatalogEntry::into_registry_pair)
            .collect(),
    ));
    Ok(LoadedCatalogSet {
        registry: reg,
        compiled_by_entry: Arc::new(compiled_by_entry),
    })
}

fn materialize_one_entry(dir: &Path, meta: CatalogManifest) -> Result<LoadedCatalogEntry, String> {
    let cgs: CGS = load_catalog_artifact(dir, &meta)?;
    let compiled = plasm_compile::load_compiled_catalog_artifact(dir, &meta, &cgs)
        .map_err(|error| error.to_string())?;
    let label = if meta.label.is_empty() {
        meta.entry_id.clone()
    } else {
        meta.label.clone()
    };
    Ok(LoadedCatalogEntry {
        entry_id: meta.entry_id,
        label,
        tags: meta.tags,
        cgs: Arc::new(cgs),
        compiled: Arc::new(compiled),
    })
}

fn materialize_entries_sequential(
    dir: &Path,
    best_by_entry: &mut HashMap<String, (u64, CatalogManifest, PathBuf)>,
    ids: &[String],
) -> Result<Vec<LoadedCatalogEntry>, String> {
    let mut entries = Vec::with_capacity(ids.len());
    for id in ids {
        let (_ver, meta, _manifest_path) = best_by_entry.remove(id).expect("key exists");
        entries.push(materialize_one_entry(dir, meta)?);
    }
    Ok(entries)
}

fn materialize_entries_parallel(
    dir: &Path,
    mut best_by_entry: HashMap<String, (u64, CatalogManifest, PathBuf)>,
    ids: &[String],
) -> Result<Vec<LoadedCatalogEntry>, String> {
    let dir = dir.to_path_buf();
    let workers = catalog_materialize_workers();
    let mut entries = Vec::with_capacity(ids.len());

    for chunk in ids.chunks(workers) {
        let batch: Arc<std::sync::Mutex<Vec<LoadedCatalogEntry>>> =
            Arc::new(std::sync::Mutex::new(Vec::with_capacity(chunk.len())));
        let err: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));

        std::thread::scope(|scope| {
            for id in chunk {
                let (_ver, meta, _manifest_path) = best_by_entry.remove(id).expect("key exists");
                let dir = dir.clone();
                let batch = Arc::clone(&batch);
                let err = Arc::clone(&err);
                scope.spawn(move || {
                    if err.lock().expect("err lock").is_some() {
                        return;
                    }
                    match materialize_one_entry(&dir, meta) {
                        Ok(entry) => batch.lock().expect("batch lock").push(entry),
                        Err(e) => *err.lock().expect("err lock") = Some(e),
                    }
                });
            }
        });

        if let Some(e) = err.lock().expect("err lock").take() {
            return Err(e);
        }
        entries.extend(
            Arc::try_unwrap(batch)
                .map_err(|_| "parallel catalog materialize: batch mutex still shared".to_string())?
                .into_inner()
                .expect("batch lock"),
        );
    }

    entries.sort_by(|a, b| a.entry_id.cmp(&b.entry_id));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::catalog_il::{CatalogManifest, PLASM_CATALOG_FORMAT_VERSION};

    fn write_manifest(dir: &Path, name: &str, manifest: &CatalogManifest) {
        let path = dir.join(name);
        let json = serde_json::to_string(manifest).expect("manifest json");
        std::fs::write(path, json).expect("write manifest");
    }

    fn manifest(entry_id: &str, version: u64, cgs_hash: &str) -> CatalogManifest {
        CatalogManifest {
            format_version: PLASM_CATALOG_FORMAT_VERSION,
            entry_id: entry_id.into(),
            version,
            cgs_hash: cgs_hash.into(),
            label: String::new(),
            tags: vec![],
            cgs_json: format!("{entry_id}.v{version}.deadbeefcafe.cgs.json"),
            recipes_json: format!("{entry_id}.recipes.json"),
            recipes_hash: "c".repeat(64),
            discovery_json: format!("{entry_id}.discovery.json"),
            discovery_hash: "b".repeat(64),
            embedding_profile: Default::default(),
        }
    }

    #[test]
    fn catalog_set_rejects_multiple_revisions_instead_of_choosing_one() {
        for versions in [[1, 2], [2, 1], [1, 1]] {
            let dir = tempfile::tempdir().unwrap();
            write_manifest(
                dir.path(),
                "a.manifest.json",
                &manifest("matrix", versions[0], &"a".repeat(64)),
            );
            write_manifest(
                dir.path(),
                "b.manifest.json",
                &manifest("matrix", versions[1], &"b".repeat(64)),
            );
            let mut entries = HashMap::new();
            ingest_manifest_candidate(&dir.path().join("a.manifest.json"), &mut entries).unwrap();
            assert!(
                ingest_manifest_candidate(&dir.path().join("b.manifest.json"), &mut entries)
                    .unwrap_err()
                    .contains("multiple revisions")
            );
        }
    }
}
