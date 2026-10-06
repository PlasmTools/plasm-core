//! SEP-1865 MCP UI delivery: triple-lane tool results survive Cursor-like `_meta` strip.

#[path = "common/hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

#[path = "common/mcp_sse.rs"]
mod mcp_sse;

#[path = "common/language_matrix.rs"]
mod language_matrix;

use language_matrix::{load_language_matrix_cgs, matrix_host_state, MATRIX_ENTRY_ID};
use plasm_agent::http::{serve_discovery_execute_and_mcp_unified, DiscoveryHttpServeOpts};
use plasm_agent::http_execute::{apply_capability_seeds, CapabilitySeed};
use plasm_agent::mcp_logical_ref::format_logical_session_wire_ref;
use plasm_runtime::{ExecutionConfig, ExecutionEngine};
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::OnceCell;

async fn spawn_server() -> (String, String, tokio::task::JoinHandle<()>) {
    let hermit = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let cgs = load_language_matrix_cgs();
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(hermit),
        ..Default::default()
    })
    .expect("engine");
    let st = matrix_host_state(engine, cgs);
    // This fixture exercises UI delivery, not semantic intent discovery.
    // Mint and bind a real anonymous host session before using the MCP transport.
    let logical = st
        .logical_sessions
        .mint_session("", "list matrix items")
        .await
        .expect("mint UI fixture logical session");
    let opened = Box::pin(apply_capability_seeds(
        &st,
        None,
        None,
        vec![CapabilitySeed {
            entry_id: MATRIX_ENTRY_ID.into(),
            entity: "LangItem".into(),
        }],
        None,
        None,
        Some(logical.logical_session_id.as_uuid()),
        &logical.accumulated_intent,
    ))
    .await
    .expect("open UI fixture execute session");
    st.logical_execute_bindings
        .insert(
            logical.logical_session_id.as_uuid(),
            opened.prompt_hash,
            opened.session_id,
        )
        .await;
    let logical_session_ref = format_logical_session_wire_ref(logical.logical_session_id);
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
    (base, logical_session_ref, handle)
}

static SERVER: OnceCell<(String, String)> = OnceCell::const_new();

async fn server_context() -> (String, String) {
    SERVER
        .get_or_init(|| async {
            let (base, logical_session_ref, _handle) = Box::pin(spawn_server()).await;
            (base, logical_session_ref)
        })
        .await
        .clone()
}

fn strip_meta_like_cursor(body: &mut Value) {
    if let Some(obj) = body.as_object_mut() {
        obj.remove("_meta");
    }
}

fn receipt_token<'a>(body: &'a Value, key: &str) -> &'a str {
    let text = body
        .pointer("/mcp_result/content/0/text")
        .and_then(Value::as_str)
        .expect("agent content receipt");
    text.lines()
        .find_map(|line| {
            let (name, value) = line.split_once('\t')?;
            (name == key).then_some(value)
        })
        .unwrap_or_else(|| panic!("missing receipt token {key}: {text}"))
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
    let (base, ls_ref) = server_context().await;
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
                "protocolVersion": "2025-11-25",
                "capabilities": {"extensions": {"io.modelcontextprotocol/ui": {
                    "mimeTypes": ["text/html;profile=mcp-app"]
                }}},
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

    let plan = mcp_sse::mcp_tool_meta(
        &client,
        &base,
        &mcp_session,
        "plasm",
        json!({
            "logical_session_ref": ls_ref,
            "program": "class ReadItems(Program):\n    def build(self):\n        return e1.query().take(2)\n"
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

    let run_ref = receipt_token(&plan, "run_ref");
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
