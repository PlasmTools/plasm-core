//! SEP-1865 MCP UI delivery: triple-lane tool results survive Cursor-like `_meta` strip.

#[path = "common/hermit_workflow_matrix.rs"]
mod hermit_workflow_matrix;

#[path = "common/mcp_sse.rs"]
mod mcp_sse;

#[path = "common/workflow_matrix.rs"]
mod workflow_matrix;

use plasm_agent::http::{serve_discovery_execute_and_mcp_unified, DiscoveryHttpServeOpts};
use plasm_runtime::{ExecutionConfig, ExecutionEngine};
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::OnceCell;
use workflow_matrix::{load_workflow_matrix_cgs, workflow_federated_host_state, CATALOG_A};

async fn spawn_server() -> (String, tokio::task::JoinHandle<()>) {
    let hermit = hermit_workflow_matrix::workflow_matrix_hermit_base_url()
        .await
        .clone();
    let cgs = load_workflow_matrix_cgs();
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(hermit),
        ..Default::default()
    })
    .expect("engine");
    let st = workflow_federated_host_state(engine, cgs);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let handle = tokio::spawn(async move {
        serve_discovery_execute_and_mcp_unified(
            listener,
            st,
            DiscoveryHttpServeOpts {
                emit_stderr_route_help: false,
            },
        )
        .await
        .ok();
    });
    tokio::time::sleep(Duration::from_millis(80)).await;
    (base, handle)
}

static SERVER: OnceCell<String> = OnceCell::const_new();

async fn base_url() -> String {
    SERVER
        .get_or_init(|| async {
            let (base, _handle) = spawn_server().await;
            base
        })
        .await
        .clone()
}

fn strip_meta_like_cursor(body: &mut Value) {
    if let Some(obj) = body.as_object_mut() {
        obj.remove("_meta");
    }
}

fn assert_triple_lane_plan(body: &Value) {
    assert!(
        body.pointer("/structuredContent/plasm/comp").is_none(),
        "agent lane must omit comp: {body}"
    );
    assert_eq!(
        body.pointer("/structuredContent/ui/kind")
            .and_then(|v| v.as_str()),
        Some("plan_review"),
        "ui lane kind: {body}"
    );
    let ui_ok = body.pointer("/structuredContent/ui/comp").is_some()
        || body
            .pointer("/structuredContent/ui/plan_http_path")
            .is_some();
    assert!(ui_ok, "ui lane must carry comp or fetch refs: {body}");
    let text = body
        .pointer("/mcp_result/content/0/text")
        .or_else(|| body.pointer("/content/0/text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(!text.is_empty(), "agent content lane must be non-empty");
}

#[test]
fn mcp_ui_delivery_e2e() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(mcp_ui_delivery_e2e_async());
        })
        .expect("spawn")
        .join()
        .expect("join");
}

async fn mcp_ui_delivery_e2e_async() {
    let base = base_url().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");

    let init = client
        .post(format!("{base}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "mcp-ui-delivery-e2e", "version": "0.1.0" }
            }
        }))
        .send()
        .await
        .expect("initialize");
    assert_eq!(init.status(), StatusCode::OK);
    let mcp_session = init
        .headers()
        .get("MCP-Session-Id")
        .and_then(|v| v.to_str().ok())
        .expect("mcp session")
        .to_string();

    let ctx = mcp_sse::mcp_tool_meta(
        &client,
        &base,
        &mcp_session,
        "plasm_context",
        json!({
            "session_mode": "new",
            "intent": "list work items",
            "seeds": [{ "api": CATALOG_A, "entity": "WorkItem" }]
        }),
        10,
    )
    .await;
    let ls_ref = ctx
        .pointer("/structuredContent/plasm/logical_session_ref")
        .or_else(|| ctx.pointer("/_meta/plasm/logical_session_ref"))
        .and_then(|v| v.as_str())
        .expect("logical_session_ref");

    let plan = mcp_sse::mcp_tool_meta(
        &client,
        &base,
        &mcp_session,
        "plasm",
        json!({
            "logical_session_ref": ls_ref,
            "program": "items = e1\nitems"
        }),
        11,
    )
    .await;
    assert_triple_lane_plan(&plan);

    let mut cursor_forward = plan.clone();
    strip_meta_like_cursor(&mut cursor_forward);
    assert!(
        cursor_forward.get("_meta").is_none(),
        "strip simulation must remove _meta"
    );
    assert_eq!(
        cursor_forward
            .pointer("/structuredContent/ui/kind")
            .and_then(|v| v.as_str()),
        Some("plan_review")
    );
    let ui_survives = cursor_forward
        .pointer("/structuredContent/ui/comp")
        .is_some()
        || cursor_forward
            .pointer("/structuredContent/ui/plan_http_path")
            .is_some();
    assert!(
        ui_survives,
        "structuredContent.ui must survive meta strip: {cursor_forward}"
    );

    if let Some(path) = plan
        .pointer("/structuredContent/ui/plan_http_path")
        .and_then(|v| v.as_str())
    {
        let archive = client
            .get(format!("{base}{path}"))
            .header("accept", "application/json")
            .send()
            .await
            .expect("plan http")
            .json::<Value>()
            .await
            .expect("plan json");
        assert!(archive.get("comp").is_some(), "plan archive comp");
    }

    let run_ref = plan
        .pointer("/structuredContent/plasm/run_ref")
        .and_then(|v| v.as_str())
        .expect("run_ref");
    let run = mcp_sse::mcp_tool_meta(
        &client,
        &base,
        &mcp_session,
        "plasm_run",
        json!({
            "logical_session_ref": ls_ref,
            "run_ref": run_ref
        }),
        12,
    )
    .await;
    assert_eq!(
        run.pointer("/structuredContent/ui/kind")
            .and_then(|v| v.as_str()),
        Some("run_explorer")
    );
    assert!(
        run.pointer("/structuredContent/ui/steps")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty()),
        "run ui steps: {run}"
    );
    assert!(
        run.pointer("/structuredContent/plasm/steps").is_none(),
        "agent lane must omit run steps"
    );

    let ui_read = mcp_sse::mcp_sse_json_by_id(
        &client,
        &base,
        &mcp_session,
        json!({
            "jsonrpc": "2.0",
            "id": 13,
            "method": "tools/call",
            "params": {
                "name": "plasm_ui_read_plan",
                "arguments": {
                    "logical_session_ref": ls_ref,
                    "run_ref": run_ref
                }
            }
        }),
        13,
    )
    .await;
    assert_eq!(
        ui_read
            .pointer("/structuredContent/ui/kind")
            .and_then(|v| v.as_str()),
        Some("plan_review")
    );
    assert!(
        ui_read.pointer("/structuredContent/ui/comp").is_some(),
        "app-only hydrate comp: {ui_read}"
    );

    let tools = mcp_sse::mcp_sse_json_by_id(
        &client,
        &base,
        &mcp_session,
        json!({
            "jsonrpc": "2.0",
            "id": 14,
            "method": "tools/list",
            "params": {}
        }),
        14,
    )
    .await;
    let names: Vec<String> = tools
        .pointer("/tools")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect();
    assert!(names.iter().any(|n| n == "plasm"));
    // Server registers app-only hydrate tools; compliant hosts filter by `_meta.ui.visibility`.
    assert!(names.iter().any(|n| n == "plasm_ui_read_plan"));
}
