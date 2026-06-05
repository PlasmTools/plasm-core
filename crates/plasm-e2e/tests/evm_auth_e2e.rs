#![cfg(feature = "evm")]

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use plasm_core::{loader, Expr, GetExpr};
use plasm_runtime::{
    AuthResolver, ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode,
    SessionMaterialization, StreamConsumeOpts,
};
use serde_json::json;
use std::{
    collections::HashMap,
    fs,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;

#[derive(Clone)]
struct RpcState {
    expected_header: Option<(String, String)>,
    expected_query: Option<(String, String)>,
    request_count: Arc<Mutex<usize>>,
}

#[tokio::test]
#[serial_test::serial]
async fn evm_call_applies_api_key_header_auth() {
    let env_name = "PLASM_EVM_HEADER_AUTH_TEST_KEY";
    // SAFETY: serialized via #[serial_test::serial] — no concurrent env access.
    unsafe { std::env::set_var(env_name, "header-secret") };

    let (base_url, request_count, server) = spawn_mock_rpc_server(
        Some(("x-api-key".to_string(), "header-secret".to_string())),
        None,
    )
    .await;

    let schema_dir = write_schema_dir(&format!(
        r#"auth:
  scheme: api_key_header
  header: x-api-key
  env: {env_name}
entities:
  Balance:
    id_field: account
    fields:
      account:
        field_type: string
        required: true
      balance:
        field_type: string
        required: true
    relations: {{}}
capabilities:
  balance_get:
    kind: get
    entity: Balance
"#
    ));

    let cgs = loader::load_schema_dir(schema_dir.path()).unwrap();
    let result = execute_balance_get(&cgs, &base_url).await;
    assert_eq!(result, Some("42".to_string()));
    assert_eq!(*request_count.lock().unwrap(), 1);

    server.abort();
    // SAFETY: serialized via #[serial_test::serial] — no concurrent env access.
    unsafe { std::env::remove_var(env_name) };
}

#[tokio::test]
#[serial_test::serial]
async fn evm_call_applies_api_key_query_auth() {
    let env_name = "PLASM_EVM_QUERY_AUTH_TEST_KEY";
    // SAFETY: serialized via #[serial_test::serial] — no concurrent env access.
    unsafe { std::env::set_var(env_name, "query-secret") };

    let (base_url, request_count, server) = spawn_mock_rpc_server(
        None,
        Some(("apikey".to_string(), "query-secret".to_string())),
    )
    .await;

    let schema_dir = write_schema_dir(&format!(
        r#"auth:
  scheme: api_key_query
  param: apikey
  env: {env_name}
entities:
  Balance:
    id_field: account
    fields:
      account:
        field_type: string
        required: true
      balance:
        field_type: string
        required: true
    relations: {{}}
capabilities:
  balance_get:
    kind: get
    entity: Balance
"#
    ));

    let cgs = loader::load_schema_dir(schema_dir.path()).unwrap();
    let result = execute_balance_get(&cgs, &base_url).await;
    assert_eq!(result, Some("42".to_string()));
    assert_eq!(*request_count.lock().unwrap(), 1);

    server.abort();
    // SAFETY: serialized via #[serial_test::serial] — no concurrent env access.
    unsafe { std::env::remove_var(env_name) };
}

async fn execute_balance_get(cgs: &plasm_core::CGS, base_url: &str) -> Option<String> {
    let auth_resolver = cgs.auth.clone().map(AuthResolver::from_env);
    let mut config = ExecutionConfig::default();
    config.base_url = Some(base_url.to_string());
    let engine = ExecutionEngine::new_with_auth(config, auth_resolver).unwrap();
    let mut cache = SessionMaterialization::new();
    let result = engine
        .execute(
            &Expr::Get(GetExpr::new(
                "Balance",
                "0x00000000000000000000000000000000000000aa",
            )),
            cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await
        .unwrap();

    result.entities[0]
        .fields
        .get("balance")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
}

async fn spawn_mock_rpc_server(
    expected_header: Option<(String, String)>,
    expected_query: Option<(String, String)>,
) -> (String, Arc<Mutex<usize>>, tokio::task::JoinHandle<()>) {
    let request_count = Arc::new(Mutex::new(0usize));
    let state = RpcState {
        expected_header,
        expected_query,
        request_count: Arc::clone(&request_count),
    };

    let app = Router::new()
        .route("/", post(mock_rpc_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (format!("http://{addr}"), request_count, server)
}

async fn mock_rpc_handler(
    State(state): State<RpcState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    if let Some((key, expected)) = &state.expected_header {
        let actual = headers.get(key).and_then(|v| v.to_str().ok());
        if actual != Some(expected.as_str()) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "missing expected auth header" })),
            );
        }
    }

    if let Some((key, expected)) = &state.expected_query {
        if params.get(key) != Some(expected) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "missing expected auth query param" })),
            );
        }
    }

    *state.request_count.lock().unwrap() += 1;
    let id = body.get("id").cloned().unwrap_or_else(|| json!(1));

    (
        StatusCode::OK,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": format!("0x{:064x}", 42_u64),
        })),
    )
}

fn write_schema_dir(domain_yaml: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("domain.yaml"), domain_yaml).unwrap();
    fs::write(
        dir.path().join("mappings.yaml"),
        r#"balance_get:
  transport: evm_call
  chain: 1
  contract:
    type: const
    value: "0x0000000000000000000000000000000000000001"
  function: "function balanceOf(address owner) view returns (uint256)"
  args:
    - type: var
      name: id
  decode:
    account:
      type: input
      index: 0
    balance:
      type: output
      index: 0
"#,
    )
    .unwrap();
    dir
}
