#![cfg(unix)]

//! Headless `plasm-server --no-tui` bootstrap smoke (no PTY). Fast CI gate for embedded PG + HTTP.

mod appliance_boot_support;

use std::panic::AssertUnwindSafe;
use std::process::{Child, Command, Stdio};

use appliance_boot_support::{
    apply_appliance_test_env, make_appliance_data_root, pick_free_tcp_port,
    push_appliance_cli_args, repo_root, schema_path, wait_bootstrap_ready,
};

fn bin_path() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_plasm-server").into()
}

fn spawn_headless_appliance() -> (u16, tempfile::TempDir, std::path::PathBuf, Child) {
    let listen_port = pick_free_tcp_port();
    let schema = schema_path();
    let (data_root, diag_log) = make_appliance_data_root("plasm-server-headless-");

    let mut cmd = Command::new(bin_path());
    cmd.current_dir(repo_root());
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());
    apply_appliance_test_env(&mut cmd, &diag_log);
    cmd.arg("--no-tui");
    push_appliance_cli_args(&mut cmd, data_root.path(), &schema, listen_port);

    eprintln!(
        "appliance-headless: spawn listen_port={listen_port} data_dir={} diag_log={}",
        data_root.path().display(),
        diag_log.display()
    );

    let child = cmd.spawn().expect("spawn plasm-server --no-tui");
    (listen_port, data_root, diag_log, child)
}

#[test]
fn appliance_headless_boot_smoke() {
    let (listen_port, _data, diag_log, mut child) = spawn_headless_appliance();
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        wait_bootstrap_ready(listen_port, &diag_log);
    }));
    let _ = child.kill();
    let _ = child.wait();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    eprintln!("appliance-headless: smoke passed");
}
