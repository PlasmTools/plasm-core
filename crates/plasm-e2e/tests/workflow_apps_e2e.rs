//! Dual-surface E2E: workflow HTTP routes + plan archive on dry-run (agent channel stays compact).

#[path = "common/hermit_workflow_matrix.rs"]
mod hermit_workflow_matrix;

#[path = "common/mcp_sse.rs"]
mod mcp_sse;

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

fn plan_ux_reflection_from_body(body: &Value) -> &Value {
    body.pointer("/_meta/ui/plasm/plan_ux_reflection")
        .or_else(|| body.pointer("/_meta/plasm/plan_ux_reflection"))
        .or_else(|| body.get("plan_ux_reflection"))
        .or_else(|| body.pointer("/plan_ux_reflection"))
        .unwrap_or_else(|| panic!("plan_ux_reflection missing in {body}"))
}

fn assert_agent_mcp_tool_compact(body: &Value) {
    assert!(
        body.pointer("/structuredContent/plasm/comp").is_none(),
        "agent structuredContent.plasm must omit comp: {body}"
    );
    assert!(
        body.pointer("/structuredContent/plasm/steps").is_none(),
        "agent structuredContent.plasm must omit snapshot steps: {body}"
    );
    assert!(
        body.pointer("/structuredContent/ui/comp").is_none(),
        "structuredContent.ui must not carry comp DAG: {body}"
    );
    assert!(
        body.pointer("/structuredContent/ui/plan_ux_reflection").is_none(),
        "structuredContent.ui must not carry plan_ux_reflection: {body}"
    );
    assert!(
        body.pointer("/structuredContent/ui/preview_entities").is_none(),
        "structuredContent.ui must not carry preview_entities: {body}"
    );
    assert!(
        body.pointer("/_meta/ui/plasm").is_none(),
        "tool result must not embed UI DAG under _meta.ui.plasm: {body}"
    );
    let text = body
        .pointer("/content/0/text")
        .or_else(|| body.pointer("/mcp_result/content/0/text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        !text.is_empty(),
        "agent markdown content must be non-empty compact text: {body}"
    );
}

async fn http_read_plan_json(client: &reqwest::Client, base: &str, body: &Value) -> Value {
    let path = body
        .pointer("/structuredContent/ui/plan_http_path")
        .or_else(|| body.pointer("/structuredContent/plasm/plan_http_path"))
        .or_else(|| body.pointer("/_meta/plasm/plan_http_path"))
        .and_then(|v| v.as_str())
        .expect("plan_http_path on dry-run MCP tool");
    let resp = client
        .get(format!("{base}{path}"))
        .header("accept", "application/json")
        .send()
        .await
        .expect("plan http get");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "plan http GET must succeed: {}",
        resp.status()
    );
    resp.json().await.expect("plan http json")
}

async fn assert_plan_archive_via_mcp_read(
    client: &reqwest::Client,
    base: &str,
    mcp_session: &str,
    body: &Value,
    read_id: u64,
) {
    let plan_uri = body
        .pointer("/structuredContent/plasm/plan_uri")
        .or_else(|| body.pointer("/structuredContent/ui/plan_uri"))
        .or_else(|| body.pointer("/structuredContent/ui/canonical_plan_uri"))
        .or_else(|| body.pointer("/_meta/plasm/plan_uri"))
        .and_then(|v| v.as_str())
        .expect("canonical plan_uri on dry-run MCP tool");
    assert!(
        plan_uri.starts_with("plasm://execute/"),
        "plan_uri must be canonical execute URI for MCP read: {plan_uri}"
    );
    let archive =
        mcp_sse::mcp_read_resource_json(client, base, mcp_session, plan_uri, read_id).await;
    assert_plan_ux_reflection(&archive);
}

