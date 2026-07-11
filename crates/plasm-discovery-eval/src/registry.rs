use std::path::{Path, PathBuf};
use std::sync::Arc;

use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_core::RegistryEntryPair;

pub fn resolve_apis_root(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        if p.is_dir() {
            return p.to_path_buf();
        }
    }
    let from_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis");
    if from_manifest.is_dir() {
        return from_manifest;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../apis")
}

fn title_case_entry_id(id: &str) -> String {
    id.split('-')
        .map(|w| {
            let mut ch = w.chars();
            match ch.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn load_registry(
    apis_root: &Path,
    entry_ids: &[String],
) -> anyhow::Result<InMemoryCgsRegistry> {
    let mut pairs: Vec<RegistryEntryPair> = Vec::new();
    for id in entry_ids {
        let dir = apis_root.join(id);
        if !dir.join("domain.yaml").is_file() || !dir.join("mappings.yaml").is_file() {
            anyhow::bail!("missing split schema for {id}");
        }
        let cgs = load_schema_dir(&dir)
            .map_err(|e| anyhow::anyhow!("load_schema_dir failed for {id}: {e}"))?;
        pairs.push((
            id.clone(),
            title_case_entry_id(id),
            Vec::new(),
            Arc::new(cgs),
        ));
    }
    Ok(InMemoryCgsRegistry::from_pairs(pairs))
}
