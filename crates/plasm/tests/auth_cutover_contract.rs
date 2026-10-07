#[path = "support/mcp_oauth.rs"]
mod support;
use plasm_agent::mcp_server::McpHttpTransport;
use support::*;
use uuid::Uuid;

struct ReviewFlow {
    base: String,
    server: tokio::task::JoinHandle<()>,
    _database: Option<ContainerDrop>,
    client: reqwest::Client,
    client_id: String,
    resource: String,
    verifier: String,
    code: String,
}

impl Drop for ReviewFlow {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn review_flow(resource_suffix: &str) -> ReviewFlow {
    flow_with_transport(resource_suffix, McpHttpTransport::StreamableHttp).await
}

async fn flow_with_transport(resource_suffix: &str, transport: McpHttpTransport) -> ReviewFlow {
    let (base, server, database) = spawn_mcp_server_with_transport(transport)
        .await
        .expect("review probes require a real PostgreSQL database");
    let client = http_client();
    let metadata: serde_json::Value = client
        .get(format!(
            "{base}/mcp/.well-known/oauth-protected-resource/mcp"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let resource = metadata["resource"].as_str().unwrap().to_owned();
    let client_id =
        register_dynamic_client(&client, &base, &["authorization_code", "refresh_token"]).await;
    let verifier = "review-verifier-0123456789012345678901234567890123456789".to_owned();
    let challenge = base64url_sha256(&verifier);
    let principal = mint_principal_jwt("user-a", "tenant-a");
    let requested_resource = format!("{resource}{resource_suffix}");
    let response = client
        .post(format!("{base}/mcp/oauth/authorize"))
        .form(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            ("scope", "mcp:tools"),
            ("state", "review-state"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", requested_resource.as_str()),
            ("principal_token", principal.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    let redirect = reqwest::Url::parse(
        response.headers()[reqwest::header::LOCATION]
            .to_str()
            .unwrap(),
    )
    .unwrap();
    let code = redirect
        .query_pairs()
        .find(|(key, _)| key == "code")
        .unwrap()
        .1
        .into_owned();
    ReviewFlow {
        base,
        server,
        _database: database,
        client,
        client_id,
        resource,
        verifier,
        code,
    }
}

async fn review_initial_refresh(flow: &ReviewFlow) -> String {
    let response = flow
        .client
        .post(format!("{}/mcp/oauth/token", flow.base))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", flow.client_id.as_str()),
            ("code", flow.code.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            ("code_verifier", flow.verifier.as_str()),
            ("resource", flow.resource.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    body["refresh_token"].as_str().unwrap().to_owned()
}

async fn review_refresh(flow: &ReviewFlow, token: &str) -> reqwest::Response {
    flow.client
        .post(format!("{}/mcp/oauth/token", flow.base))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", flow.client_id.as_str()),
            ("refresh_token", token),
            ("resource", flow.resource.as_str()),
        ])
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn review_refresh_replay_revokes_the_active_successor() {
    let flow = review_flow("").await;
    let original = review_initial_refresh(&flow).await;
    let rotated = review_refresh(&flow, &original).await;
    assert_eq!(rotated.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = rotated.json().await.unwrap();
    let successor = body["refresh_token"].as_str().unwrap();
    let replay = review_refresh(&flow, &original).await;
    assert_eq!(replay.status(), reqwest::StatusCode::BAD_REQUEST);
    let after_replay = review_refresh(&flow, successor).await;
    assert_eq!(after_replay.status(), reqwest::StatusCode::BAD_REQUEST,
        "a replayed consumed refresh token must invalidate its active family, not leave the successor usable");
}

#[tokio::test]
async fn review_duplicate_code_verifier_is_rejected() {
    let flow = review_flow("").await;
    let response = flow
        .client
        .post(format!("{}/mcp/oauth/token", flow.base))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", flow.client_id.as_str()),
            ("code", flow.code.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            (
                "code_verifier",
                "incorrect-verifier-0123456789012345678901234567890123456789",
            ),
            ("code_verifier", flow.verifier.as_str()),
            ("resource", flow.resource.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST,
        "duplicate OAuth code_verifier parameters must be rejected rather than selecting the last value");
}

#[tokio::test]
async fn review_fragmented_resource_is_rejected_before_issuance() {
    let flow = review_flow("").await;
    let challenge = base64url_sha256(&flow.verifier);
    let principal = mint_principal_jwt("user-a", "tenant-a");
    let resource = format!("{}#forbidden-fragment", flow.resource);
    let response = flow
        .client
        .post(format!("{}/mcp/oauth/authorize", flow.base))
        .form(&[
            ("response_type", "code"),
            ("client_id", flow.client_id.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            ("scope", "mcp:tools"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", resource.as_str()),
            ("principal_token", principal.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    let redirect = reqwest::Url::parse(
        response.headers()[reqwest::header::LOCATION]
            .to_str()
            .unwrap(),
    )
    .unwrap();
    let query: std::collections::HashMap<_, _> = redirect.query_pairs().collect();
    assert_eq!(
        query.get("error").map(|value| value.as_ref()),
        Some("invalid_target")
    );
    assert!(!query.contains_key("code"));
    assert_eq!(
        query.get("iss").map(|value| value.as_ref()),
        Some(flow.resource.as_str())
    );
}

#[tokio::test]
async fn review_stateless_metadata_discovery_is_mounted() {
    let (base, server, _database) = spawn_mcp_server_with_transport(McpHttpTransport::Stateless)
        .await
        .unwrap();
    let response = http_client()
        .get(format!(
            "{base}/mcp/.well-known/oauth-protected-resource/mcp"
        ))
        .send()
        .await
        .unwrap();
    server.abort();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "the stateless MCP router must expose protected-resource metadata"
    );
}

#[tokio::test]
async fn review_stateless_unauthenticated_requests_include_challenge() {
    let (base, server, _database) = spawn_mcp_server_with_transport(McpHttpTransport::Stateless)
        .await
        .unwrap();
    let response = http_client()
        .post(format!("{base}/mcp"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": { "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {"name": "review", "version": "1"},
                "io.modelcontextprotocol/clientCapabilities": {}
            }}
        }))
        .send()
        .await
        .unwrap();
    server.abort();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(
        response
            .headers()
            .contains_key(reqwest::header::WWW_AUTHENTICATE),
        "an unauthorized MCP request must receive a Bearer authentication challenge"
    );
}

#[tokio::test]
async fn review_postgres_bootstrap_reclaims_expired_kv() {
    let (_database, url) = oauth_test_postgres_url()
        .await
        .expect("real PostgreSQL test database");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query("CREATE TABLE IF NOT EXISTS kv_store (key VARCHAR(255) PRIMARY KEY, value BYTEA NOT NULL, expires_at TIMESTAMPTZ, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW())")
        .execute(&pool).await.unwrap();
    let key = format!("review-expired:{}", Uuid::new_v4());
    sqlx::query(
        "INSERT INTO kv_store (key, value, expires_at) VALUES ($1, $2, NOW() - INTERVAL '1 day')",
    )
    .bind(&key)
    .bind(b"expired".as_slice())
    .execute(&pool)
    .await
    .unwrap();
    let live_key = format!("{key}:live");
    let permanent_key = format!("{key}:permanent");
    sqlx::query("INSERT INTO kv_store (key, value, expires_at) VALUES ($1, $3, NOW() + INTERVAL '1 day'), ($2, $3, NULL)")
        .bind(&live_key).bind(&permanent_key).bind(b"survivor".as_slice()).execute(&pool).await.unwrap();
    let store = plasm_agent_core::secret_store_host::init_postgres_secret_store(
        pool.clone(),
        plasm_agent_core::secret_store::SecretEncryption::from_key(&[7; 32]).unwrap(),
    )
    .await
    .unwrap();
    assert!(store.get_kv(&key).await.unwrap().is_none());
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM kv_store WHERE key = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    let survivors: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM kv_store WHERE key = $1 OR key = $2")
            .bind(&live_key)
            .bind(&permanent_key)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        survivors, 2,
        "expiry cleanup must retain live and permanent records"
    );
    let expired_a = format!("{key}:expired-a");
    let expired_b = format!("{key}:expired-b");
    sqlx::query("INSERT INTO kv_store (key, value, expires_at) VALUES ($1, $3, NOW() - INTERVAL '2 days'), ($2, $3, NOW() - INTERVAL '2 days')")
        .bind(&expired_a).bind(&expired_b).bind(b"expired".as_slice()).execute(&pool).await.unwrap();
    assert_eq!(store.purge_expired(1).await.unwrap(), 1);
    let pending: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM kv_store WHERE key = $1 OR key = $2")
            .bind(&expired_a)
            .bind(&expired_b)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pending, 1, "expiry sweeping must obey its batch bound");
    assert_eq!(store.purge_expired(1).await.unwrap(), 1);
    sqlx::query("DELETE FROM kv_store WHERE key = $1 OR key = $2")
        .bind(live_key)
        .bind(permanent_key)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM kv_store WHERE key = $1")
        .bind(&key)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        remaining, 0,
        "bootstrap must reclaim expired durable rows as the previous framework initializer did"
    );
}

#[tokio::test]
async fn stateless_oauth_exchange_lists_tools_and_replay_revokes_access() {
    let flow = flow_with_transport("", McpHttpTransport::Stateless).await;
    let original = review_initial_refresh(&flow).await;
    let rotated: serde_json::Value = review_refresh(&flow, &original).await.json().await.unwrap();
    let access = rotated["access_token"].as_str().unwrap();
    let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"auth-contract","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}});
    let response = flow
        .client
        .post(format!("{}/mcp", flow.base))
        .bearer_auth(access)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let tools: serde_json::Value = response.json().await.unwrap();
    assert!(
        tools["result"]["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()),
        "{tools}"
    );
    assert_eq!(
        review_refresh(&flow, &original).await.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let denied = flow
        .client
        .post(format!("{}/mcp", flow.base))
        .bearer_auth(access)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(denied.headers()[reqwest::header::WWW_AUTHENTICATE]
        .to_str()
        .unwrap()
        .contains("invalid_token"));
}

#[tokio::test]
async fn validated_authorization_errors_preserve_state_and_issuer_but_invalid_redirect_is_local() {
    let flow = review_flow("").await;
    let challenge = base64url_sha256(&flow.verifier);
    let principal = mint_principal_jwt("user-a", "tenant-a");
    let missing_method = flow
        .client
        .post(format!("{}/mcp/oauth/authorize", flow.base))
        .form(&[
            ("client_id", flow.client_id.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            ("response_type", "code"),
            ("code_challenge", challenge.as_str()),
            ("resource", flow.resource.as_str()),
            ("principal_token", principal.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_callback_error(missing_method, "invalid_request");
    let response = flow
        .client
        .post(format!("{}/mcp/oauth/authorize", flow.base))
        .form(&[
            ("client_id", flow.client_id.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            ("state", "opaque + & state"),
            ("response_type", "token"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    let location = reqwest::Url::parse(
        response.headers()[reqwest::header::LOCATION]
            .to_str()
            .unwrap(),
    )
    .unwrap();
    let params: std::collections::HashMap<_, _> = location.query_pairs().collect();
    assert_eq!(params.get("state").unwrap(), "opaque + & state");
    assert_eq!(params.get("iss").unwrap(), flow.resource.as_str());
    assert!(!params.contains_key("code"));
    let response = flow
        .client
        .post(format!("{}/mcp/oauth/authorize", flow.base))
        .form(&[
            ("client_id", flow.client_id.as_str()),
            ("redirect_uri", "https://evil.example/callback"),
            ("response_type", "code"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(!response.headers().contains_key(reqwest::header::LOCATION));
}

#[tokio::test]
async fn followup_single_registered_redirect_can_be_omitted() {
    let flow = review_flow("").await;
    let principal = mint_principal_jwt("user-a", "tenant-a");
    let challenge = base64url_sha256(&flow.verifier);
    let response = flow
        .client
        .post(format!("{}/mcp/oauth/authorize", flow.base))
        .form(&[
            ("response_type", "code"),
            ("client_id", flow.client_id.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", flow.resource.as_str()),
            ("principal_token", principal.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::FOUND,
        "the sole registered redirect URI is optional in an authorization request"
    );
    let callback = reqwest::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
    assert_eq!(
        callback.as_str().split('?').next().unwrap(),
        "https://example.com/callback"
    );
    let code = callback
        .query_pairs()
        .find(|(key, _)| key == "code")
        .unwrap()
        .1
        .into_owned();
    let response = flow
        .client
        .post(format!("{}/mcp/oauth/token", flow.base))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", flow.client_id.as_str()),
            ("code", code.as_str()),
            ("code_verifier", flow.verifier.as_str()),
            ("resource", flow.resource.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn followup_duplicate_authorization_scope_returns_validated_callback_error() {
    let flow = review_flow("").await;
    let challenge = base64url_sha256(&flow.verifier);
    let principal = mint_principal_jwt("user-a", "tenant-a");
    let response = flow
        .client
        .post(format!("{}/mcp/oauth/authorize", flow.base))
        .form(&[
            ("response_type", "code"),
            ("client_id", flow.client_id.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            ("scope", "mcp:tools"),
            ("scope", "mcp:tools"),
            ("state", "retained-state"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", flow.resource.as_str()),
            ("principal_token", principal.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::FOUND,
        "unique validated client and redirect require a callback error, including state and issuer"
    );
    let location = response.headers()["location"].to_str().unwrap();
    let callback = reqwest::Url::parse(location).unwrap();
    let params: std::collections::HashMap<_, _> = callback.query_pairs().into_owned().collect();
    assert_eq!(params["error"], "invalid_request");
    assert_eq!(params["state"], "retained-state");
    assert_eq!(params["iss"], flow.resource);
    assert!(!params.contains_key("code"));
}

#[tokio::test]
async fn followup_empty_refresh_scope_is_treated_as_omitted() {
    let flow = review_flow("").await;
    let refresh = review_initial_refresh(&flow).await;
    let response = flow
        .client
        .post(format!("{}/mcp/oauth/token", flow.base))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", flow.client_id.as_str()),
            ("refresh_token", refresh.as_str()),
            ("resource", flow.resource.as_str()),
            ("scope", ""),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "empty optional scope must behave like omitted scope"
    );
    let token: serde_json::Value = response.json().await.unwrap();
    assert_eq!(token["scope"], "mcp:tools");
    let response = flow
        .client
        .post(format!("{}/mcp/oauth/token", flow.base))
        .json(
            &serde_json::json!({"grant_type":"refresh_token", "client_id":flow.client_id,
            "refresh_token":token["refresh_token"], "resource":flow.resource, "scope":""}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["scope"],
        "mcp:tools"
    );
}

#[tokio::test]
async fn followup_native_loopback_redirect_allows_a_different_ephemeral_port() {
    let flow = review_flow("").await;
    let registered: serde_json::Value = flow.client.post(format!("{}/mcp/oauth/register", flow.base)).json(&serde_json::json!({"redirect_uris":["http://127.0.0.1:1234/callback"],"application_type":"native","token_endpoint_auth_method":"none","grant_types":["authorization_code"],"response_types":["code"]})).send().await.unwrap().json().await.unwrap();
    let client_id = registered["client_id"].as_str().unwrap();
    let principal = mint_principal_jwt("user-a", "tenant-a");
    let challenge = base64url_sha256(&flow.verifier);
    let response = flow
        .client
        .post(format!("{}/mcp/oauth/authorize", flow.base))
        .form(&[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", "http://127.0.0.1:4567/callback"),
            ("scope", "mcp:tools"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", flow.resource.as_str()),
            ("principal_token", principal.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::FOUND,
        "native loopback redirects must allow ephemeral port changes"
    );
    let callback = reqwest::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
    let code = callback
        .query_pairs()
        .find(|(key, _)| key == "code")
        .unwrap()
        .1
        .into_owned();
    for (redirect, expected) in [
        (
            "http://127.0.0.1:1234/callback",
            reqwest::StatusCode::BAD_REQUEST,
        ),
        ("http://127.0.0.1:4567/callback", reqwest::StatusCode::OK),
    ] {
        let response = flow
            .client
            .post(format!("{}/mcp/oauth/token", flow.base))
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id),
                ("code", code.as_str()),
                ("redirect_uri", redirect),
                ("code_verifier", flow.verifier.as_str()),
                ("resource", flow.resource.as_str()),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
}

#[tokio::test]
async fn authorization_context_boundaries_hold_on_both_transports() {
    for transport in [
        McpHttpTransport::StreamableHttp,
        McpHttpTransport::Stateless,
    ] {
        let flow = flow_with_transport("", transport).await;
        let challenge = base64url_sha256(&flow.verifier);
        let principal = mint_principal_jwt("user-a", "tenant-a");
        let base = vec![
            ("response_type", "code"),
            ("client_id", flow.client_id.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", flow.resource.as_str()),
            ("principal_token", principal.as_str()),
        ];
        for duplicate in [
            ("client_id", flow.client_id.as_str()),
            ("redirect_uri", "https://example.com/callback"),
        ] {
            let mut params = base.clone();
            params.push(duplicate);
            let response = flow
                .client
                .post(format!("{}/mcp/oauth/authorize", flow.base))
                .form(&params)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
            assert!(!response.headers().contains_key("location"));
        }
        let mut params = base.clone();
        params.extend([("state", "first"), ("state", "second")]);
        let response = flow
            .client
            .post(format!("{}/mcp/oauth/authorize", flow.base))
            .form(&params)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        let callback =
            reqwest::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
        let params: std::collections::HashMap<_, _> = callback.query_pairs().into_owned().collect();
        assert_eq!(params["error"], "invalid_request");
        assert!(!params.contains_key("state"));
        assert_eq!(params["iss"], flow.resource);
        let mut params = base.clone();
        params.push(("scope", ""));
        let response = flow
            .client
            .post(format!("{}/mcp/oauth/authorize", flow.base))
            .form(&params)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        let callback =
            reqwest::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
        assert!(callback.query_pairs().any(|(key, _)| key == "code"));

        let registered: serde_json::Value = flow.client.post(format!("{}/mcp/oauth/register", flow.base))
            .json(&serde_json::json!({"redirect_uris":["https://example.com/one", "https://example.com/two"],
                "token_endpoint_auth_method":"none", "grant_types":["authorization_code"], "response_types":["code"]}))
            .send().await.unwrap().json().await.unwrap();
        let response = flow
            .client
            .get(format!("{}/mcp/oauth/authorize", flow.base))
            .query(&[
                ("client_id", registered["client_id"].as_str().unwrap()),
                ("response_type", "code"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        assert!(!response.headers().contains_key("location"));
        let response = flow
            .client
            .get(format!("{}/mcp/oauth/authorize", flow.base))
            .query(&[
                ("client_id", flow.client_id.as_str()),
                ("response_type", "code"),
                ("code_challenge", challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("resource", flow.resource.as_str()),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let consent = response.text().await.unwrap();
        assert!(consent.contains("name=\"redirect_uri\" value=\"https://example.com/callback\""));
    }
}
