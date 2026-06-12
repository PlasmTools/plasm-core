//! Dual-surface E2E: workflow HTTP routes + mandatory plan_ux_reflection on dry-run.

#[path = "common/hermit_workflow_matrix.rs"]
mod hermit_workflow_matrix;

#[path = "common/workflow_matrix.rs"]
mod workflow_matrix;

use plasm_agent::http::{serve_discovery_execute_and_mcp_unified, DiscoveryHttpServeOpts};
use plasm_runtime::{ExecutionConfig, ExecutionEngine};
use reqwest::header::LOCATION;
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::OnceCell;
use workflow_matrix::{load_workflow_matrix_cgs, workflow_federated_host_state, CATALOG_A};

async fn spawn_workflow_server() -> (String, tokio::task::JoinHandle<()>) {
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
            let (base, _handle) = spawn_workflow_server().await;
            base
        })
        .await
        .clone()
}

fn assert_plan_ux_reflection(body: &Value) {
    let reflection = body
        .pointer("/_meta/plasm/plan_ux_reflection")
        .or_else(|| body.pointer("/plan_ux_reflection"))
        .unwrap_or_else(|| panic!("plan_ux_reflection missing in {body}"));
    assert_eq!(
        reflection.get("schema_version").and_then(|v| v.as_u64()),
        Some(1)
    );
    assert!(reflection.get("steps").and_then(|v| v.as_array()).is_some());
    for step in reflection["steps"].as_array().into_iter().flatten() {
        let op = step["operation"].as_str().unwrap_or("");
        assert!(
            !op.contains("PlanDryOp"),
            "operation must be human-readable, got {op}"
        );
    }
}

fn assert_plan_dag_human_ops(body: &Value) {
    let plan = body
        .pointer("/_meta/plasm/plan")
        .expect("plan missing in _meta.plasm");
    let nodes = plan["nodes"].as_array().expect("plan.nodes");
    assert!(!nodes.is_empty(), "plan.nodes must be non-empty");
    for node in nodes {
        let op = node["operation"].as_str().unwrap_or("");
        assert!(
            !op.contains("PlanDryOp") && !op.is_empty(),
            "plan node operation must be human-readable, got {op:?}"
        );
    }
    assert!(plan["edges"].is_array(), "plan.edges required");
}

async fn http_open_workflow_session(client: &reqwest::Client, base: &str) -> (String, String) {
    let resp = client
        .post(format!("{base}/execute"))
        .header("accept", "application/json")
        .json(&json!({
            "entry_id": CATALOG_A,
            "entities": ["WorkItem"],
        }))
        .send()
        .await
        .expect("open session");
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let loc = resp
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("location")
        .to_string();
    let body: Value = client
        .get(format!("{base}{loc}"))
        .header("accept", "application/json")
        .send()
        .await
        .expect("get session")
        .json()
        .await
        .expect("session json");
    (
        body["prompt_hash"].as_str().expect("ph").to_string(),
        body["session"].as_str().expect("sid").to_string(),
    )
}

async fn mcp_tool_meta(
    client: &reqwest::Client,
    base: &str,
    mcp_session: &str,
    tool: &str,
    args: Value,
    id: u64,
) -> Value {
    let resp = client
        .post(format!("{base}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("MCP-Session-Id", mcp_session)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args }
        }))
        .send()
        .await
        .expect("mcp call");
    let text = resp.text().await.expect("mcp body");
    let mut result = json!({});
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(j) = serde_json::from_str::<Value>(rest) else {
            continue;
        };
        if j.get("id").and_then(|v| v.as_u64()) == Some(id) {
            result = j.get("result").cloned().unwrap_or(json!({}));
            break;
        }
    }
    let meta = result.get("_meta").cloned().unwrap_or(json!({}));
    let structured = result
        .get("structuredContent")
        .or_else(|| result.get("structured_content"))
        .cloned()
        .unwrap_or(json!({}));
    json!({ "_meta": meta, "structuredContent": structured, "mcp_result": result })
}

#[test]
fn workflow_apps_e2e() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(workflow_apps_e2e_async());
        })
        .expect("spawn")
        .join()
        .expect("join");
}

