//! Stack-budget probes for large MCP / execute futures (debug builds inflate state machines).

use std::future::Future;
use std::sync::Arc;

use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
use crate::http_execute::{apply_capability_seeds, CapabilitySeed};
use plasm_core::discovery::CgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

fn size_of_future<F: Future>(f: F) -> usize {
    let n = std::mem::size_of_val(&f);
    drop(f);
    n
}

fn matrix_host() -> crate::server_state::PlasmHostState {
    if std::env::var_os("PLASM_HTTP_NO_SYSTEM_PROXY").is_none() {
        // SAFETY: test-only process env before reqwest Client build.
        unsafe { std::env::set_var("PLASM_HTTP_NO_SYSTEM_PROXY", "1") };
    }
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = Arc::new(load_schema_dir(&dir).expect("plasm_language_matrix"));
    let reg = CgsRegistry::from_pairs(vec![(
        "github".into(),
        "GitHub".into(),
        vec!["github".into()],
        cgs,
    )]);
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(reg),
        catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}

/// Tokio default worker stacks are ~2 MiB. Measuring on a 64 MiB thread so the probe itself
/// cannot overflow while constructing a large future.
#[test]
fn mcp_apply_capability_seeds_future_size() {
    let join = std::thread::Builder::new()
        .name("fut-size-probe".into())
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt");
            let _guard = rt.enter();
            let st = matrix_host();
            let seeds = vec![CapabilitySeed {
                entry_id: "github".into(),
                entity: "LangItem".into(),
            }];
            let fut =
                apply_capability_seeds(&st, None, None, seeds, None, None, None, "size probe");
            let n = size_of_future(fut);
            eprintln!(
                "apply_capability_seeds future: {n} bytes ({:.1} KiB)",
                n as f64 / 1024.0
            );
            assert!(
                n < 256 * 1024,
                "apply_capability_seeds future is {n} bytes — unexpected bloat"
            );
        })
        .expect("spawn probe thread");
    join.join().expect("probe thread panicked");
}

#[test]
fn mcp_plasm_context_future_size() {
    use rust_mcp_sdk::schema::{
        Implementation, InitializeResult, ProtocolVersion, ServerCapabilities,
    };
    use rust_mcp_sdk::ToMcpServerHandler;
    use serde_json::json;

    let join = std::thread::Builder::new()
        .name("ctx-fut-size".into())
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt");
            let _guard = rt.enter();
            let st = Arc::new(matrix_host());
            let handler = crate::mcp_server::PlasmMcpHandler::new(Arc::clone(&st));
            let mcp_handler =
                crate::mcp_server::PlasmMcpHandler::new(Arc::clone(&st)).to_mcp_server_handler();
            let details = Arc::new(InitializeResult {
                protocol_version: ProtocolVersion::V2025_11_25.into(),
                capabilities: ServerCapabilities::default(),
                server_info: Implementation {
                    name: "size-probe".into(),
                    version: "0".into(),
                    title: None,
                    description: None,
                    icons: vec![],
                    website_url: None,
                },
                instructions: None,
                meta: None,
            });
            let runtime: Arc<dyn rust_mcp_sdk::McpServer> =
                rust_mcp_sdk::mcp_server::server_runtime::create_server_instance(
                    details,
                    mcp_handler,
                    "size-probe-session".into(),
                    None,
                    None,
                    None,
                    None,
                );
            let args = json!({
                "session_mode": "new",
                "intent": "size probe",
                "seeds": [{"api": "github", "entity": "LangItem"}]
            });
            let fut = handler.handle_mcp_tool_plasm_context("size-probe-session", &runtime, &args);
            let n = size_of_future(fut);
            eprintln!(
                "handle_mcp_tool_plasm_context future: {n} bytes ({:.1} KiB)",
                n as f64 / 1024.0
            );
            eprintln!("budget_ok={}", n < 512 * 1024);
        })
        .expect("spawn");
    join.join().expect("probe panicked");
}

#[test]
fn mcp_call_tool_dispatch_future_size() {
    use rust_mcp_sdk::schema::{
        CallToolRequestParams, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities,
    };
    use rust_mcp_sdk::ToMcpServerHandler;
    use serde_json::json;

    let join = std::thread::Builder::new()
        .name("dispatch-fut-size".into())
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt");
            let _guard = rt.enter();
            let st = Arc::new(matrix_host());
            let handler = crate::mcp_server::PlasmMcpHandler::new(Arc::clone(&st));
            let mcp_handler =
                crate::mcp_server::PlasmMcpHandler::new(Arc::clone(&st)).to_mcp_server_handler();
            let details = Arc::new(InitializeResult {
                protocol_version: ProtocolVersion::V2025_11_25.into(),
                capabilities: ServerCapabilities::default(),
                server_info: Implementation {
                    name: "size-probe".into(),
                    version: "0".into(),
                    title: None,
                    description: None,
                    icons: vec![],
                    website_url: None,
                },
                instructions: None,
                meta: None,
            });
            let runtime: Arc<dyn rust_mcp_sdk::McpServer> =
                rust_mcp_sdk::mcp_server::server_runtime::create_server_instance(
                    details,
                    mcp_handler,
                    "size-probe-session".into(),
                    None,
                    None,
                    None,
                    None,
                );
            let params = CallToolRequestParams {
                name: "plasm_context".into(),
                arguments: Some(
                    json!({
                        "session_mode": "new",
                        "intent": "size probe",
                        "seeds": [{"api": "github", "entity": "LangItem"}]
                    })
                    .as_object()
                    .cloned()
                    .unwrap(),
                ),
                meta: None,
                task: None,
            };
            let fut = super::call_tool_dispatch::dispatch_plasm_mcp_call_tool_request(
                &handler, params, runtime,
            );
            let n = size_of_future(fut);
            eprintln!(
                "dispatch_plasm_mcp_call_tool_request future: {n} bytes ({:.1} KiB)",
                n as f64 / 1024.0
            );
            eprintln!("dispatch_budget_ok={}", n < 512 * 1024);
        })
        .expect("spawn");
    join.join().expect("probe panicked");
}
