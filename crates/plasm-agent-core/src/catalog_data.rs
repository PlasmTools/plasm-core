//! Build an [`plasm_core::discovery::InMemoryCgsRegistry`] from compiled JSON catalog artifacts.

use plasm_core::catalog_il::{
    is_catalog_manifest_path, load_catalog_artifact, read_catalog_manifest, CatalogManifest,
};
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::schema::CGS;
use plasm_core::CgsCatalog;
use std::collections::hash_map::Entry;
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
}

impl LoadedCatalogEntry {
    fn into_registry_pair(self) -> (String, String, Vec<String>, Arc<CGS>) {
        (self.entry_id, self.label, self.tags, self.cgs)
    }
}

/// Validate capability CML templates for every entry in a loaded registry.
pub fn validate_registry_templates_with_progress<P: FnMut(&str)>(
    reg: &InMemoryCgsRegistry,
    progress: &mut P,
) -> Result<(), String> {
    let metas = reg.list_entries();
    let n = metas.len();
    progress(&format!("validating capability templates ({n} entries)…"));
    if n <= 1 {
        for meta in metas {
            validate_one_entry(reg, &meta.entry_id)?;
        }
        return Ok(());
    }

    let workers = catalog_materialize_workers();
    let err: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));

    for chunk in metas.chunks(workers) {
        std::thread::scope(|scope| {
            for meta in chunk {
                let entry_id = meta.entry_id.clone();
                let err = Arc::clone(&err);
                scope.spawn(move || {
                    if err.lock().expect("err lock").is_some() {
                        return;
                    }
                    if let Err(e) = validate_one_entry(reg, &entry_id) {
                        *err.lock().expect("err lock") = Some(e);
                    }
                });
            }
        });
        if let Some(e) = err.lock().expect("err lock").take() {
            return Err(e);
        }
    }
    Ok(())
}

fn validate_one_entry(reg: &InMemoryCgsRegistry, entry_id: &str) -> Result<(), String> {
    let ctx = reg
        .load_context(entry_id)
        .map_err(|e| e.to_string())?;
    plasm_compile::validate_cgs_capability_templates(ctx.cgs.as_ref())
        .map_err(|e| format!("{entry_id}: {e}"))
}

fn ingest_manifest_candidate(
    path: &Path,
    best_by_entry: &mut HashMap<String, (u64, CatalogManifest, PathBuf)>,
) -> Result<(), String> {
    let manifest = read_catalog_manifest(path)?;
    let ver = manifest.version;
    let eid = manifest.entry_id.clone();

    match best_by_entry.entry(eid.clone()) {
        Entry::Vacant(v) => {
            v.insert((ver, manifest, path.to_path_buf()));
        }
        Entry::Occupied(mut o) => {
            let (best_ver, best_meta, best_path) = o.get();
            if ver > *best_ver {
                o.insert((ver, manifest, path.to_path_buf()));
            } else if ver == *best_ver {
                if best_meta.cgs_hash != manifest.cgs_hash {
                    return Err(format!(
                        "conflicting catalogs for entry `{eid}` v{ver}: cgs_hash {} vs {}",
                        best_meta.cgs_hash, manifest.cgs_hash
                    ));
                }
                if path < best_path.as_path() {
                    o.insert((ver, manifest, path.to_path_buf()));
                }
            }
        }
    }
    Ok(())
}

/// Scan `dir` for catalog manifests, select the highest `CGS.version` per `entry_id`, validate
/// capability templates, and build a registry. Fails on the first invalid artifact.
pub fn load_registry_from_catalog_dir(dir: &Path) -> Result<InMemoryCgsRegistry, String> {
    load_registry_from_catalog_dir_with_progress(dir, &mut |_: &str| {})
}