async fn workflow_apps_e2e_async() {
    let base = base_url().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");

    let vm = client
        .get(format!(
            "{base}/v1/workflows/workflow_matrix_parallel/view-model"
        ))
        .send()
        .await
        .expect("view-model");
    assert_eq!(vm.status(), StatusCode::OK);
    let vm_body: Value = vm.json().await.expect("json");
    assert_eq!(vm_body["id"], "workflow_matrix_parallel");
    assert_eq!(vm_body["seeds"].as_array().map(|a| a.len()), Some(2));
    assert_eq!(vm_body["ready"], true);

    let inst = client
        .post(format!(
            "{base}/v1/workflows/workflow_matrix_parallel/instantiate"
        ))
        .json(&json!({ "parameters": { "limit": 3 } }))
        .send()
        .await
        .expect("instantiate");
    assert_eq!(inst.status(), StatusCode::OK);
    let inst_body: Value = inst.json().await.expect("json");
    let program = inst_body["program"].as_str().expect("program");
    assert!(program.contains("e1") && program.contains('3'));

    let (ph, sid) = http_open_workflow_session(&client, &base).await;
    let dry = client
        .post(format!("{base}/execute/{ph}/{sid}?mode=plan"))
        .header("accept", "application/json")
        .header("content-type", "text/plain")
        .body("items = WorkItem.limit(3)\nitems")
        .send()
        .await
        .expect("http dry");
    let dry_status = dry.status();
    let dry_body: Value = dry.json().await.expect("dry json");
    assert!(
        dry_status.is_success(),
        "http dry: {dry_status} body={dry_body}"
    );
    assert_plan_ux_reflection(&dry_body);
    assert_plan_dag_human_ops(&dry_body);

    let list = client
        .get(format!("{base}/v1/workflows"))
        .send()
        .await
        .expect("list workflows");
    assert_eq!(list.status(), StatusCode::OK);
    let list_body: Value = list.json().await.expect("list json");
    assert!(list_body.as_array().map(|a| !a.is_empty()).unwrap_or(false));

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
                "capabilities": {},
                "clientInfo": { "name": "workflow-e2e", "version": "0" }
            }
        }))
        .send()
        .await
        .expect("mcp init");
    let mcp_session = init
        .headers()
        .get("mcp-session-id")
        .or_else(|| init.headers().get("MCP-Session-Id"))
        .and_then(|v| v.to_str().ok())
        .expect("mcp session id")
        .to_string();

    let ctx = mcp_tool_meta(
        &client,
        &base,
        &mcp_session,
        "plasm_context",
        json!({
            "intent": "workflow matrix reads",
            "seeds": [
                { "api": "catalog_a", "entity": "WorkItem" },
                { "api": "catalog_b", "entity": "WorkItem" }
            ]
        }),
        2,
    )
    .await;
    let ls = ctx
        .pointer("/_meta/plasm/logical_session_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("s0");

    let dry_mcp = mcp_tool_meta(
        &client,
        &base,
        &mcp_session,
        "plasm",
        json!({
            "logical_session_ref": ls,
            "program": "a = e1.limit(1)\nb = e2.limit(1)\na"
        }),
        3,
    )
    .await;
    assert_plan_ux_reflection(&dry_mcp);
    assert_plan_dag_human_ops(&dry_mcp);

    let open_wf = mcp_tool_meta(
        &client,
        &base,
        &mcp_session,
        "open_workflow",
        json!({
            "id": "workflow_matrix_parallel",
            "intent": "workflow matrix e2e open",
        }),
        4,
    )
    .await;
    let open_meta = open_wf
        .pointer("/_meta/plasm")
        .expect("open_workflow _meta.plasm");
    assert_eq!(open_meta["view_model"]["ready"], true);
    let ls_wf = open_meta["logical_session_ref"]
        .as_str()
        .expect("logical_session_ref");

    let dry_wf = mcp_tool_meta(
        &client,
        &base,
        &mcp_session,
        "dry_workflow",
        json!({
            "id": "workflow_matrix_parallel",
            "logical_session_ref": ls_wf,
            "parameters": { "limit": 3 },
        }),
        5,
    )
    .await;
    assert_plan_ux_reflection(&dry_wf);
    assert_plan_dag_human_ops(&dry_wf);

    let plan_shell = client
        .get(format!("{base}/v1/plan/ui"))
        .send()
        .await
        .expect("plan ui shell");
    assert_eq!(plan_shell.status(), StatusCode::OK);
    let shell_html = plan_shell.text().await.expect("shell html");
    assert!(shell_html.contains("Plasm Plan Review"));
    assert!(shell_html.contains("/v1/plan/ui/shell.js"));

    let plan_app = client
        .get(format!("{base}/v1/plan/ui/app"))
        .send()
        .await
        .expect("plan ui app");
    assert_eq!(plan_app.status(), StatusCode::OK);
    let app_html = plan_app.text().await.expect("app html");
    assert!(app_html.contains("plan-canvas-host"));
    assert!(app_html.contains("/v1/plan/ui/app.js"));

    let templates = client
        .post(format!("{base}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("MCP-Session-Id", &mcp_session)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "resources/templates/list",
            "params": {}
        }))
        .send()
        .await
        .expect("templates list");
    let templates_text = templates.text().await.expect("templates body");
    assert!(
        templates_text.contains("ui://plasm/plan-review"),
        "plan review resource template missing: {templates_text}"
    );

    let plan_resource = client
        .post(format!("{base}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("MCP-Session-Id", &mcp_session)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "resources/read",
            "params": { "uri": "ui://plasm/plan-review" }
        }))
        .send()
        .await
        .expect("plan resource read");
    let resource_text = plan_resource.text().await.expect("resource body");
    assert!(
        resource_text.contains("Plasm Plan Review"),
        "plan review resource HTML missing title: {}",
        &resource_text[..resource_text.len().min(500)]
    );

    assert!(
        dry_mcp
            .pointer("/_meta/ui/resourceUri")
            .and_then(|v| v.as_str())
            == Some("ui://plasm/plan-review"),
        "plasm dry-run must attach plan review ui meta: {dry_mcp}"
    );

    let dry_structured_plasm = dry_mcp
        .pointer("/structuredContent/plasm")
        .or_else(|| dry_mcp.pointer("/mcp_result/structuredContent/plasm"));
    assert!(
        dry_structured_plasm
            .and_then(|p| p.get("plan"))
            .is_some(),
        "plasm dry-run must mirror plan into structuredContent.plasm: {dry_mcp}"
    );
    assert_eq!(
        dry_mcp.pointer("/_meta/plasm/plan"),
        dry_structured_plasm.and_then(|p| p.get("plan")),
        "structuredContent.plasm.plan must mirror _meta.plasm.plan"
    );

    let plan_commit_ref = dry_mcp
        .pointer("/_meta/plasm/plan_commit_ref")
        .and_then(|v| v.as_str())
        .expect("plan_commit_ref from dry-run");
    let _ = plan_commit_ref;

    let run_mcp = mcp_tool_meta(
        &client,
        &base,
        &mcp_session,
        "plasm_run",
        json!({
            "logical_session_ref": ls,
            "program": "e1.limit(5)",
            "force": true,
        }),
        8,
    )
    .await;
    assert!(
        !run_mcp
            .pointer("/mcp_result/isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "plasm_run should succeed: {run_mcp}"
    );

    let small_steps = run_mcp
        .pointer("/_meta/plasm/steps")
        .and_then(|v| v.as_array())
        .expect("small plasm_run must emit _meta.plasm.steps");
    assert!(
        !small_steps.is_empty(),
        "small plasm_run steps must be non-empty: {run_mcp}"
    );
    assert_eq!(
        run_mcp
            .pointer("/_meta/ui/resourceUri")
            .and_then(|v| v.as_str()),
        Some("ui://plasm/run-explorer"),
        "small plasm_run must attach run-explorer ui meta: {run_mcp}"
    );
    let first_small = &small_steps[0];
    assert!(
        first_small
            .get("return_label")
            .and_then(|v| v.as_str())
            .is_some(),
        "step must include return_label: {first_small}"
    );
    assert!(
        first_small
            .get("display")
            .and_then(|v| v.as_str())
            .is_some(),
        "step must include display: {first_small}"
    );
    assert!(
        first_small
            .get("row_count")
            .and_then(|v| v.as_u64())
            .is_some(),
        "step must include row_count: {first_small}"
    );
    assert!(
        first_small
            .get("preview_entities")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty()),
        "bounded small plasm_run must inline preview_entities: {first_small}"
    );

    let run_structured_steps = run_mcp
        .pointer("/structuredContent/plasm/steps")
        .or_else(|| run_mcp.pointer("/mcp_result/structuredContent/plasm/steps"));
    assert!(
        run_structured_steps.and_then(|v| v.as_array()).is_some_and(|a| !a.is_empty()),
        "plasm_run must mirror steps into structuredContent.plasm: {run_mcp}"
    );
    assert_eq!(
        run_mcp.pointer("/_meta/plasm/steps"),
        run_structured_steps,
        "structuredContent.plasm.steps must mirror _meta.plasm.steps"
    );

    let run_large_mcp = mcp_tool_meta(
        &client,
        &base,
        &mcp_session,
        "plasm_run",
        json!({
            "logical_session_ref": ls,
            "program": "items = e1.limit(5)[id,title]\nwide = items <<PLASM_RUN_UI_E2E_WIDE\n{% for r in rows %}{% for i in range(500) %}w{% endfor %}\n{% endfor %}\nPLASM_RUN_UI_E2E_WIDE\nwide",
            "force": true,
        }),
        11,
    )
    .await;
    assert!(
        !run_large_mcp
            .pointer("/mcp_result/isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "plasm_run multi-return should succeed: {run_large_mcp}"
    );
    let run_steps = run_large_mcp
        .pointer("/_meta/plasm/steps")
        .and_then(|v| v.as_array());
    assert!(
        run_steps.is_some_and(|s| !s.is_empty()),
        "multi-return plasm_run must emit truncated _meta.plasm.steps: {run_large_mcp}"
    );
    assert_eq!(
        run_large_mcp
            .pointer("/_meta/ui/resourceUri")
            .and_then(|v| v.as_str()),
        Some("ui://plasm/run-explorer"),
        "live plasm_run with artifact steps must attach run-explorer ui meta: {run_large_mcp}"
    );
    let large_steps = run_steps.expect("run_steps");
    assert!(
        large_steps.iter().any(|step| {
            step.get("dict_ref").is_some()
                || step.get("artifact_uri").is_some()
                || step.get("run_id").is_some()
        }),
        "truncated multi-return plasm_run steps must reference artifacts: {run_large_mcp}"
    );

    let tools_list = client
        .post(format!("{base}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("MCP-Session-Id", &mcp_session)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/list",
            "params": {}
        }))
        .send()
        .await
        .expect("tools list");
    let tools_text = tools_list.text().await.expect("tools list body");
    assert!(
        tools_text.contains("ui://plasm/run-explorer"),
        "plasm_run tool must advertise run-explorer ui meta: {}",
        &tools_text[..tools_text.len().min(800)]
    );

    let run_shell = client
        .get(format!("{base}/v1/run/ui"))
        .send()
        .await
        .expect("run ui shell");
    assert_eq!(run_shell.status(), StatusCode::OK);
    let run_shell_html = run_shell.text().await.expect("run shell html");
    assert!(run_shell_html.contains("Plasm Run Explorer"));
    assert!(run_shell_html.contains("/v1/run/ui/shell.js"));

    let run_app = client
        .get(format!("{base}/v1/run/ui/app"))
        .send()
        .await
        .expect("run ui app");
    assert_eq!(run_app.status(), StatusCode::OK);

    let run_app_js = client
        .get(format!("{base}/v1/run/ui/app.js"))
        .send()
        .await
        .expect("run ui app js");
    assert_eq!(run_app_js.status(), StatusCode::OK);

    let run_resource = client
        .post(format!("{base}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("MCP-Session-Id", &mcp_session)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "resources/read",
            "params": { "uri": "ui://plasm/run-explorer" }
        }))
        .send()
        .await
        .expect("run resource read");
    let run_resource_text = run_resource.text().await.expect("run resource body");
    assert!(
        run_resource_text.contains("Plasm Run Explorer"),
        "run explorer resource HTML missing title: {}",
        &run_resource_text[..run_resource_text.len().min(500)]
    );
    assert!(
        run_resource_text.contains("run-step-rail")
            || run_resource_text.contains("Plasm Run Explorer"),
        "run explorer resource HTML missing shell markers: {}",
        &run_resource_text[..run_resource_text.len().min(500)]
    );
}
