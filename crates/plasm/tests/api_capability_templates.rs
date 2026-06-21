//! Every split schema under `apis/<name>/` must have CML mapping templates that parse.
//! Mirrors `plasm-pack-catalogs` validation (same `plasm-compile` + `plasm-cml` feature graph as the binary).

use plasm_compile::validate_cgs_capability_templates;
use plasm_core::loader::load_schema_dir;
use std::fs;
use std::path::PathBuf;

#[test]
fn apis_capability_templates_validate() {
    let apis_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis");
    if !apis_root.is_dir() {
        return;
    }
    for ent in fs::read_dir(&apis_root).expect("read_dir apis") {
        let ent = ent.expect("dir entry");
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if !path.join("domain.yaml").is_file() || !path.join("mappings.yaml").is_file() {
            continue;
        }
        let cgs =
            load_schema_dir(&path).unwrap_or_else(|e| panic!("load_schema_dir apis/{name}: {e}"));
        validate_cgs_capability_templates(&cgs)
            .unwrap_or_else(|e| panic!("validate_cgs_capability_templates apis/{name}: {e}"));
    }
}