async fn assert_plan_ux_from_mcp_tool(
    client: &reqwest::Client,
    base: &str,
    mcp_session: &str,
    body: &Value,
    read_id: u64,
) {
    assert_agent_mcp_tool_compact(body);
    let archive = http_read_plan_json(client, base, body).await;
    assert_plan_ux_reflection(&archive);
    assert_plan_archive_via_mcp_read(client, base, mcp_session, body, read_id + 1000).await;
}

fn assert_comp_human_ops_from_value(comp: &Value, reflection: &Value) {
    let steps = comp["steps"].as_object().expect("comp.steps");
    assert!(!steps.is_empty(), "comp.steps must be non-empty");
    assert!(comp["bind"]["topo"].is_array(), "comp.bind.topo required");
    let ux_steps = reflection["steps"]
        .as_array()
        .expect("plan_ux_reflection.steps");
    assert!(
        !ux_steps.is_empty(),
        "plan_ux_reflection.steps must be non-empty"
    );
    for step in ux_steps {
        let op = step["operation"].as_str().unwrap_or("");
        assert!(
            !op.contains("PlanDryOp") && !op.is_empty(),
            "plan_ux step operation must be human-readable, got {op:?}"
        );
    }
}

fn assert_plan_ux_reflection(body: &Value) {
    let reflection = plan_ux_reflection_from_body(body);
    plasm_agent::plan_ux_reflection::validate_plan_ux_reflection_wire(reflection)
        .unwrap_or_else(|e| panic!("invalid plan_ux_reflection wire: {e}; got {reflection:?}"));
    for step in reflection["steps"].as_array().into_iter().flatten() {
        let op = step["operation"].as_str().unwrap_or("");
        assert!(
            !op.contains("PlanDryOp"),
            "operation must be human-readable, got {op}"
        );
    }
}

fn assert_comp_human_ops(body: &Value) {
    let comp = body
        .pointer("/_meta/ui/plasm/comp")
        .or_else(|| body.pointer("/_meta/plasm/comp"))
        .or_else(|| body.get("comp"))
        .expect("comp missing in _meta.ui.plasm, legacy _meta.plasm, or top-level comp");
    let reflection = plan_ux_reflection_from_body(body);
    assert_comp_human_ops_from_value(comp, reflection);
}

