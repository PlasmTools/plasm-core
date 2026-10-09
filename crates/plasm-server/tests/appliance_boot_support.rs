//! Shared bootstrap polling for appliance integration tests (diag log + TCP).

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const TEST_AUTH_STORAGE_ENCRYPTION_KEY: &str = "YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWE=";

pub const BOOTSTRAP_WAIT: Duration = Duration::from_secs(600);
pub const BOOTSTRAP_PROGRESS_INTERVAL: Duration = Duration::from_secs(15);
pub const EMBEDDED_PG_TIMEOUT_SECS: &str = "300";
/// `plasm_appliance_boot` / `plasm_agent` at `info` so milestones land in `PLASM_APPLIANCE_DIAG_LOG`.
pub const APPLIANCE_TEST_RUST_LOG: &str =
    "warn,plasm_appliance_boot=info,plasm_agent=info,plasm_agent_core=warn,pg_embed=warn,sqlx=warn";

/// Keep in sync with `scripts/ci/clear-integration-test-env.sh` (appliance child env uses this subset).
pub const EXTERNAL_POSTGRES_ENV_KEYS: &[&str] = &[
    "DATABASE_URL",
    "PLASM_MCP_CONFIG_DATABASE_URL",
    "PLASM_AUTH_STORAGE_URL",
    "PGDATA",
    "PGHOST",
    "PGPORT",
    "PGUSER",
    "PGPASSWORD",
    "PGDATABASE",
    "POSTGRES_URL",
    "POSTGRES_HOST",
    "POSTGRES_PORT",
    "PLASM_EMBEDDED_POSTGRES_DATA_DIR",
    "PLASM_LOCAL_STATE_DIR",
];

const OTEL_EXPORT_ENV_KEYS: &[&str] = &[
    "OTEL_SDK_DISABLED",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
    "OTEL_TRACES_EXPORTER",
    "OTEL_METRICS_EXPORTER",
    "OTEL_LOGS_EXPORTER",
];

/// Must match `tracing` lines in `plasm-server` `main.rs` (`target: plasm_appliance_boot`).
pub const LOG_PHASE_HEADLESS_LISTENER: &str = "phase: headless listener running";
pub const LOG_EMBEDDED_PG_READY: &str = "embedded postgres: server ready";
pub const LOG_RUN_HANDSHAKE_TIMEOUT: &str = "RUN UI RunEntered not observed within 120s";

pub const LOG_POLICY_STORE_CONNECTED: &str = "project_mcp_* policy store connected";

const HEADLESS_MILESTONES: &[(&str, &str)] = &[
    (LOG_EMBEDDED_PG_READY, "embedded Postgres up"),
    (LOG_POLICY_STORE_CONNECTED, "MCP policy store connected"),
    (LOG_PHASE_HEADLESS_LISTENER, "headless listener running"),
];

pub fn read_tail(path: &Path, max: usize) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let s = String::from_utf8_lossy(&bytes).into_owned();
            if s.len() <= max {
                s
            } else {
                format!("…[truncated]\n{}", &s[s.len().saturating_sub(max)..])
            }
        }
        Err(e) => format!("(could not read {:?}: {e})", path),
    }
}

/// One-line progress for periodic eprintln (no diag tail dump).
pub fn bootstrap_progress_line(diag: &Path, http_up: bool, elapsed: Duration) -> String {
    let tail = read_tail(diag, 64 * 1024);
    let missing: Vec<&str> = HEADLESS_MILESTONES
        .iter()
        .filter(|(needle, _)| !tail.contains(*needle))
        .map(|(_, label)| *label)
        .collect();
    if missing.is_empty() {
        format!("appliance-boot: waiting {elapsed:?} http={http_up} (milestones ok)")
    } else {
        format!(
            "appliance-boot: waiting {elapsed:?} http={http_up} missing: {}",
            missing.join(", ")
        )
    }
}

/// Full milestone report for panic / timeout messages.
pub fn diag_boot_milestone_report(diag: &Path) -> String {
    let full = read_tail(diag, 512 * 1024);
    let mut lines = Vec::new();
    lines.push(format!("diag log: {}", diag.display()));
    for (needle, label) in HEADLESS_MILESTONES {
        let mark = if full.contains(needle) {
            "ok"
        } else {
            "MISSING"
        };
        lines.push(format!("  [{mark}] {label}"));
    }
    lines.join("\n")
}

pub fn pick_free_tcp_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind 127.0.0.1:0")
        .local_addr()
        .expect("local_addr")
        .port()
}

pub fn diag_has_fatal(diag: &Path) -> Option<String> {
    let tail = read_tail(diag, 8 * 1024);
    if tail.contains("bootstrap failed")
        || tail.contains("[plasm-server] fatal:")
        || tail.contains(LOG_RUN_HANDSHAKE_TIMEOUT)
    {
        Some(tail)
    } else {
        None
    }
}

fn bootstrap_complete(tail: &str, http_up: bool) -> bool {
    http_up && tail.contains(LOG_EMBEDDED_PG_READY) && tail.contains(LOG_PHASE_HEADLESS_LISTENER)
}

