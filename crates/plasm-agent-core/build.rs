//! Compile-time guards for MCP App bundles embedded via `include_str!` in the binary.

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

fn main() {
    let target = std::env::var("TARGET").expect("TARGET must be set by Cargo for build scripts");
    println!("cargo:rustc-env=PLASM_HOST_TARGET_TRIPLE={target}");

    for path in EMBEDDED_UI_ASSETS {
        println!("cargo:rerun-if-changed={path}");
        let body = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!("missing embedded MCP asset {path}: {e} — run scripts/ci/ensure-*-ui-bundle.sh")
        });
        assert_no_dev_refs(path, &body);
    }
}