/// Like [`load_registry_from_catalog_dir`], with progress callbacks.
pub fn load_registry_from_catalog_dir_with_progress<P: FnMut(&str)>(
    dir: &Path,
    progress: &mut P,
) -> Result<InMemoryCgsRegistry, String> {
    progress(&format!("scanning catalog-dir {}", dir.display()));
    let read = std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;

    let mut best_by_entry: HashMap<String, (u64, CatalogManifest, PathBuf)> = HashMap::new();
    let mut manifest_count = 0usize;

    for ent in read {
        let ent = ent.map_err(|e| format!("read_dir: {e}"))?;
        let path = ent.path();
        if !path.is_file() || !is_catalog_manifest_path(&path) {
            continue;
        }
        manifest_count += 1;
        ingest_manifest_candidate(&path, &mut best_by_entry)?;
    }

    progress(&format!(
        "found {manifest_count} catalog manifest(s); {} entry id(s) after version resolution",
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

    let reg = InMemoryCgsRegistry::from_pairs(
        entries
            .into_iter()
            .map(LoadedCatalogEntry::into_registry_pair)
            .collect(),
    );
    validate_registry_templates_with_progress(&reg, progress)?;
    Ok(reg)
}

fn materialize_one_entry(
    dir: &Path,
    meta: CatalogManifest,
) -> Result<LoadedCatalogEntry, String> {
    let cgs: CGS = load_catalog_artifact(dir, &meta)?;
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
        }
    }

    #[test]
    fn ingest_prefers_higher_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_manifest(
            dir.path(),
            "a.manifest.json",
            &manifest("github", 1, &"a".repeat(64)),
        );
        write_manifest(
            dir.path(),
            "b.manifest.json",
            &manifest("github", 2, &"b".repeat(64)),
        );
        let mut best = HashMap::new();
        ingest_manifest_candidate(&dir.path().join("a.manifest.json"), &mut best).unwrap();
        ingest_manifest_candidate(&dir.path().join("b.manifest.json"), &mut best).unwrap();
        assert_eq!(best["github"].0, 2);
        assert_eq!(best["github"].2, dir.path().join("b.manifest.json"));
    }

    #[test]
    fn ingest_keeps_higher_version_when_seen_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_manifest(
            dir.path(),
            "a.manifest.json",
            &manifest("github", 1, &"a".repeat(64)),
        );
        write_manifest(
            dir.path(),
            "b.manifest.json",
            &manifest("github", 2, &"b".repeat(64)),
        );
        let mut best = HashMap::new();
        ingest_manifest_candidate(&dir.path().join("b.manifest.json"), &mut best).unwrap();
        ingest_manifest_candidate(&dir.path().join("a.manifest.json"), &mut best).unwrap();
        assert_eq!(best["github"].0, 2);
        assert_eq!(best["github"].2, dir.path().join("b.manifest.json"));
    }

    #[test]
    fn ingest_rejects_same_version_conflicting_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_manifest(
            dir.path(),
            "a.manifest.json",
            &manifest("github", 3, &"a".repeat(64)),
        );
        write_manifest(
            dir.path(),
            "b.manifest.json",
            &manifest("github", 3, &"b".repeat(64)),
        );
        let mut best = HashMap::new();
        ingest_manifest_candidate(&dir.path().join("a.manifest.json"), &mut best).unwrap();
        let err =
            ingest_manifest_candidate(&dir.path().join("b.manifest.json"), &mut best).unwrap_err();
        assert!(err.contains("conflicting catalogs"));
    }

    #[test]
    fn ingest_same_version_same_hash_prefers_lexicographic_path() {
        let hash = "c".repeat(64);
        let dir = tempfile::tempdir().expect("tempdir");
        write_manifest(dir.path(), "z.manifest.json", &manifest("github", 1, &hash));
        write_manifest(dir.path(), "a.manifest.json", &manifest("github", 1, &hash));
        let mut best = HashMap::new();
        ingest_manifest_candidate(&dir.path().join("z.manifest.json"), &mut best).unwrap();
        ingest_manifest_candidate(&dir.path().join("a.manifest.json"), &mut best).unwrap();
        assert_eq!(best["github"].2, dir.path().join("a.manifest.json"));
    }
}
