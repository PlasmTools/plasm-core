#[path = "support/mcp_oauth.rs"]
mod support;
use support::*;

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
    assert_eq!(
        asm_body["authorization_response_iss_parameter_supported"],
        true
    );
    assert_eq!(asm_body["client_id_metadata_document_supported"], true);
    let registration_endpoint = asm_body["registration_endpoint"]
        .as_str()
        .expect("registration endpoint URL");
    assert!(
        registration_endpoint.ends_with("/mcp/oauth/register"),
        "registration endpoint must be the canonical /mcp/oauth/register path"
    );

    let oidc = client
        .get(format!("{base}/mcp/.well-known/openid-configuration"))
        .send()
        .await
        .expect("openid configuration metadata request");
    assert_eq!(oidc.status(), reqwest::StatusCode::OK);
    let oidc_body: serde_json::Value = oidc.json().await.expect("openid configuration body");
    assert_eq!(
        oidc_body["grant_types_supported"], asm_body["grant_types_supported"],
        "openid-configuration must match oauth-authorization-server grant_types"
    );
    assert_eq!(
        oidc_body["token_endpoint"], asm_body["token_endpoint"],
        "openid-configuration must match oauth-authorization-server token_endpoint"
    );

    let client_id =
        register_dynamic_client(&client, &base, &["authorization_code", "refresh_token"]).await;

    let verifier = "verifier-1234567890-01234567890123456789012345678901234567890123456789";
    let challenge = base64url_sha256(verifier);
    let principal = mint_principal_jwt("user-a", "tenant-a");
    let authz = client
        .post(format!("{base}/mcp/oauth/authorize"))
        .form(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            ("scope", "mcp:tools"),
            ("state", "s1"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", resource),
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
    assert_eq!(
        parsed
            .query_pairs()
            .find(|(key, _)| key == "iss")
            .map(|(_, value)| value.into_owned()),
        Some(resource.to_string())
    );
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
            ("resource", resource),
        ])
        .send()
        .await
        .expect("token request");
    assert_eq!(token.status(), reqwest::StatusCode::OK);
    let token_body: serde_json::Value = token.json().await.expect("token body");
    let access_token = token_body["access_token"].as_str().expect("access_token");
    let refresh_token = token_body["refresh_token"].as_str().expect("refresh_token");
    let (iss, aud) = decode_oauth_access_token_claims(access_token, resource);
    assert_eq!(iss, resource, "access token iss must be MCP resource URL");
    assert!(
        aud.iter().any(|a| a == resource),
        "access token aud must include MCP resource URL"
    );

    assert_streamable_tools(&client, &base, access_token).await;

    let refreshed = client
        .post(format!("{base}/mcp/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id.as_str()),
            ("refresh_token", refresh_token),
            ("resource", resource),
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
        .bearer_auth(refreshed_access_token)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":9,"method":"tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        mcp_refreshed.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "replay revokes issued access tokens too"
    );

    handle.abort();
}

#[tokio::test]
async fn inbound_oauth_rejects_wrong_resource_parameter() {
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
    let prm_body: serde_json::Value = prm.json().await.expect("protected resource metadata body");
    let resource = prm_body["resource"]
        .as_str()
        .expect("resource metadata URL");

    let client_id =
        register_dynamic_client(&client, &base, &["authorization_code", "refresh_token"]).await;
    let verifier =
        "verifier-resource-test-1234567890-01234567890123456789012345678901234567890123456789";
    let challenge = base64url_sha256(verifier);
    let principal = mint_principal_jwt("user-a", "tenant-a");
    let authz = client
        .post(format!("{base}/mcp/oauth/authorize"))
        .form(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", resource),
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
    let code = reqwest::Url::parse(location)
        .expect("redirect URL")
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .expect("auth code");

    let bad_resource = client
        .post(format!("{base}/mcp/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("code", code.as_str()),
            ("redirect_uri", "https://example.com/callback"),
            ("code_verifier", verifier),
            ("resource", "https://evil.example/mcp"),
        ])
        .send()
        .await
        .expect("token request with wrong resource");
    assert_eq!(bad_resource.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = bad_resource.json().await.expect("error body");
    assert_eq!(body["error"].as_str(), Some("invalid_target"));

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
        .post(format!("{base}/mcp/oauth/authorize"))
        .form(&[
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
    assert_callback_error(no_pkce, "invalid_request");

    let mismatch = client
        .post(format!("{base}/mcp/oauth/authorize"))
        .form(&[
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
    assert_callback_error(mismatch, "access_denied");

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
