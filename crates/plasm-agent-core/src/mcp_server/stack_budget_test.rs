//! Regression: MCP tool dispatch must not require process-wide `RUST_MIN_STACK`.

use std::sync::Arc;

use rust_mcp_sdk::schema::{
    CallToolRequestParams, Implementation, InitializeResult, ProtocolVersion, ServerCapabilities,
};
use rust_mcp_sdk::ToMcpServerHandler;
use serde_json::json;

use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
use crate::mcp_server::call_tool_dispatch;
use crate::mcp_server::PlasmMcpHandler;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

fn stack_bytes() -> usize {
    std::env::var("PLASM_TEST_STACK_MIB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2)
        * 1024
        * 1024
}

fn host_from_schema_dir(entry_id: &str, title: &str, dir: &std::path::Path) -> Arc<crate::server_state::PlasmHostState> {
    if std::env::var_os("PLASM_HTTP_NO_SYSTEM_PROXY").is_none() {
        unsafe { std::env::set_var("PLASM_HTTP_NO_SYSTEM_PROXY", "1") };
    }
    let cgs = Arc::new(load_schema_dir(dir).expect("schema"));
    let reg = InMemoryCgsRegistry::from_pairs(vec![(
        entry_id.into(),
        title.into(),
        vec![entry_id.into()],
        cgs,
    )]);
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    Arc::new(build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(reg),
        catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    }))
}

async fn dispatch_plasm_context(
    st: Arc<crate::server_state::PlasmHostState>,
    session: &str,
    api: &str,
    entity: &str,
    intent: &str,
) {
    let handler = PlasmMcpHandler::new(Arc::clone(&st));
    let mcp_handler = PlasmMcpHandler::new(Arc::clone(&st)).to_mcp_server_handler();
    let details = Arc::new(InitializeResult {
        protocol_version: ProtocolVersion::V2025_11_25.into(),
        capabilities: ServerCapabilities::default(),
        server_info: Implementation {
            name: "stack-budget".into(),
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
            session.into(),
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
                "intent": intent,
                "seeds": [{"api": api, "entity": entity}]
            })
            .as_object()
            .cloned()
            .unwrap(),
        ),
        meta: None,
        task: None,
    };
    let res = call_tool_dispatch::dispatch_plasm_mcp_call_tool_request(&handler, params, runtime)
        .await
        .expect("plasm_context must return (not abort)");
    let _ = res;
}

fn run_on_stack<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    let stack = stack_bytes();
    eprintln!("stack_budget_bytes={stack}");
    let join = std::thread::Builder::new()
        .name("mcp-stack-budget".into())
        .stack_size(stack)
        .spawn(f)
        .expect("spawn stack-budget thread");
    join.join()
        .expect("worker stack overflow — MCP tool path must not rely on RUST_MIN_STACK");
}

#[test]
fn mcp_plasm_context_survives_two_mib_worker_stack() {
    run_on_stack(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        rt.block_on(async {
            let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../apis/appworld/supervisor");
            let st = host_from_schema_dir("supervisor", "Supervisor", &dir);
            dispatch_plasm_context(
                st,
                "stack-budget-session",
                "supervisor",
                "Supervisor",
                "show supervisor profile",
            )
            .await;
        });
    });
}

#[test]
fn mcp_plasm_context_matrix_survives_two_mib_worker_stack() {
    run_on_stack(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        rt.block_on(async {
            let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix");
            let st = host_from_schema_dir("github", "GitHub", &dir);
            dispatch_plasm_context(st, "stack-budget-matrix", "github", "LangItem", "list items")
                .await;
        });
    });
}
