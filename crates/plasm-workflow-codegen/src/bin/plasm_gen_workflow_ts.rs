//! Emit `apps/workflow-mcp-app/src/generated/contracts.ts`.

use std::env;
use std::path::PathBuf;

fn default_out_paths() -> Vec<PathBuf> {
    let root = env!("CARGO_MANIFEST_DIR");
    let base = PathBuf::from(root).join("../../..");
    vec![
        base.join("apps/workflow-mcp-app/src/generated/contracts.ts"),
        base.join("apps/plan-ui/src/generated/contracts.ts"),
    ]
}

fn main() {
    let explicit: Vec<PathBuf> = env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .collect();
    let outs = if explicit.is_empty() {
        default_out_paths()
    } else {
        explicit
    };
    let ts = plasm_workflow_codegen::emit_contracts_ts();
    for out in outs {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).expect("create generated dir");
        }
        std::fs::write(&out, &ts).expect("write contracts.ts");
        eprintln!("wrote {}", out.display());
    }
}
