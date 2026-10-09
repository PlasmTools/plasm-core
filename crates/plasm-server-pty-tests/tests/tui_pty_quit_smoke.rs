#![cfg(unix)]

//! Single PTY smoke: spawn release `plasm-server`, wait for RUN footer, quit with `q`.
//!
//! Lives in a separate crate so `testty` (vt100 0.16) does not conflict with `ratatui`'s
//! pinned `unicode-width = 0.2.0`.

#[path = "../../plasm-server/tests/appliance_boot_support.rs"]
mod appliance_boot_support;

use std::path::Path;
use std::time::Duration;

use testty::session::PtySession;

use appliance_boot_support::{
    appliance_pty_spawn_env, make_appliance_data_root, pick_free_tcp_port, repo_root, schema_path,
};

const STABLE_FRAME: Duration = Duration::from_secs(2);
const SCREEN_WAIT: Duration = Duration::from_secs(300);
const EXIT_WAIT: Duration = Duration::from_secs(30);

fn bin_path() -> std::path::PathBuf {
    std::env::var_os("CARGO_BIN_EXE_plasm_server")
        .expect("appliance PTY gate must supply the built plasm-server binary")
        .into()
}

#[test]
fn tui_pty_quit_smoke() {
    let listen_port = pick_free_tcp_port();
    let schema = schema_path();
    let (data_root, diag_log) = make_appliance_data_root("plasm-server-pty-");
    let data_dir = data_root.path();
    let env_owned = appliance_pty_spawn_env(&diag_log);
    let env_pairs: Vec<(&str, &str)> = env_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let data_dir_s = path_to_str(data_dir);
    let schema_s = path_to_str(&schema);
    let port_s = listen_port.to_string();
    let args = [
        "--data-dir",
        data_dir_s.as_str(),
        "--schema",
        schema_s.as_str(),
        "--port",
        port_s.as_str(),
    ];

    eprintln!(
        "appliance-pty: spawn {} listen_port={listen_port} data_dir={} diag_log={}",
        bin_path().display(),
        data_dir.display(),
        diag_log.display()
    );

    let mut session =
        PtySession::spawn_with_size(&bin_path(), 120, 40, &args, &env_pairs, Some(&repo_root()))
            .unwrap_or_else(|e| panic!("spawn plasm-server in PTY: {e}"));

    session
        .wait_for_stable_frame(STABLE_FRAME, SCREEN_WAIT)
        .unwrap_or_else(|e| panic_pty("stable RUN frame", &diag_log, e));
    session
        .wait_for_text("q: quit", SCREEN_WAIT)
        .unwrap_or_else(|e| panic_pty("footer q: quit", &diag_log, e));
    session
        .press_key("q")
        .unwrap_or_else(|e| panic!("press q: {e}"));
    let exited_ok = session
        .wait_for_exit(EXIT_WAIT)
        .unwrap_or_else(|e| panic_pty("process exit after q", &diag_log, e));
    assert!(
        exited_ok,
        "plasm-server should exit 0 after q; diag tail:\n{}",
        appliance_boot_support::read_tail(&diag_log, 16 * 1024)
    );
}

fn path_to_str(path: &Path) -> String {
    path.to_str()
        .unwrap_or_else(|| panic!("non-UTF-8 path {}", path.display()))
        .to_string()
}

fn panic_pty(context: &str, diag: &Path, err: impl std::fmt::Display) -> ! {
    panic!(
        "{context}: {err}\n--- PLASM_APPLIANCE_DIAG_LOG ({}) ---\n{}",
        diag.display(),
        appliance_boot_support::read_tail(diag, 16 * 1024)
    );
}
