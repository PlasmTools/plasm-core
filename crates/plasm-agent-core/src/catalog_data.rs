//! Build an [`plasm_core::discovery::InMemoryCgsRegistry`] from compiled CBOR catalog artifacts.

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

/// Validate capability CML templates for every entry in a loaded registry.
pub fn validate_registry_templates_with_progress<P: FnMut(&str)>(
    reg: &InMemoryCgsRegistry,
    progress: &mut P,
) -> Result<(), String> {
    let metas = reg.list_entries();
    let n = metas.len();
    progress(&format!("validating capability templates ({n} entries)…"));
    for meta in metas {
        let ctx = reg
            .load_context(&meta.entry_id)
            .map_err(|e| e.to_string())?;
        plasm_compile::validate_cgs_capability_templates(ctx.cgs.as_ref())
            .map_err(|e| format!("{}: {e}", meta.entry_id))?;
    }
    Ok(())
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

    progress("materializing CGS entries from compiled CBOR IL…");

    let mut ids: Vec<String> = best_by_entry.keys().cloned().collect();
    ids.sort();

    let mut pairs = Vec::with_capacity(ids.len());
    for id in ids {
        let (_ver, meta, _manifest_path) = best_by_entry.remove(&id).expect("key exists");
        let cgs: CGS = load_catalog_artifact(dir, &meta)?;
        let label = if meta.label.is_empty() {
            meta.entry_id.clone()
        } else {
            meta.label.clone()
        };
        pairs.push((meta.entry_id, label, meta.tags, Arc::new(cgs)));
    }

    let reg = InMemoryCgsRegistry::from_pairs(pairs);
    validate_registry_templates_with_progress(&reg, progress)?;
    Ok(reg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::catalog_il::{CatalogManifest, PLASM_CATALOG_FORMAT_VERSION};
    use std::path::PathBuf;

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
            cgs_cbor: format!("{entry_id}.v{version}.deadbeefcafe.cgs.cbor"),
        }
    }

    #[test]
    fn ingest_prefers_higher_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_manifest(dir.path(), "a.manifest.json", &manifest("github", 1, &"a".repeat(64)));
        write_manifest(dir.path(), "b.manifest.json", &manifest("github", 2, &"b".repeat(64)));
        let mut best = HashMap::new();
        ingest_manifest_candidate(&dir.path().join("a.manifest.json"), &mut best).unwrap();
        ingest_manifest_candidate(&dir.path().join("b.manifest.json"), &mut best).unwrap();
        assert_eq!(best["github"].0, 2);
        assert_eq!(best["github"].2, PathBuf::from(dir.path().join("b.manifest.json")));
    }

    #[test]
    fn ingest_keeps_higher_version_when_seen_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_manifest(dir.path(), "a.manifest.json", &manifest("github", 1, &"a".repeat(64)));
        write_manifest(dir.path(), "b.manifest.json", &manifest("github", 2, &"b".repeat(64)));
        let mut best = HashMap::new();
        ingest_manifest_candidate(&dir.path().join("b.manifest.json"), &mut best).unwrap();
        ingest_manifest_candidate(&dir.path().join("a.manifest.json"), &mut best).unwrap();
        assert_eq!(best["github"].0, 2);
        assert_eq!(best["github"].2, PathBuf::from(dir.path().join("b.manifest.json")));
    }

    #[test]
    fn ingest_rejects_same_version_conflicting_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_manifest(dir.path(), "a.manifest.json", &manifest("github", 3, &"a".repeat(64)));
        write_manifest(dir.path(), "b.manifest.json", &manifest("github", 3, &"b".repeat(64)));
        let mut best = HashMap::new();
        ingest_manifest_candidate(&dir.path().join("a.manifest.json"), &mut best).unwrap();
        let err = ingest_manifest_candidate(&dir.path().join("b.manifest.json"), &mut best).unwrap_err();
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
        assert_eq!(best["github"].2, PathBuf::from(dir.path().join("a.manifest.json")));
    }
}
