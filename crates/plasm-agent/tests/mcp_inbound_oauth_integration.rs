use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use auth_framework::storage::MemoryStorage;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use plasm_agent::http::{build_plasm_host_state, PlasmHostBootstrap};
use plasm_agent::incoming_auth::{IncomingAuthConfig, IncomingAuthMode, IncomingAuthVerifier};
use plasm_agent::mcp_api_key_registry::McpApiKeyRegistry;
use plasm_agent::mcp_config_repository::McpConfigRepository;
use plasm_agent::mcp_runtime_config::McpRuntimeConfig;
use plasm_agent::mcp_server::run_mcp_server;
use plasm_agent::mcp_transport_auth::McpTransportAuth;
use plasm_agent::oauth_link_catalog::OauthLinkCatalog;
use plasm_agent::outbound_secret_provider::AgentOutboundSecretProvider;
use plasm_agent::server_state::CatalogBootstrap;
use plasm_agent::server_state::PlasmSaaSHostExtension;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode, SecretProvider};
use serde::Serialize;
use sha2::{Digest, Sha256};
use testcontainers_modules::{
    postgres::Postgres,
    testcontainers::{runners::AsyncRunner, ContainerAsync},
};
use uuid::Uuid;

const TEST_JWT_SECRET: &str = "inbound-oauth-test-secret-012345678901234567890123";

#[allow(dead_code)]
struct ContainerDrop(ContainerAsync<Postgres>);

async fn oauth_test_postgres_url() -> Option<(Option<ContainerDrop>, String)> {
    if let Ok(url) = std::env::var("PLASM_MCP_CONFIG_TEST_DATABASE_URL") {
        let url = url.trim().to_string();
        if !url.is_empty() {
            return Some((None, url));
        }
    }
    const START_TIMEOUT: Duration = Duration::from_secs(45);
    let node = match tokio::time::timeout(START_TIMEOUT, Postgres::default().start()).await {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => {
            eprintln!("skip inbound_oauth tests: postgres failed ({e})");
            return None;
        }
        Err(_) => {
            eprintln!("skip inbound_oauth tests: postgres start timeout");
            return None;
        }
    };
    let port = node.get_host_port_ipv4(5432).await.ok()?;
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    Some((Some(ContainerDrop(node)), url))
}

fn dnd5e_registry() -> Arc<InMemoryCgsRegistry> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/dnd5e");
    let cgs = Arc::new(load_schema(&dir).expect("dnd5e schema"));
    Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
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

