//! Run [`plasm_core::iter_all_cases`] against every loadable API under an `apis/` root.
//!
//! ```text
//! cargo run -p plasm-core --bin discovery_adversarial
//! cargo run -p plasm-core --bin discovery_adversarial /path/to/apis
//! ```
//!
//! Resolves a relative `apis` argument against the workspace root (`crates/plasm-core/../../`).

use plasm_core::discovery::CgsDiscovery;
use plasm_core::iter_all_cases;
use plasm_core::loader::load_schema_dir;
use plasm_core::InMemoryCgsRegistry;
use plasm_core::RegistryEntryPair;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn resolve_apis_root(arg: Option<&str>) -> PathBuf {
    if let Some(a) = arg {
        let p = PathBuf::from(a);
        if p.is_dir() {
            return p;
        }
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let from_root = manifest.join("../..").join(a);
        if from_root.is_dir() {
            return from_root;
        }
        return p;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis")
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

fn load_registry(apis_root: &Path) -> InMemoryCgsRegistry {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(apis_root)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", apis_root.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join("domain.yaml").is_file() && p.join("mappings.yaml").is_file())
        .collect();
    dirs.sort();

    let mut pairs: Vec<RegistryEntryPair> = Vec::new();
    for dir in dirs {
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        match load_schema_dir(&dir) {
            Ok(cgs) => {
                pairs.push((
                    name.clone(),
                    title_case_entry_id(&name),
                    Vec::new(),
                    Arc::new(cgs),
                ));
            }
            Err(e) => {
                eprintln!("skip {}: {e}", dir.display());
            }
        }
    }
    eprintln!(
        "loaded {} catalogs from {}",
        pairs.len(),
        apis_root.display()
    );
    InMemoryCgsRegistry::from_pairs(pairs)
}

fn main() {
    let apis_root = resolve_apis_root(std::env::args().nth(1).as_deref());
    if !apis_root.is_dir() {
        eprintln!("apis root is not a directory: {}", apis_root.display());
        std::process::exit(1);
    }

    let reg = load_registry(&apis_root);

    for case in iter_all_cases() {
        let q = case.capability_query();
        print!(
            "[{}] {:?} — {} … ",
            case.id,
            case.kind,
            &case.intent[..case.intent.len().min(72)]
        );
        match reg.discover(&q) {
            Ok(r) => {
                let mut eids: Vec<String> = r
                    .candidates
                    .iter()
                    .map(|c| c.entry_id.clone())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                eids.sort();
                let top: Vec<String> = r
                    .candidates
                    .iter()
                    .take(5)
                    .map(|c| {
                        format!(
                            "{}:{}:{}={}",
                            c.entry_id, c.entity, c.capability_name, c.score
                        )
                    })
                    .collect();
                println!(
                    "candidates={} catalogs={:?} top={:?}",
                    r.candidates.len(),
                    eids,
                    top
                );
            }
            Err(e) => {
                println!("ERR {e}");
            }
        }
    }
}
