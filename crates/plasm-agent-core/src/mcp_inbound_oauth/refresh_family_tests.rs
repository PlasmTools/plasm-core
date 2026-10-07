use super::*;
use crate::secret_store::{EncryptedSecretStore, MemorySecretStore, SecretEncryption, SecretStore};

async fn replay_contract(left: &dyn SecretStore, right: &dyn SecretStore) {
    let first = OAuthSessionStore::new(left);
    let other = OAuthSessionStore::new(right);
    let (id, wire) = first
        .mint_grant("client", "tenant", "subject", "https://resource/mcp", true)
        .await
        .unwrap();
    let wire = wire.unwrap();
    // Knowing the family identifier does not authorize revocation.
    let forged = format!("plasm_rtok_{id}.{}", "A".repeat(43));
    assert!(other
        .rotate_refresh_token(&forged, "client", "https://resource/mcp")
        .await
        .is_err());
    assert!(other
        .rotate_refresh_token(&wire, "wrong-client", "https://resource/mcp")
        .await
        .is_err());
    assert!(other
        .rotate_refresh_token(&wire, "client", "https://other/mcp")
        .await
        .is_err());
    first
        .validate_access_grant(&id, "client", "tenant", "subject", "https://resource/mcp")
        .await
        .unwrap();
    let rotated = other
        .rotate_refresh_token(&wire, "client", "https://resource/mcp")
        .await
        .unwrap();
    assert_eq!(rotated.grant_id, id);
    first
        .validate_access_grant(&id, "client", "tenant", "subject", "https://resource/mcp")
        .await
        .unwrap();
    assert!(first
        .rotate_refresh_token(&wire, "client", "https://resource/mcp")
        .await
        .is_err());
    assert!(other
        .rotate_refresh_token(&rotated.refresh_token, "client", "https://resource/mcp")
        .await
        .is_err());
    assert!(other
        .validate_access_grant(&id, "client", "tenant", "subject", "https://resource/mcp")
        .await
        .is_err());
}

#[tokio::test]
async fn encrypted_memory_family_replay_revokes_grant_without_forged_token_denial_of_service() {
    let store = EncryptedSecretStore::new(
        MemorySecretStore::new(),
        SecretEncryption::from_key(&[7; 32]).unwrap(),
    );
    replay_contract(&store, &store).await;
}

#[tokio::test]
async fn postgres_family_rotation_and_replay_cross_independent_hosts() {
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        return;
    };
    let left = crate::secret_store_host::init_postgres_secret_store(
        sqlx::PgPool::connect(&url).await.unwrap(),
        SecretEncryption::from_key(&[7; 32]).unwrap(),
    )
    .await
    .unwrap();
    let right = crate::secret_store_host::init_postgres_secret_store(
        sqlx::PgPool::connect(&url).await.unwrap(),
        SecretEncryption::from_key(&[7; 32]).unwrap(),
    )
    .await
    .unwrap();
    replay_contract(left.as_ref(), right.as_ref()).await;
    let sessions = OAuthSessionStore::new(left.as_ref());
    let (id, wire) = sessions
        .mint_grant("client", "tenant", "subject", "https://resource/mcp", true)
        .await
        .unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query("UPDATE kv_store SET expires_at = NOW() - INTERVAL '1 second' WHERE key = $1")
        .bind(grant_key(&id))
        .execute(&pool)
        .await
        .unwrap();
    assert!(OAuthSessionStore::new(right.as_ref())
        .rotate_refresh_token(&wire.unwrap(), "client", "https://resource/mcp")
        .await
        .is_err());
    assert!(sessions
        .validate_access_grant(&id, "client", "tenant", "subject", "https://resource/mcp")
        .await
        .is_err());
}

#[tokio::test]
async fn compare_exchange_keeps_absolute_expiry_and_has_one_winner() {
    let store = EncryptedSecretStore::new(
        MemorySecretStore::new(),
        SecretEncryption::from_key(&[7; 32]).unwrap(),
    );
    store
        .store_kv("record", b"old", Some(Duration::from_millis(100)))
        .await
        .unwrap();
    let (left, right) = tokio::join!(
        store.compare_exchange_kv("record", b"old", b"left"),
        store.compare_exchange_kv("record", b"old", b"right")
    );
    assert_eq!(usize::from(left.unwrap()) + usize::from(right.unwrap()), 1);
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(store.get_kv("record").await.unwrap().is_none());
}