fn mint_principal_jwt(subject: &str, tenant_id: &str) -> String {
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

fn base64url_sha256(input: &str) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let digest = Sha256::digest(input.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

async fn spawn_mcp_server() -> Option<(String, tokio::task::JoinHandle<()>, Option<ContainerDrop>)>
{
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
        plugin_manager: None,
        incoming_auth: Some(Arc::new(incoming)),
        run_artifacts: Arc::new(plasm_agent::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    });
    let storage = Arc::new(MemoryStorage::new());
    let catalog = Arc::new(OauthLinkCatalog::default());
    let outbound = Arc::new(AgentOutboundSecretProvider::new(
        storage.clone(),
        catalog.clone(),
    )) as Arc<dyn SecretProvider>;
    st.oss.auth_storage = Some(storage.clone());
    st.oss.oauth_link_catalog = Some(catalog);
    st.oss.outbound_secret_provider = Some(outbound);
    let mut saas = PlasmSaaSHostExtension {
        auth_framework: None,
        mcp_config_repository: None,
        mcp_transport_auth: Some(
            Arc::new(McpApiKeyRegistry::new(storage.clone())) as Arc<dyn McpTransportAuth>
        ),
        tenant_binding: None,
    };

    let repo = McpConfigRepository::connect_and_migrate(&url).await.ok()?;
    let cfg_id = Uuid::new_v4();
    let runtime = McpRuntimeConfig {
        id: cfg_id,
        tenant_id: "tenant-a".to_string(),
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
    drop(listener);

    std::env::set_var(
        "PLASM_MCP_PUBLIC_BASE_URL",
        format!("http://127.0.0.1:{port}"),
    );
    let st = Arc::new(st);
    let handle = tokio::spawn(async move {
        run_mcp_server("127.0.0.1", port, st)
            .await
            .expect("run mcp server");
    });
    tokio::time::sleep(Duration::from_millis(120)).await;
    Some((format!("http://127.0.0.1:{port}"), handle, keep))
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client")
}

async fn register_dynamic_client(
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

#[tokio::test]
async fn inbound_oauth_dynamic_registration_pkce_and_transport_access() {
    let Some((base, handle, _keep)) = spawn_mcp_server().await else {
        return;
    };
    let client = http_client();

    let prm = client
        .get(format!(
            "{base}/mcp/.well-known/oauth-protected-resource/mcp"
        ))
        .send()
        .await
        .expect("protected resource metadata request");
    assert_eq!(prm.status(), reqwest::StatusCode::OK);
    let prm_body: serde_json::Value = prm.json().await.expect("protected resource metadata body");
    let resource = prm_body["resource"]
        .as_str()
        .expect("resource metadata URL");
    assert!(
        resource.ends_with("/mcp"),
        "protected resource metadata must advertise /mcp resource URL"
    );
    let authorization_server = prm_body["authorization_servers"]
        .as_array()
        .and_then(|v| v.first())
        .and_then(|v| v.as_str())
        .expect("authorization server metadata URL");
    assert!(
        authorization_server.ends_with("/mcp"),
        "authorization server metadata must advertise /mcp issuer URL"
    );

    let asm = client
        .get(format!("{base}/mcp/.well-known/oauth-authorization-server"))
        .send()
        .await
        .expect("authorization server metadata request");
    assert_eq!(asm.status(), reqwest::StatusCode::OK);
    let asm_body: serde_json::Value = asm
        .json()
        .await
        .expect("authorization server metadata body");
    let registration_endpoint = asm_body["registration_endpoint"]
        .as_str()
        .expect("registration endpoint URL");
    assert!(
        registration_endpoint.ends_with("/mcp/oauth/register"),
        "registration endpoint must be the canonical /mcp/oauth/register path"
    );

    let client_id =
        register_dynamic_client(&client, &base, &["authorization_code", "refresh_token"]).await;

    let verifier = "verifier-1234567890";
    let challenge = base64url_sha256(verifier);
    let principal = mint_principal_jwt("user-a", "tenant-a");
    let authz = client
        .get(format!("{base}/mcp/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            ("scope", "mcp:tools"),
            ("state", "s1"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("principal_token", principal.as_str()),
        ])
        .send()
        .await
        .expect("authorize request");
    assert_eq!(authz.status(), reqwest::StatusCode::FOUND);
    let location = authz
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("location header")
        .to_str()
        .expect("location string");
    let parsed = reqwest::Url::parse(location).expect("redirect URL");
    let code = parsed
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .expect("auth code");

    let token = client
        .post(format!("{base}/mcp/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("code", code.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .expect("token request");
    assert_eq!(token.status(), reqwest::StatusCode::OK);
    let token_body: serde_json::Value = token.json().await.expect("token body");
    let access_token = token_body["access_token"].as_str().expect("access_token");
    let refresh_token = token_body["refresh_token"].as_str().expect("refresh_token");

    // Auth passes if MCP does not return transport auth failures (content may still be invalid).
    let mcp = client
        .post(format!("{base}/mcp"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {access_token}"),
        )
        .body("{}")
        .send()
        .await
        .expect("mcp request");
    assert_ne!(mcp.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_ne!(mcp.status(), reqwest::StatusCode::FORBIDDEN);

    let refreshed = client
        .post(format!("{base}/mcp/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id.as_str()),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .expect("refresh token request");
    assert_eq!(refreshed.status(), reqwest::StatusCode::OK);
    let refreshed_body: serde_json::Value = refreshed.json().await.expect("refreshed body");
    let refreshed_access_token = refreshed_body["access_token"]
        .as_str()
        .expect("refreshed access token");
    let rotated_refresh_token = refreshed_body["refresh_token"]
        .as_str()
        .expect("rotated refresh token");
    assert_ne!(
        rotated_refresh_token, refresh_token,
        "refresh token must rotate after successful refresh grant"
    );

    let replay = client
        .post(format!("{base}/mcp/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id.as_str()),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .expect("refresh replay request");
    assert_eq!(replay.status(), reqwest::StatusCode::BAD_REQUEST);
    let replay_body: serde_json::Value = replay.json().await.expect("refresh replay error body");
    assert_eq!(replay_body["error"].as_str(), Some("invalid_grant"));

    let mismatch_client_id =
        register_dynamic_client(&client, &base, &["authorization_code", "refresh_token"]).await;
    let mismatch = client
        .post(format!("{base}/mcp/oauth/token"))
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": mismatch_client_id,
            "refresh_token": rotated_refresh_token
        }))
        .send()
        .await
        .expect("refresh mismatch request");
    assert_eq!(mismatch.status(), reqwest::StatusCode::BAD_REQUEST);
    let mismatch_body: serde_json::Value = mismatch.json().await.expect("refresh mismatch body");
    assert_eq!(mismatch_body["error"].as_str(), Some("invalid_grant"));

    let mcp_refreshed = client
        .post(format!("{base}/mcp"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {refreshed_access_token}"),
        )
        .body("{}")
        .send()
        .await
        .expect("mcp request with refreshed access token");
    assert_ne!(mcp_refreshed.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_ne!(mcp_refreshed.status(), reqwest::StatusCode::FORBIDDEN);

    handle.abort();
}

#[tokio::test]
async fn inbound_oauth_rejects_missing_pkce_and_subject_mismatch() {
    let Some((base, handle, _keep)) = spawn_mcp_server().await else {
        return;
    };
    let client = http_client();

    let client_id = register_dynamic_client(&client, &base, &["authorization_code"]).await;

    let no_pkce = client
        .get(format!("{base}/mcp/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            (
                "principal_token",
                mint_principal_jwt("user-a", "tenant-a").as_str(),
            ),
        ])
        .send()
        .await
        .expect("authorize no pkce");
    assert_eq!(no_pkce.status(), reqwest::StatusCode::BAD_REQUEST);

    let mismatch = client
        .get(format!("{base}/mcp/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            ("code_challenge", base64url_sha256("x").as_str()),
            ("code_challenge_method", "S256"),
            (
                "principal_token",
                mint_principal_jwt("user-other", "tenant-a").as_str(),
            ),
        ])
        .send()
        .await
        .expect("authorize mismatch");
    assert_eq!(mismatch.status(), reqwest::StatusCode::FORBIDDEN);

    let bad_subject_token = mint_principal_jwt("user-other", "tenant-a");
    let mcp = client
        .post(format!("{base}/mcp"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {bad_subject_token}"),
        )
        .body("{}")
        .send()
        .await
        .expect("mcp request");
    assert_eq!(mcp.status(), reqwest::StatusCode::UNAUTHORIZED);

    let unauthorized_refresh = client
        .post(format!("{base}/mcp/oauth/token"))
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": client_id,
            "refresh_token": "plasm_rtok_not_real"
        }))
        .send()
        .await
        .expect("unauthorized refresh");
    assert_eq!(
        unauthorized_refresh.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let unauthorized_refresh_body: serde_json::Value = unauthorized_refresh
        .json()
        .await
        .expect("unauthorized refresh body");
    assert_eq!(
        unauthorized_refresh_body["error"].as_str(),
        Some("unauthorized_client")
    );

    handle.abort();
}
