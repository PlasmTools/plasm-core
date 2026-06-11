//! Emit `apps/workflow-mcp-app/src/generated/contracts.ts`.

use std::env;
use std::path::PathBuf;

fn default_out_path() -> PathBuf {
    let root = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(root)
        .join("../../..")
        .join("apps/workflow-mcp-app/src/generated/contracts.ts")
}

fn main() {
    let out = env::args()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .unwrap_or_else(default_out_path);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).expect("create generated dir");
    }
    let ts = plasm_workflow_codegen::emit_contracts_ts();
    std::fs::write(&out, ts).expect("write contracts.ts");
    eprintln!("wrote {}", out.display());
}
