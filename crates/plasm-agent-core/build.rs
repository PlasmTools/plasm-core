//! Compile-time guards for MCP App bundles embedded via `include_str!` in the binary.
//! Also stamps [`PLASM_RELEASE_VERSION`](crate::release_version::RELEASE_VERSION) for `/v1/health`.

use std::path::{Path, PathBuf};

const EMBEDDED_UI_ASSETS: &[&str] = &[
    "src/assets/plan_ui.html",
    "src/assets/plan_ui_page.html",
    "src/assets/plan_ui.js",
    "src/assets/plan_shell.html",
    "src/assets/plan_shell.js",
    "src/assets/workflow_ui.html",
    "src/assets/workflow_ui_page.html",
    "src/assets/workflow_ui.js",
    "src/assets/workflow_shell.html",
    "src/assets/workflow_shell.js",
    "src/assets/run_ui.html",
    "src/assets/run_ui_page.html",
    "src/assets/run_ui.js",
    "src/assets/run_shell.html",
    "src/assets/run_shell.js",
];

const DEV_REF_NEEDLES: &[&str] = &[
    "/src/main.ts",
    "/src/appliance-shell.ts",
    "vite/client",
    "@vite/env",
];

fn assert_no_dev_refs(label: &str, body: &str) {
    for needle in DEV_REF_NEEDLES {
        if body.contains(needle) {
            panic!(
                "embedded MCP asset {label} contains dev reference `{needle}` — rebuild UI bundles (scripts/ci/ensure-*-ui-bundle.sh)"
            );
        }
    }
}

fn parse_workspace_package_version(cargo_toml: &Path) -> Option<String> {
    let text = std::fs::read_to_string(cargo_toml).ok()?;
    let mut in_workspace_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line == "[workspace.package]" {
            in_workspace_package = true;
            continue;
        }
        if in_workspace_package && line.starts_with('[') {
            break;
        }
        if in_workspace_package && line.starts_with("version = ") {
            return line.split('"').nth(1).map(str::to_string);
        }
    }
    None
}

fn workspace_release_version(manifest_dir: &Path) -> Option<String> {
    for rel in ["../../../Cargo.toml", "../../Cargo.toml"] {
        let path = manifest_dir.join(rel);
        println!("cargo:rerun-if-changed={}", path.display());
        if let Some(version) = parse_workspace_package_version(&path) {
            return Some(version);
        }
    }
    None
}

fn docker_image_tag_version() -> Option<String> {
    let path = PathBuf::from("/build/.plasm_last_image_tag");
    println!("cargo:rerun-if-changed={}", path.display());
    let raw = std::fs::read_to_string(path).ok()?;
    let tag = raw.trim().trim_start_matches('v');
    if tag.is_empty() || tag == "latest" {
        None
    } else {
        Some(tag.to_string())
    }
}

fn stamp_release_version() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let version = std::env::var("PLASM_RELEASE_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(docker_image_tag_version)
        .or_else(|| workspace_release_version(&manifest_dir))
        .unwrap_or(pkg_version);
    println!("cargo:rustc-env=PLASM_RELEASE_VERSION={version}");
}

fn main() {
    stamp_release_version();
    for path in EMBEDDED_UI_ASSETS {
        println!("cargo:rerun-if-changed={path}");
        let body = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!("missing embedded MCP asset {path}: {e} — run scripts/ci/ensure-*-ui-bundle.sh")
        });
        assert_no_dev_refs(path, &body);
    }
}
