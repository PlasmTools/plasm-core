#[path = "../src/tracing_setup.rs"]
mod tracing_setup;

#[test]
fn napi_subscriber_child() {
    if std::env::var_os("PLASM_TRACING_TEST_CHILD").is_none() {
        return;
    }
    if std::env::var_os("PLASM_TRACING_TEST_EXISTING").is_some() {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .init();
    }
    tracing_setup::init();
    tracing_setup::init();
    tracing::warn!("napi_warn_witness");
    tracing::trace!(target: "plasm_runtime::hydration", attempt = 17, "hydration_trace_witness");
    tracing::trace!(target: "unrelated", "unrelated_trace_witness");
}

#[test]
fn napi_subscriber_selects_hydration_without_stdout() {
    for (filter, selected) in [
        ("warn", false),
        ("warn,plasm_runtime::hydration=trace", true),
    ] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "napi_subscriber_child", "--nocapture"])
            .env("PLASM_TRACING_TEST_CHILD", "1")
            .env("RUST_LOG", filter)
            .output()
            .unwrap();
        assert!(out.status.success());
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(stderr.contains("napi_warn_witness"), "{stderr}");
        assert_eq!(
            stderr.contains("hydration_trace_witness"),
            selected,
            "{stderr}"
        );
        assert!(!stderr.contains("unrelated_trace_witness"));
        assert!(!String::from_utf8(out.stdout)
            .unwrap()
            .contains("hydration_trace_witness"));
    }
}

#[test]
fn napi_subscriber_preserves_embedder_subscriber() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "napi_subscriber_child", "--nocapture"])
        .env("PLASM_TRACING_TEST_CHILD", "1")
        .env("PLASM_TRACING_TEST_EXISTING", "1")
        .env("RUST_LOG", "off")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stderr)
        .unwrap()
        .contains("unrelated_trace_witness"));
}
