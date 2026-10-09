#![allow(dead_code)]
pub(crate) fn assert_callback_error(response: reqwest::Response, error: &str) {
    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    let redirect = reqwest::Url::parse(
        response.headers()[reqwest::header::LOCATION]
            .to_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        redirect
            .query_pairs()
            .find(|(key, _)| key == "error")
            .unwrap()
            .1,
        error
    );
    assert!(redirect.query_pairs().any(|(key, _)| key == "iss"));
    assert!(!redirect.query_pairs().any(|(key, _)| key == "code"));
}

pub(crate) async fn assert_streamable_tools(client: &reqwest::Client, base: &str, token: &str) {
    let response = client.post(format!("{base}/mcp")).bearer_auth(token).header("Accept", "application/json, text/event-stream").json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"auth-contract","version":"1"},"capabilities":{}}})).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let session = response
        .headers()
        .get("MCP-Session-Id")
        .map(|value| value.to_str().unwrap().to_owned());
    let initialization = decode_rpc_response(response).await;
    assert!(
        initialization["result"]["capabilities"]["tools"].is_object(),
        "{initialization}"
    );
    let request = client
        .post(format!("{base}/mcp"))
        .bearer_auth(token)
        .header("Accept", "application/json, text/event-stream")
        .json(&serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    let request = if let Some(session) = &session {
        request.header("MCP-Session-Id", session)
    } else {
        request
    };
    assert_eq!(
        request.send().await.unwrap().status(),
        reqwest::StatusCode::ACCEPTED
    );
    let request = client
        .post(format!("{base}/mcp"))
        .bearer_auth(token)
        .header("Accept", "application/json, text/event-stream")
        .json(&serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}));
    let request = if let Some(session) = &session {
        request.header("MCP-Session-Id", session)
    } else {
        request
    };
    let response = request.send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = decode_rpc_response(response).await;
    assert!(
        body["result"]["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()),
        "{body}"
    );
}

async fn decode_rpc_response(response: reqwest::Response) -> serde_json::Value {
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let body = response.text().await.unwrap();
    if content_type.starts_with("application/json") {
        serde_json::from_str(&body).unwrap()
    } else {
        assert!(
            content_type.starts_with("text/event-stream"),
            "unexpected RPC content type: {content_type}"
        );
        let event = body
            .lines()
            .find_map(|line| line.strip_prefix("data:"))
            .expect("SSE RPC data event");
        serde_json::from_str(event.trim()).unwrap()
    }
}
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use jsonwebtoken::{decode, Algorithm, DecodingKey, Header, Validation};
use jsonwebtoken::{encode, EncodingKey};
use plasm_agent::http::{build_plasm_host_state, PlasmHostBootstrap};
use plasm_agent::incoming_auth::{IncomingAuthConfig, IncomingAuthMode, IncomingAuthVerifier};
use plasm_agent::mcp_api_key_registry::McpApiKeyRegistry;
use plasm_agent::mcp_config_repository::McpConfigRepository;
use plasm_agent::mcp_runtime_config::McpRuntimeConfig;
use plasm_agent::mcp_server::{build_mcp_router_for_transport, McpHttpTransport};
use plasm_agent::mcp_transport_auth::McpTransportAuth;
use plasm_agent::oauth_link_catalog::OauthLinkCatalog;
use plasm_agent::outbound_secret_provider::AgentOutboundSecretProvider;
use plasm_agent::server_state::CatalogBootstrap;
use plasm_agent::server_state::PlasmSaaSHostExtension;
use plasm_agent_core::secret_store::MemorySecretStore;
use plasm_core::discovery::CgsRegistry;
use plasm_core::loader::load_schema;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode, SecretProvider};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[path = "../../../plasm-agent-core/tests/support/postgres.rs"]
mod integration_postgres;

use integration_postgres::{integration_postgres_url, PostgresKeepAlive};

const TEST_JWT_SECRET: &str = "inbound-oauth-test-secret-012345678901234567890123";

#[allow(dead_code)]
pub(crate) struct ContainerDrop {
    _database: PostgresKeepAlive,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

pub(crate) async fn oauth_test_postgres_url() -> Option<(Option<ContainerDrop>, String)> {
    const START_TIMEOUT: Duration = Duration::from_secs(45);
    static LOCK: std::sync::OnceLock<Arc<tokio::sync::Mutex<()>>> = std::sync::OnceLock::new();
    let guard = LOCK
        .get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
        .lock_owned()
        .await;
    integration_postgres_url(START_TIMEOUT)
        .await
        .map(|(k, url)| {
            (
                Some(ContainerDrop {
                    _database: k,
                    _guard: guard,
                }),
                url,
            )
        })
}

fn dnd5e_registry() -> Arc<CgsRegistry> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/dnd5e");
    let cgs = Arc::new(load_schema(&dir).expect("dnd5e schema"));
    Arc::new(CgsRegistry::from_pairs(vec![(
        "dnd5e".into(),
        "D&D 5e".into(),
        vec!["demo".into()],
        cgs.clone(),
    )]))
}