async fn assert_comp_human_ops_from_mcp_plan_archive(
    client: &reqwest::Client,
    base: &str,
    _mcp_session: &str,
    body: &Value,
    _read_id: u64,
) {
    let archive = http_read_plan_json(client, base, body).await;
    let comp = archive.get("comp").expect("plan archive comp");
    let reflection = archive
        .get("plan_ux_reflection")
        .expect("plan archive plan_ux_reflection");
    assert_comp_human_ops_from_value(comp, reflection);
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
    mcp_sse::mcp_tool_meta(client, base, mcp_session, tool, args, id).await
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
    assert_comp_human_ops(&dry_body);

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
            "session_mode": "new",
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
        .expect("logical_session_ref from plasm_context");

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
    assert_plan_ux_from_mcp_tool(&client, &base, &mcp_session, &dry_mcp, 9).await;
    assert_comp_human_ops_from_mcp_plan_archive(&client, &base, &mcp_session, &dry_mcp, 10).await;

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
    assert_plan_ux_from_mcp_tool(&client, &base, &mcp_session, &dry_wf, 11).await;
    assert_comp_human_ops_from_mcp_plan_archive(&client, &base, &mcp_session, &dry_wf, 12).await;

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
        dry_structured_plasm.and_then(|p| p.get("comp")).is_none(),
        "agent structuredContent.plasm must omit comp DAG: {dry_mcp}"
    );
    assert!(
        dry_structured_plasm
            .and_then(|p| p.get("plan_uri"))
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.starts_with("plasm://execute/")),
        "structuredContent.plasm must carry canonical plan_uri: {dry_mcp}"
    );
    assert!(
        dry_mcp.pointer("/structuredContent/plasm/plan_http_path").is_none(),
        "structuredContent.plasm must omit plan_http_path (UI channel only): {dry_mcp}"
    );
    assert!(
        dry_mcp
            .pointer("/structuredContent/ui/plan_http_path")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.starts_with("/execute/")),
        "structuredContent.ui must carry plan_http_path ref: {dry_mcp}"
    );
    assert!(
        dry_mcp.pointer("/structuredContent/ui/comp").is_none(),
        "structuredContent.ui must not include comp DAG: {dry_mcp}"
    );
    assert!(
        dry_mcp.pointer("/_meta/plasm/comp").is_none(),
        "agent _meta.plasm must omit comp: {dry_mcp}"
    );
    assert!(
        dry_mcp.pointer("/_meta/ui/plasm").is_none(),
        "dry-run must not embed comp under _meta.ui.plasm: {dry_mcp}"
    );
    assert!(
        dry_mcp.pointer("/_meta/plasm/plan_text").is_none(),
        "agent _meta.plasm must omit plan_text (structuredContent only): {dry_mcp}"
    );
    assert!(
        dry_structured_plasm
            .and_then(|p| p.get("plan_text"))
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty()),
        "structuredContent.plasm must carry compact plan_text: {dry_mcp}"
    );

    let run_ref = dry_mcp
        .pointer("/_meta/plasm/run_ref")
        .or_else(|| dry_mcp.pointer("/structuredContent/plasm/run_ref"))
        .and_then(|v| v.as_str())
        .expect("run_ref from dry-run");
    let _ = run_ref;

    let run_mcp = mcp_tool_meta(
        &client,
        &base,
        &mcp_session,
        "plasm_run",
        json!({
            "logical_session_ref": ls,
            "run_ref": run_ref,
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
    let preview = first_small
        .get("preview_entities")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty());
    let artifact_uri = first_small
        .get("artifact_uri")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    assert!(
        preview.is_some() || artifact_uri.is_some(),
        "bounded small plasm_run must inline preview_entities or attach artifact_uri: {first_small}"
    );
    if let Some(row) = preview.and_then(|a| a.first()) {
        for key in ["_ref", "_version", "_last_updated", "_completeness"] {
            assert!(
                row.get(key).is_none(),
                "preview_entities must not include cache key {key}: {row}"
            );
        }
    }
    assert!(
        first_small.get("column_schema").is_some(),
        "plasm_run step must include column_schema: {first_small}"
    );

    assert_agent_mcp_tool_compact(&run_mcp);
    let run_structured_steps = run_mcp
        .pointer("/structuredContent/plasm/steps")
        .or_else(|| run_mcp.pointer("/mcp_result/structuredContent/plasm/steps"));
    assert!(
        run_structured_steps.is_none(),
        "agent structuredContent.plasm must omit snapshot steps: {run_mcp}"
    );
    let markdown = run_mcp
        .pointer("/content/0/text")
        .or_else(|| run_mcp.pointer("/mcp_result/content/0/text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        markdown.contains("```tsv") || markdown.contains("```text"),
        "small plasm_run markdown must include inline TSV for agent channel: {markdown}"
    );

    let dry_large = mcp_tool_meta(
        &client,
        &base,
        &mcp_session,
        "plasm",
        json!({
            "logical_session_ref": ls,
            "program": "items = e1.limit(5)[id,title]\nwide = items <<PLASM_RUN_UI_E2E_WIDE\n{% for r in rows %}{% for i in range(500) %}w{% endfor %}\n{% endfor %}\nPLASM_RUN_UI_E2E_WIDE\nwide",
        }),
        10,
    )
    .await;
    let large_run_ref = dry_large
        .pointer("/_meta/plasm/run_ref")
        .and_then(|v| v.as_str())
        .expect("run_ref from large dry-run");

    let run_large_mcp = mcp_tool_meta(
        &client,
        &base,
        &mcp_session,
        "plasm_run",
        json!({
            "logical_session_ref": ls,
            "run_ref": large_run_ref,
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
