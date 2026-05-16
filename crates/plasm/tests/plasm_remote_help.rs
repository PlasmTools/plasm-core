//! Smoke test: `plasm` remote-terminal CLI is wired (no HTTP server required).

use serde_json::Value;
use std::path::{Path, PathBuf};

fn plasm_exe() -> PathBuf {
    if let Some(p) = option_env!("CARGO_BIN_EXE_plasm") {
        return PathBuf::from(p);
    }
    if let Some(p) = std::env::var_os("CARGO_BIN_EXE_plasm") {
        return PathBuf::from(p);
    }
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target")
        .join(&profile)
        .join("plasm")
}

#[test]
fn plasm_remote_help_ok() {
    let exe = plasm_exe();
    let out = std::process::Command::new(&exe)
        .arg("--help")
        .output()
        .expect("spawn plasm");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("doctor")
            && s.contains("init")
            && s.contains("search")
            && s.contains("context")
            && s.contains("run"),
        "expected core subcommands in help: {s}"
    );
    assert!(
        !s.contains("open")
            && !s.contains("auth-set")
            && !s.contains("--server")
            && !s.contains("prompt-hash"),
        "transport/debug flags must not appear in main help: {s}"
    );

    let ctx = std::process::Command::new(&exe)
        .arg("context")
        .arg("--help")
        .output()
        .expect("context --help");
    assert!(ctx.status.success());
    let ctx_help = String::from_utf8_lossy(&ctx.stdout);
    assert!(
        ctx_help.contains("--new") && ctx_help.contains("--verbose"),
        "context should expose --new and --verbose: {ctx_help}"
    );
}

#[test]
fn plasm_init_writes_profile_and_search_uses_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path();
    let exe = plasm_exe();
    std::fs::create_dir_all(home.join(".plasm/cgs/profiles")).expect("mkdir");

    let out = std::process::Command::new(&exe)
        .env("HOME", home)
        .args(["init", "--server", "http://127.0.0.1:9"])
        .output()
        .expect("spawn init");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let prof_raw =
        std::fs::read_to_string(home.join(".plasm/cgs/profiles/default.json")).expect("profile");
    let v: Value = serde_json::from_str(&prof_raw).expect("json");
    assert_eq!(v["server"], "http://127.0.0.1:9");
    let grammar_raw =
        std::fs::read_to_string(home.join(".plasm/cgs/plasm_grammar.md")).expect("grammar");
    assert!(grammar_raw.contains("# Plasm Grammar"));
    let expected_frontmatter = plasm_core::prompt_render::render_plasm_mcp_language_frontmatter();
    let expected_block = format!("```text\n{}\n```", expected_frontmatter.trim());
    assert!(grammar_raw.contains(&expected_block));

    let search = std::process::Command::new(&exe)
        .env("HOME", home)
        .args(["search", "hello"])
        .output()
        .expect("spawn search");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&search.stderr),
        String::from_utf8_lossy(&search.stdout)
    );
    assert!(
        !combined.contains("Plasm is not configured"),
        "search should use profile server: {combined}"
    );
}

#[test]
fn plasm_init_default_server_without_flag() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path();
    let exe = plasm_exe();
    let out = std::process::Command::new(&exe)
        .env("HOME", home)
        .arg("init")
        .output()
        .expect("spawn init");
    assert!(out.status.success());
    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".plasm/cgs/profiles/default.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(v["server"], "http://127.0.0.1:3000");
    assert!(home.join(".plasm/cgs/plasm_grammar.md").exists());
}

#[test]
fn plasm_init_api_key_only_preserves_server() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path();
    let exe = plasm_exe();
    std::process::Command::new(&exe)
        .env("HOME", home)
        .args(["init", "--server", "http://127.0.0.1:21112"])
        .output()
        .expect("init server");
    let out = std::process::Command::new(&exe)
        .env("HOME", home)
        .args(["init", "--api-key", "k_only"])
        .output()
        .expect("init key");
    assert!(out.status.success());
    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".plasm/cgs/profiles/default.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(v["server"], "http://127.0.0.1:21112");
    assert_eq!(v["api_key"], "k_only");
}

#[test]
fn plasm_doctor_runs_without_server() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let exe = plasm_exe();
    let out = std::process::Command::new(&exe)
        .env("HOME", tmp.path())
        .args(["doctor"])
        .output()
        .expect("spawn doctor");
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("not configured") && s.contains("plasm init"),
        "expected doctor to guide init when unconfigured: {s}"
    );
}

#[test]
fn plasm_doctor_uses_profile_server() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path();
    let exe = plasm_exe();
    std::process::Command::new(&exe)
        .env("HOME", home)
        .args(["init", "--server", "http://127.0.0.1:21112"])
        .output()
        .expect("init");

    let out = std::process::Command::new(&exe)
        .env("HOME", home)
        .args(["doctor"])
        .output()
        .expect("spawn doctor");
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("http://127.0.0.1:21112"),
        "expected profile server in doctor output: {s}"
    );
    assert!(
        s.contains("resolved from: profile"),
        "expected doctor to report profile source: {s}"
    );
}

#[test]
fn plasm_search_requires_init() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path();
    let exe = plasm_exe();
    std::fs::create_dir_all(home.join(".plasm/cgs/profiles")).expect("mkdir");

    let out = std::process::Command::new(&exe)
        .env("HOME", home)
        .args(["search", "x"])
        .output()
        .expect("spawn search");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.contains("Plasm is not configured") && combined.contains("plasm init"),
        "expected init guidance: {combined}"
    );
}

#[test]
fn plasm_run_without_active_context_fails_actionably() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path();
    let exe = plasm_exe();
    std::process::Command::new(&exe)
        .env("HOME", home)
        .args(["init"])
        .output()
        .expect("init");
    let out = std::process::Command::new(&exe)
        .env("HOME", home)
        .args(["run"])
        .stdin(std::process::Stdio::piped())
        .output()
        .expect("spawn run");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.contains("No active plasm context") && combined.contains("plasm context"),
        "expected actionable missing-context error: {combined}"
    );
}