#[derive(Serialize)]
struct PrincipalClaims<'a> {
    sub: &'a str,
    tenant_id: &'a str,
    exp: u64,
}

pub(crate) fn mint_principal_jwt(subject: &str, tenant_id: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = PrincipalClaims {
        sub: subject,
        tenant_id,
        exp: now + 3600,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .expect("mint principal jwt")
}

pub(crate) fn decode_oauth_access_token_claims(
    token: &str,
    resource: &str,
) -> (String, Vec<String>) {
    #[derive(Debug, Deserialize)]
    struct AccessClaims {
        iss: String,
        aud: serde_json::Value,
    }

    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[resource]);
    validation.set_issuer(&[resource]);
    let data = decode::<AccessClaims>(
        token,
        &DecodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
        &validation,
    )
    .expect("decode oauth access token");
    let aud = match &data.claims.aud {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => panic!("unexpected aud claim shape"),
    };
    (data.claims.iss, aud)
}

pub(crate) fn base64url_sha256(input: &str) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let digest = Sha256::digest(input.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

pub(crate) async fn spawn_mcp_server(
) -> Option<(String, tokio::task::JoinHandle<()>, Option<ContainerDrop>)> {
    spawn_mcp_server_with_transport(McpHttpTransport::StreamableHttp).await
}

pub(crate) async fn spawn_mcp_server_with_transport(
    transport: McpHttpTransport,
) -> Option<(String, tokio::task::JoinHandle<()>, Option<ContainerDrop>)> {
    let (keep, url) = oauth_test_postgres_url().await?;
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    let incoming = IncomingAuthVerifier::new(IncomingAuthConfig {
        mode: IncomingAuthMode::Optional,
        jwt_secret: Some(TEST_JWT_SECRET.to_string()),
        jwt_issuer: None,
        jwt_audience: None,
        api_keys_file: None,
    })
    .expect("incoming auth verifier");
    let mut st = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: dnd5e_registry(),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: Some(Arc::new(incoming)),
        run_artifacts: Arc::new(plasm_agent::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
    .expect("valid catalog fixture");
    let storage = Arc::new(MemorySecretStore::new());
    let catalog = Arc::new(OauthLinkCatalog::default());
    let outbound = Arc::new(AgentOutboundSecretProvider::new(
        storage.clone(),
        catalog.clone(),
    )) as Arc<dyn SecretProvider>;
    st.oss.auth_storage = Some(storage.clone());
    st.oss.oauth_link_catalog = Some(catalog);
    st.oss.outbound_secret_provider = Some(outbound);
    let mut saas = PlasmSaaSHostExtension {
        mcp_config_repository: None,
        mcp_transport_auth: Some(
            Arc::new(McpApiKeyRegistry::new(storage.clone())) as Arc<dyn McpTransportAuth>
        ),
        tenant_binding: None,
        flow_policy_repository: None,
    };

    let repo = McpConfigRepository::connect_and_migrate(&url).await.ok()?;
    let cfg_id = Uuid::new_v4();
    let runtime = McpRuntimeConfig {
        id: cfg_id,
        tenant_id: "tenant-a".to_string(),
        workspace_slug: "default".to_string(),
        project_slug: "default".to_string(),
        space_type: "personal".to_string(),
        owner_subject: Some("user-a".to_string()),
        version: 1,
        endpoint_secret_hash: [7u8; 32],
        credential_secret_hashes: HashSet::new(),
        allowed_entry_ids: HashSet::new(),
        capabilities_by_entry: HashMap::new(),
        auth_config_by_entry: HashMap::new(),
    };
    repo.upsert_full(runtime, "default", "default", "personal MCP", "active", &[])
        .await
        .ok()?;
    saas.mcp_config_repository = Some(Arc::new(repo));
    st.saas = Some(saas);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();

    std::env::set_var(
        "PLASM_MCP_PUBLIC_BASE_URL",
        format!("http://127.0.0.1:{port}"),
    );
    let st = Arc::new(st);
    let router = build_mcp_router_for_transport(st, transport)
        .await
        .expect("MCP router");
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("run MCP server");
    });
    tokio::time::sleep(Duration::from_millis(120)).await;
    Some((format!("http://127.0.0.1:{port}"), handle, keep))
}

pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client")
}

pub(crate) async fn register_dynamic_client(
    client: &reqwest::Client,
    base: &str,
    grant_types: &[&str],
) -> String {
    let reg = client
        .post(format!("{base}/mcp/oauth/register"))
        .json(&serde_json::json!({
          "redirect_uris": ["https://example.com/callback"],
          "token_endpoint_auth_method": "none",
          "grant_types": grant_types,
          "response_types": ["code"]
        }))
        .send()
        .await
        .expect("register request");
    assert_eq!(reg.status(), reqwest::StatusCode::CREATED);
    let reg_body: serde_json::Value = reg.json().await.expect("register body");
    reg_body["client_id"]
        .as_str()
        .expect("client_id")
        .to_string()
}