/// Poll `PLASM_APPLIANCE_DIAG_LOG` and HTTP until headless bootstrap is complete.
pub fn wait_bootstrap_ready(listen_port: u16, diag: &Path) {
    let started = Instant::now();
    let deadline = started + BOOTSTRAP_WAIT;
    let mut last_progress = started;
    let addr = SocketAddr::from(([127, 0, 0, 1], listen_port));

    while Instant::now() < deadline {
        if let Some(tail) = diag_has_fatal(diag) {
            panic!(
                "bootstrap fatal\n{}\n--- diag tail ---\n{tail}",
                diag_boot_milestone_report(diag)
            );
        }
        let tail = read_tail(diag, 64 * 1024);
        if tail.contains(LOG_RUN_HANDSHAKE_TIMEOUT) {
            panic!(
                "plasm-server gave up waiting for RUN UI RunEntered\n{}",
                diag_boot_milestone_report(diag)
            );
        }

        let http_up = TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok();
        if bootstrap_complete(&tail, http_up) {
            eprintln!(
                "appliance-boot: bootstrap ready after {:?}",
                started.elapsed()
            );
            return;
        }

        let now = Instant::now();
        if now.duration_since(last_progress) >= BOOTSTRAP_PROGRESS_INTERVAL {
            last_progress = now;
            eprintln!(
                "{}",
                bootstrap_progress_line(diag, http_up, started.elapsed())
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!(
        "timeout waiting for bootstrap ({BOOTSTRAP_WAIT:?})\n{}",
        diag_boot_milestone_report(diag)
    );
}

pub fn make_appliance_data_root(prefix: &str) -> (tempfile::TempDir, PathBuf) {
    let data_root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(embedded_pg_temp_parent())
        .expect("temp appliance data root");
    let diag_log = data_root.path().join("appliance-diag.log");
    (data_root, diag_log)
}

pub fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for rel in ["../../..", "../../../..", "../../../../.."] {
        let candidate = manifest.join(rel);
        let Ok(root) = candidate.canonicalize() else {
            continue;
        };
        if root.join("fixtures/schemas/overshow_tools").is_dir() {
            return root;
        }
    }
    panic!(
        "monorepo root not found from {}; expected fixtures/schemas/overshow_tools",
        manifest.display()
    );
}

pub fn schema_path() -> PathBuf {
    let p = repo_root().join("fixtures/schemas/overshow_tools");
    assert!(
        p.is_dir(),
        "missing schema path {p:?}; run tests from the monorepo root"
    );
    p
}

pub fn embedded_pg_temp_parent() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/tmp")
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::temp_dir()
    }
}

pub fn clear_external_postgres_env(cmd: &mut std::process::Command) {
    for key in EXTERNAL_POSTGRES_ENV_KEYS {
        cmd.env_remove(key);
    }
    cmd.env("PLASM_EMBEDDED_POSTGRES", "1");
}

fn clear_otel_export_env(cmd: &mut std::process::Command) {
    for key in OTEL_EXPORT_ENV_KEYS {
        cmd.env_remove(key);
    }
    cmd.env("OTEL_SDK_DISABLED", "true");
}

pub fn clear_inherited_postgres_urls(cmd: &mut std::process::Command) {
    for key in [
        "DATABASE_URL",
        "PLASM_MCP_CONFIG_DATABASE_URL",
        "PLASM_AUTH_STORAGE_URL",
    ] {
        cmd.env_remove(key);
    }
}

pub fn apply_appliance_test_env(cmd: &mut std::process::Command, diag_log: &Path) {
    cmd.env("NO_COLOR", "1");
    clear_external_postgres_env(cmd);
    clear_inherited_postgres_urls(cmd);
    clear_otel_export_env(cmd);
    cmd.env(
        "AUTH_STORAGE_ENCRYPTION_KEY",
        TEST_AUTH_STORAGE_ENCRYPTION_KEY,
    );
    cmd.env(
        "PLASM_EMBEDDED_POSTGRES_TIMEOUT_SECS",
        EMBEDDED_PG_TIMEOUT_SECS,
    );
    cmd.env("PLASM_APPLIANCE_DIAG_LOG", diag_log);
    cmd.env("RUST_LOG", APPLIANCE_TEST_RUST_LOG);
}

/// Environment pairs for `testty::PtySession::spawn_with_size` (PTY quit smoke).
#[allow(dead_code)] // used by `plasm-server-pty-tests` via `#[path]` include
pub fn appliance_pty_spawn_env(diag_log: &Path) -> Vec<(String, String)> {
    let mut pairs = vec![
        ("NO_COLOR".into(), "1".into()),
        ("PLASM_EMBEDDED_POSTGRES".into(), "1".into()),
        ("OTEL_SDK_DISABLED".into(), "true".into()),
        (
            "AUTH_STORAGE_ENCRYPTION_KEY".into(),
            TEST_AUTH_STORAGE_ENCRYPTION_KEY.into(),
        ),
        (
            "PLASM_EMBEDDED_POSTGRES_TIMEOUT_SECS".into(),
            EMBEDDED_PG_TIMEOUT_SECS.into(),
        ),
        (
            "PLASM_APPLIANCE_DIAG_LOG".into(),
            diag_log.display().to_string(),
        ),
        ("RUST_LOG".into(), APPLIANCE_TEST_RUST_LOG.into()),
    ];
    for key in [
        "DATABASE_URL",
        "PLASM_MCP_CONFIG_DATABASE_URL",
        "PLASM_AUTH_STORAGE_URL",
        "PLASM_EMBEDDED_POSTGRES_PORT",
        "PGDATA",
    ] {
        pairs.push(((*key).into(), String::new()));
    }
    pairs
}

pub fn push_appliance_cli_args(
    cmd: &mut std::process::Command,
    data_dir: &Path,
    schema: &Path,
    listen_port: u16,
) {
    cmd.arg("--data-dir").arg(data_dir);
    cmd.arg("--schema").arg(schema);
    cmd.arg("--port").arg(listen_port.to_string());
}
