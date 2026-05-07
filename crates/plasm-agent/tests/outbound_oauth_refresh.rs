//! Hosted KV OAuth envelope refresh (auth_config outbound) via [`AgentOutboundSecretProvider`].

use std::sync::Arc;

use auth_framework::storage::{AuthStorage, MemoryStorage};
use plasm_agent::oauth_link_catalog::{OauthLinkCatalog, RuntimeOauthProviderMeta};
use plasm_agent::outbound_secret_provider::AgentOutboundSecretProvider;
use plasm_core::AuthScheme;
use plasm_runtime::{AuthResolver, OutboundOAuthKvV1, SecretProvider, OUTBOUND_OAUTH_KV_VERSION};

#[tokio::test]
async fn hosted_bearer_refreshes_expired_envelope_and_rewrites_kv() {
    let storage = Arc::new(MemoryStorage::new()) as Arc<dyn AuthStorage>;
    let catalog = Arc::new(OauthLinkCatalog::default());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let token_url = format!("http://{addr}/oauth/token");

    catalog
        .upsert_runtime(
            "test_entry".to_string(),
            RuntimeOauthProviderMeta::try_new(
                "http://example.invalid/authorize",
                &token_url,
                vec![],
                "cid",
                "plasm:outbound:test:appsecret",
            )
            .expect("valid test meta"),
        )
        .await;

    storage
        .store_kv("plasm:outbound:test:appsecret", b"mysecret", None)
        .await
        .unwrap();

    let body = r#"{"access_token":"after_refresh","expires_in":3600,"token_type":"Bearer"}"#;
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 16384];
        let _n = stream.read(&mut buf).await.unwrap();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let kv_key = "plasm:outbound:v1:test-session";
    let env = OutboundOAuthKvV1 {
        version: OUTBOUND_OAUTH_KV_VERSION,
        entry_id: "test_entry".into(),
        access_token: "expired_access".into(),
        refresh_token: Some("rt1".into()),
        token_type: Some("Bearer".into()),
        expires_at_unix: Some(1),
        scope: None,
    };
    storage
        .store_kv(kv_key, &serde_json::to_vec(&env).unwrap(), None)
        .await
        .unwrap();

    let prov = AgentOutboundSecretProvider::new(storage.clone(), catalog);
    let scheme = AuthScheme::BearerToken {
        env: None,
        hosted_kv: Some(kv_key.to_string()),
        optional_env: false,
    };
    let resolver = AuthResolver::new(scheme, Arc::new(prov) as Arc<dyn SecretProvider>);
    let resolved = resolver.resolve().await.expect("resolve after refresh");
    let authz = &resolved.headers[0].1;
    assert!(
        authz.contains("after_refresh"),
        "expected refreshed token in header, got {authz:?}"
    );

    let bytes = storage.get_kv(kv_key).await.unwrap().expect("kv");
    let stored = String::from_utf8(bytes).expect("utf8");
    assert!(
        stored.contains("after_refresh"),
        "KV should hold updated envelope: {stored}"
    );
}
