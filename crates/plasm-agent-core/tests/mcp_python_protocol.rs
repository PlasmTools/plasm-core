//! Python admission and inbound/outbound MCP over real loopback HTTP.
//! Session exposure is explicit in the first test; it is not semantic discovery.
#![recursion_limit = "256"]
use axum::response::{IntoResponse, Response};
use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use plasm_agent_core::{
    http::{build_plasm_host_state, PlasmHostBootstrap},
    server_state::CatalogBootstrap,
};
use plasm_runtime::{
    mcp_execution::McpBindings,
    mcp_transport::{McpHttpConfig, McpSession},
    ExecutionConfig, ExecutionEngine, ExecutionMode,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
#[path = "support/mcp_discovery.rs"]
mod discovery;

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn serve(router: Router) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    (
        url,
        Server(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap()
        })),
    )
}

type Calls = Arc<Mutex<Vec<Value>>>;
async fn peer(State(calls): State<Calls>, Json(request): Json<Value>) -> Response {
    calls.lock().unwrap().push(request.clone());
    if request["method"] == "notifications/initialized" {
        return StatusCode::ACCEPTED.into_response();
    }
    let result = match request["method"].as_str().unwrap() {
        "initialize" => {
            json!({"protocolVersion":request["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"independent-peer","version":"1"}})
        }
        "tools/list" => {
            json!({"tools":[{"name":"list_items","inputSchema":{"type":"object","additionalProperties":false}}]})
        }
        "tools/call" => {
            json!({"content":[{"type":"text","text":"original receipt"}],"structuredContent":{"items":[{"id":7}]},"isError":request["params"]["name"]=="tool_error","_meta":{"receipt":"retained"}})
        }
        other => panic!("unexpected protocol method {other}"),
    };
    (
        StatusCode::OK,
        Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})),
    )
        .into_response()
}
async fn connect(url: &str) -> McpSession {
    McpSession::connect_http(McpHttpConfig {
        url: url.parse().unwrap(),
        headers: HashMap::new(),
        deadline: Duration::from_secs(30),
    })
    .await
    .unwrap()
}
struct WireClient {
    http: reqwest::Client,
    url: String,
    session: String,
    next_id: u64,
}
impl WireClient {
    async fn connect(url: &str) -> Self {
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let response = http.post(url).header("Accept", "application/json, text/event-stream").json(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"protocol-integration","version":"1"}}})).send().await.unwrap().error_for_status().unwrap();
        let session = response.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let mut client = Self {
            http,
            url: url.into(),
            session,
            next_id: 1,
        };
        let initialized = client
            .request("notifications/initialized", json!({}), false)
            .await;
        assert!(initialized.is_null());
        client
    }
    async fn request(&mut self, method: &str, params: Value, reply: bool) -> Value {
        let mut message = json!({"jsonrpc":"2.0","method":method,"params":params});
        if reply {
            message["id"] = json!(self.next_id);
            self.next_id += 1;
        }
        let response = self
            .http
            .post(&self.url)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25")
            .header("Mcp-Session-Id", &self.session)
            .json(&message)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        if !reply {
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            return Value::Null;
        }
        let text = response.text().await.unwrap();
        let response: Value = if text.trim_start().starts_with('{') {
            serde_json::from_str(&text).expect("JSON response")
        } else {
            text.replace("\r\n", "\n")
                .split("\n\n")
                .filter_map(|event| {
                    let data = event
                        .lines()
                        .filter_map(|line| line.strip_prefix("data:"))
                        .map(|line| line.strip_prefix(' ').unwrap_or(line))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if data.is_empty() {
                        None
                    } else {
                        Some(serde_json::from_str::<Value>(&data).expect("SSE JSON data"))
                    }
                })
                .find(|value| value.get("id") == message.get("id"))
                .expect("matching response after SSE priming and notifications")
        };
        assert_eq!(response["id"], message["id"]);
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }
    async fn shutdown(&self) {
        self.http
            .delete(&self.url)
            .header("Mcp-Session-Id", &self.session)
            .header("MCP-Protocol-Version", "2025-11-25")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
}
async fn call(client: &mut WireClient, name: &str, arguments: Value) -> Value {
    client
        .request(
            "tools/call",
            json!({"name":name,"arguments":arguments}),
            true,
        )
        .await
}
fn tool_calls(calls: &Calls) -> usize {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["method"] == "tools/call")
        .count()
}
async fn assert_run_evidence(client: &mut WireClient, run: &Value) -> Value {
    let step = &run["_meta"]["plasm"]["steps"][0];
    assert_eq!(step["coverage"], "unknown");
    assert_eq!(step["row_count"], 1);
    assert_eq!(step["column_schema"]["columns"][0]["wire_type"], "integer");
    let resource = client
        .request("resources/read", json!({"uri":step["artifact_uri"]}), true)
        .await;
    assert_eq!(resource["contents"][0]["mimeType"], "text/markdown");
    let text = resource["contents"][0]["text"]
        .as_str()
        .expect("artifact text");
    assert!(text.contains("id\n7\n"), "{resource}");
    resource
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an isolated PLASM_DISCOVERY_DATABASE_URL with pgvector and a dummy OPENROUTER_API_KEY"]
async fn intent_discovery_python_inline_read_and_extension_over_mcp() {
    assert_eq!(
        std::env::var("OPENROUTER_API_KEY").unwrap(),
        "offline-test-no-provider-calls"
    );
    let calls = Calls::default();
    let (backend_url, _backend) = serve(
        Router::new()
            .route("/mcp", post(peer))
            .with_state(calls.clone()),
    )
    .await;
    let outbound = connect(&backend_url).await;
    let dir = tempfile::tempdir().unwrap();
    let cgs = discovery::pack(
        dir.path(),
        plasm_core::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/mcp_transport_matrix"),
        )
        .unwrap(),
    );
    let registry = Arc::new(plasm_core::discovery::CgsRegistry::from_pairs(vec![(
        "matrix".into(),
        "MCP matrix".into(),
        vec![],
        Arc::new(cgs.clone()),
    )]));
    let engine = ExecutionEngine::new(ExecutionConfig {
        hydrate: false,
        ..Default::default()
    })
    .unwrap()
    .with_mcp_bindings(McpBindings::new(HashMap::from([(
        "fixture".into(),
        outbound,
    )])));
    let mut host = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry,
        catalog_bootstrap: CatalogBootstrap::CatalogDir {
            path: dir.path().into(),
        },
        incoming_auth: None,
        run_artifacts: Arc::new(plasm_agent_core::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
    .unwrap();
    host.oss.live_plan_pool =
        Arc::new(plasm_agent_core::live_plan_run_worker::LivePlanRunPool::new());
    let generation = host.catalog.activate_discovery().await.unwrap();
    let store = host.catalog.discovery_store().await.unwrap();
    discovery::seed_model_boundary(store, &generation, &cgs, &["Observe items"]).await;
    discovery::seed_model_boundary(
        store,
        &generation,
        &cgs,
        &["Observe items", "Inspect available item operations"],
    )
    .await;
    let host = Arc::new(host);
    let router = plasm_agent_core::mcp_server::build_mcp_router_for_transport(
        host.clone(),
        plasm_agent_core::mcp_server::McpHttpTransport::StreamableHttp,
    )
    .await
    .unwrap();
    let (host_url, _server) = serve(router).await;
    let mut client = WireClient::connect(&host_url).await;
    let context = call(
        &mut client,
        "plasm_context",
        json!({"session_mode":"new","intent":"Observe items"}),
    )
    .await;
    assert_ne!(context["isError"], true, "{context}");
    let logical_ref = context
        .pointer("/_meta/plasm/logical_session_ref")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("context: {context}"));
    assert_eq!(tool_calls(&calls), 0);
    // Use declarations delivered by the public protocol, never caller-selected seeds.
    let text = context["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let entity = text
        .lines()
        .find_map(|line| {
            line.strip_suffix(':').filter(|symbol| {
                symbol
                    .strip_prefix('e')
                    .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
            })
        })
        .unwrap_or_else(|| panic!("no entity declaration: {text}"));
    let program =
        format!("class Observe(Program):\n    def build(self):\n        return {entity}.query()\n");
    let review = call(
        &mut client,
        "plasm",
        json!({"logical_session_ref":logical_ref,"program":program}),
    )
    .await;
    assert_ne!(review["isError"], true, "{review}");
    assert!(review.pointer("/_meta/plasm/run_ref").is_none());
    let run = review.clone();
    assert_ne!(run["isError"], true, "{run}");
    assert_eq!(tool_calls(&calls), 1);
    let artifact = assert_run_evidence(&mut client, &run).await;
    let extension = call(&mut client, "plasm_context", json!({"session_mode":"extend","logical_session_ref":logical_ref,"intent":"Inspect available item operations"})).await;
    assert_ne!(extension["isError"], true, "{extension}");
    assert_eq!(
        extension
            .pointer("/_meta/plasm/logical_session_ref")
            .and_then(Value::as_str),
        Some(logical_ref)
    );
    if let Some(path) = std::env::var_os("PLASM_MCP_TEST_EVIDENCE") {
        std::fs::write(path, serde_json::to_vec_pretty(&json!({"mode":"deterministic cached model boundary; real pgvector and both MCP HTTP hops", "context":context,"program":program,"review":review,"run":run,"artifact":artifact,"extension":extension,"outbound_requests":*calls.lock().unwrap()})).unwrap()).unwrap();
    }
    client.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_inline_read_uses_mcp_and_rejection_dispatches_nothing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("rmcp=debug,plasm_agent_core=debug")
        .with_test_writer()
        .try_init();
    let calls = Calls::default();
    let (backend_url, _backend) = serve(
        Router::new()
            .route("/mcp", post(peer))
            .with_state(calls.clone()),
    )
    .await;
    let mut outbound = connect(&backend_url).await;
    let listed = outbound
        .list_tools(None)
        .await
        .unwrap_or_else(|error| panic!("{error}; requests: {:?}", calls.lock().unwrap()));
    assert_eq!(listed.tools[0].name, "list_items");
    let cgs = Arc::new(
        plasm_core::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/mcp_transport_matrix"),
        )
        .unwrap(),
    );
    let registry = Arc::new(plasm_core::discovery::CgsRegistry::from_pairs(vec![(
        "matrix".into(),
        "MCP matrix".into(),
        vec![],
        cgs,
    )]));
    let engine = ExecutionEngine::new(ExecutionConfig {
        hydrate: false,
        ..Default::default()
    })
    .unwrap()
    .with_mcp_bindings(McpBindings::new(HashMap::from([(
        "fixture".into(),
        outbound,
    )])));
    let mut host = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry,
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(plasm_agent_core::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
    .unwrap();
    host.oss.live_plan_pool =
        Arc::new(plasm_agent_core::live_plan_run_worker::LivePlanRunPool::new());
    let host = Arc::new(host);
    let logical = uuid::Uuid::new_v4();
    let opened = plasm_agent_core::http_execute::apply_capability_seeds(
        &host,
        None,
        None,
        vec![plasm_agent_core::http_execute::CapabilitySeed {
            entry_id: "matrix".into(),
            entity: "Item".into(),
        }],
        None,
        None,
        Some(logical),
        "Observe items",
    )
    .await
    .unwrap();
    host.logical_sessions
        .register_routed_session(
            plasm_agent_core::session_identity::LogicalSessionId(logical),
            "",
            &plasm_agent_core::intent_provenance::IntentProvenance::from_turns([
                "Observe items".into()
            ])
            .unwrap(),
            None,
        )
        .await
        .unwrap();
    host.logical_execute_bindings
        .insert(
            logical,
            opened.prompt_hash.clone(),
            opened.session_id.clone(),
        )
        .await;
    let es = host
        .get_execute_session(&opened.prompt_hash, &opened.session_id)
        .await
        .unwrap();
    let symbols = plasm_agent_core::plasm_plan_run::symbol_map_for_plasm_surface_parse(&es, None);
    let entity = symbols.entity_sym_for("matrix", "Item");
    let logical_ref =
        plasm_agent_core::mcp_logical_ref::format_logical_session_wire_ref_from_uuid(logical);
    let router = plasm_agent_core::mcp_server::build_mcp_router_for_transport(
        host.clone(),
        plasm_agent_core::mcp_server::McpHttpTransport::StreamableHttp,
    )
    .await
    .unwrap();
    let (host_url, _server) = serve(router).await;
    // Optional standalone official-client check of both explicit protocol lifecycles.
    if let Some(binary) = std::env::var_os("PLASM_MCP_CLIENT_PROBE_BINARY") {
        for modern in [false, true] {
            let router = plasm_agent_core::mcp_server::build_mcp_router_for_transport(
                host.clone(),
                if modern {
                    plasm_agent_core::mcp_server::McpHttpTransport::Stateless
                } else {
                    plasm_agent_core::mcp_server::McpHttpTransport::StreamableHttp
                },
            )
            .await
            .unwrap();
            let (url, _server) = serve(router).await;
            let output = tokio::process::Command::new(&binary)
                .arg(url)
                .arg(if modern { "2026" } else { "2025" })
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "candidate failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            println!("{}", String::from_utf8_lossy(&output.stdout));
        }
    }
    let mut sdk = connect(&host_url).await;
    sdk.list_tools(None).await.expect("SDK-to-Plasm tools/list");
    sdk.shutdown().await.unwrap();
    let mut client = WireClient::connect(&host_url).await;
    let tools = client.request("tools/list", json!({}), true).await;
    serde_json::from_value::<plasm_runtime::mcp_transport::ListToolsResult>(tools.clone())
        .expect("SDK can decode the public tool list");
    assert!(tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "plasm_context"));
    let rejected = call(&mut client, "plasm", json!({"logical_session_ref":logical_ref,"program":"class Broken(Program):\n    def build(self):\n        return nonexistent.query()\n"})).await;
    assert_eq!(
        rejected.pointer("/_meta/plasm/dry_verdict"),
        Some(&json!("needs_fix")),
        "{rejected}"
    );
    assert_eq!(tool_calls(&calls), 0);
    let program = format!(
        "class ReadItems(Program):\n    def build(self):\n        return {entity}.query()\n"
    );
    let review = call(
        &mut client,
        "plasm",
        json!({"logical_session_ref":logical_ref,"program":program}),
    )
    .await;
    assert_ne!(review["isError"], true, "{review}");
    assert!(review.pointer("/_meta/plasm/run_ref").is_none());
    let run = review;
    assert_ne!(run["isError"], true, "{run}");
    assert_eq!(tool_calls(&calls), 1, "{run}");
    assert_run_evidence(&mut client, &run).await;
    client.shutdown().await;
}
