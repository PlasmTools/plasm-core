//! Plasm-owned expiring secret records and authenticated encryption.
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum SecretStoreError {
    #[error("invalid secret record input: {0}")]
    InvalidInput(String),
    #[error("secret record not found")]
    NotFound,
    #[error("secret-store backend unavailable")]
    BackendUnavailable,
    #[error("invalid secret-store configuration: {0}")]
    Configuration(&'static str),
    #[error("secret record serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("secret record base64 decoding failed")]
    Base64(#[from] base64::DecodeError),
    #[error("operating system entropy unavailable: {0}")]
    Entropy(getrandom::Error),
    #[error("secret record authentication failed")]
    Authentication,
    #[error("secret-store database operation failed")]
    Database(#[from] sqlx::Error),
}

/// A 256-bit opaque credential from the operating system randomness source.
pub fn random_token() -> Result<String, SecretStoreError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(SecretStoreError::Entropy)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    fn backend_label(&self) -> &'static str;
    async fn store_kv(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<(), SecretStoreError>;
    async fn get_kv(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError>;
    async fn delete_kv(&self, key: &str) -> Result<(), SecretStoreError>;
    /// Atomically consume a live record. Only one competing caller can obtain it.
    async fn take_kv(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError>;
    /// Consume only the live record whose bytes the caller validated.
    async fn consume_kv(&self, key: &str, expected: &[u8]) -> Result<bool, SecretStoreError>;
    /// Replace only a live record whose validated bytes still match, preserving its expiry.
    async fn compare_exchange_kv(
        &self,
        key: &str,
        expected: &[u8],
        replacement: &[u8],
    ) -> Result<bool, SecretStoreError>;
    /// Physically reclaim at most `limit` expired records.
    async fn purge_expired(&self, limit: u32) -> Result<u64, SecretStoreError>;
}

struct MemoryRecord {
    value: Vec<u8>,
    expires: Option<Instant>,
}
#[derive(Default)]
pub struct MemorySecretStore {
    records: Mutex<HashMap<String, MemoryRecord>>,
}
impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}
#[async_trait]
impl SecretStore for MemorySecretStore {
    fn backend_label(&self) -> &'static str {
        "memory"
    }
    async fn compare_exchange_kv(
        &self,
        key: &str,
        expected: &[u8],
        replacement: &[u8],
    ) -> Result<bool, SecretStoreError> {
        let mut records = self.records.lock().await;
        let Some(row) = records.get_mut(key).filter(|row| {
            row.value == expected && row.expires.is_none_or(|expiry| expiry > Instant::now())
        }) else {
            return Ok(false);
        };
        row.value = replacement.into();
        Ok(true)
    }
    async fn purge_expired(&self, limit: u32) -> Result<u64, SecretStoreError> {
        let mut records = self.records.lock().await;
        let now = Instant::now();
        let keys: Vec<String> = records
            .iter()
            .filter(|(_, row)| row.expires.is_some_and(|expiry| expiry <= now))
            .take(limit as usize)
            .map(|(key, _)| key.clone())
            .collect();
        let count = keys.len() as u64;
        for key in keys {
            records.remove(&key);
        }
        Ok(count)
    }
    async fn consume_kv(&self, key: &str, expected: &[u8]) -> Result<bool, SecretStoreError> {
        let mut records = self.records.lock().await;
        let matches = records.get(key).is_some_and(|row| {
            row.value == expected && row.expires.is_none_or(|expiry| expiry > Instant::now())
        });
        if matches {
            records.remove(key);
        }
        Ok(matches)
    }
    async fn store_kv(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<(), SecretStoreError> {
        let expires = ttl
            .map(|ttl| {
                Instant::now()
                    .checked_add(ttl)
                    .ok_or(SecretStoreError::Configuration(
                        "secret expiry exceeds supported range",
                    ))
            })
            .transpose()?;
        let mut records = self.records.lock().await;
        let now = Instant::now();
        records.retain(|_, row| row.expires.is_none_or(|expiry| expiry > now));
        records.insert(
            key.into(),
            MemoryRecord {
                value: value.into(),
                expires,
            },
        );
        Ok(())
    }
    async fn get_kv(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        let mut records = self.records.lock().await;
        if records
            .get(key)
            .is_some_and(|row| row.expires.is_some_and(|expiry| expiry <= Instant::now()))
        {
            records.remove(key);
        }
        Ok(records.get(key).map(|row| row.value.clone()))
    }
    async fn delete_kv(&self, key: &str) -> Result<(), SecretStoreError> {
        self.records.lock().await.remove(key);
        Ok(())
    }
    async fn take_kv(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        Ok(self
            .records
            .lock()
            .await
            .remove(key)
            .filter(|row| row.expires.is_none_or(|expiry| expiry > Instant::now()))
            .map(|row| row.value))
    }
}

pub struct PostgresSecretStore {
    pool: crate::traced_pg::PgPool,
}
impl PostgresSecretStore {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self {
            pool: crate::traced_pg::wrap(pool),
        }
    }
}
#[async_trait]
impl SecretStore for PostgresSecretStore {
    fn backend_label(&self) -> &'static str {
        "postgres"
    }
    async fn compare_exchange_kv(
        &self,
        key: &str,
        expected: &[u8],
        replacement: &[u8],
    ) -> Result<bool, SecretStoreError> {
        Ok(sqlx::query("UPDATE kv_store SET value = $3 WHERE key = $1 AND value = $2 AND (expires_at IS NULL OR expires_at > NOW())")
            .bind(key).bind(expected).bind(replacement).execute(&self.pool).await?.rows_affected() == 1)
    }
    async fn purge_expired(&self, limit: u32) -> Result<u64, SecretStoreError> {
        Ok(sqlx::query("WITH expired AS (SELECT key FROM kv_store WHERE expires_at <= NOW() ORDER BY expires_at LIMIT $1 FOR UPDATE SKIP LOCKED) DELETE FROM kv_store USING expired WHERE kv_store.key = expired.key")
            .bind(i64::from(limit)).execute(&self.pool).await?.rows_affected())
    }
    async fn consume_kv(&self, key: &str, expected: &[u8]) -> Result<bool, SecretStoreError> {
        Ok(sqlx::query("DELETE FROM kv_store WHERE key = $1 AND value = $2 AND (expires_at IS NULL OR expires_at > NOW())").bind(key).bind(expected).execute(&self.pool).await?.rows_affected() == 1)
    }
    async fn store_kv(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<(), SecretStoreError> {
        let expires = ttl
            .map(|ttl| {
                let duration = chrono::Duration::from_std(ttl).map_err(|_| {
                    SecretStoreError::Configuration("secret expiry exceeds supported range")
                })?;
                chrono::Utc::now().checked_add_signed(duration).ok_or(
                    SecretStoreError::Configuration("secret expiry exceeds supported range"),
                )
            })
            .transpose()?;
        sqlx::query("INSERT INTO kv_store (key, value, expires_at) VALUES ($1, $2, $3) ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, expires_at = EXCLUDED.expires_at").bind(key).bind(value).bind(expires).execute(&self.pool).await?;
        Ok(())
    }
    async fn get_kv(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        Ok(sqlx::query_scalar("SELECT value FROM kv_store WHERE key = $1 AND (expires_at IS NULL OR expires_at > NOW())").bind(key).fetch_optional(&self.pool).await?)
    }
    async fn delete_kv(&self, key: &str) -> Result<(), SecretStoreError> {
        sqlx::query("DELETE FROM kv_store WHERE key = $1")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    async fn take_kv(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        Ok(sqlx::query_scalar("DELETE FROM kv_store WHERE key = $1 AND (expires_at IS NULL OR expires_at > NOW()) RETURNING value").bind(key).fetch_optional(&self.pool).await?)
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    data: String,
    nonce: String,
    algorithm: String,
    key_derivation: String,
}
pub struct SecretEncryption {
    cipher: Aes256Gcm,
}
pub struct EncryptedSecretStore<S> {
    inner: S,
    encryption: SecretEncryption,
}
impl<S> EncryptedSecretStore<S> {
    pub fn new(inner: S, encryption: SecretEncryption) -> Self {
        Self { inner, encryption }
    }
}
#[async_trait]
impl<S: SecretStore> SecretStore for EncryptedSecretStore<S> {
    fn backend_label(&self) -> &'static str {
        self.inner.backend_label()
    }
    async fn compare_exchange_kv(
        &self,
        key: &str,
        expected: &[u8],
        replacement: &[u8],
    ) -> Result<bool, SecretStoreError> {
        let Some(ciphertext) = self.inner.get_kv(key).await? else {
            return Ok(false);
        };
        if self.encryption.decrypt(&ciphertext)? != expected {
            return Ok(false);
        }
        self.inner
            .compare_exchange_kv(key, &ciphertext, &self.encryption.encrypt(replacement)?)
            .await
    }
    async fn purge_expired(&self, limit: u32) -> Result<u64, SecretStoreError> {
        self.inner.purge_expired(limit).await
    }
    async fn consume_kv(&self, key: &str, expected: &[u8]) -> Result<bool, SecretStoreError> {
        let Some(ciphertext) = self.inner.get_kv(key).await? else {
            return Ok(false);
        };
        if self.encryption.decrypt(&ciphertext)? != expected {
            return Ok(false);
        }
        self.inner.consume_kv(key, &ciphertext).await
    }
    async fn store_kv(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<(), SecretStoreError> {
        self.inner
            .store_kv(key, &self.encryption.encrypt(value)?, ttl)
            .await
    }
    async fn get_kv(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        self.inner
            .get_kv(key)
            .await?
            .map(|bytes| self.encryption.decrypt(&bytes))
            .transpose()
    }
    async fn delete_kv(&self, key: &str) -> Result<(), SecretStoreError> {
        self.inner.delete_kv(key).await
    }
    async fn take_kv(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        let Some(ciphertext) = self.inner.get_kv(key).await? else {
            return Ok(None);
        };
        let plaintext = self.encryption.decrypt(&ciphertext)?;
        Ok(self
            .inner
            .consume_kv(key, &ciphertext)
            .await?
            .then_some(plaintext))
    }
}
impl SecretEncryption {
    pub fn generate_key() -> Result<String, SecretStoreError> {
        let mut key = [0; 32];
        getrandom::fill(&mut key).map_err(SecretStoreError::Entropy)?;
        Ok(STANDARD.encode(key))
    }
    pub fn from_key(key: &[u8]) -> Result<Self, SecretStoreError> {
        Ok(Self {
            cipher: Aes256Gcm::new_from_slice(key).map_err(|_| {
                SecretStoreError::Configuration("encryption key must contain 32 bytes")
            })?,
        })
    }
    pub fn from_environment() -> Result<Self, SecretStoreError> {
        let key = std::env::var("AUTH_STORAGE_ENCRYPTION_KEY").map_err(|_| {
            SecretStoreError::Configuration("AUTH_STORAGE_ENCRYPTION_KEY is required")
        })?;
        Self::from_key(&STANDARD.decode(key)?)
    }
    pub fn encrypt(&self, bytes: &[u8]) -> Result<Vec<u8>, SecretStoreError> {
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce).map_err(SecretStoreError::Entropy)?;
        let data = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), bytes)
            .map_err(|_| SecretStoreError::Authentication)?;
        Ok(serde_json::to_vec(&Envelope {
            data: STANDARD.encode(data),
            nonce: STANDARD.encode(nonce),
            algorithm: "AES-256-GCM".into(),
            key_derivation: "direct".into(),
        })?)
    }
    pub fn decrypt(&self, bytes: &[u8]) -> Result<Vec<u8>, SecretStoreError> {
        let envelope: Envelope = serde_json::from_slice(bytes)?;
        if envelope.algorithm != "AES-256-GCM" || envelope.key_derivation != "direct" {
            return Err(SecretStoreError::Authentication);
        }
        let nonce = STANDARD.decode(envelope.nonce)?;
        if nonce.len() != 12 {
            return Err(SecretStoreError::Authentication);
        }
        self.cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                STANDARD.decode(envelope.data)?.as_slice(),
            )
            .map_err(|_| SecretStoreError::Authentication)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_existing_aes256_gcm_envelope() {
        // NIST empty-plaintext AES-256-GCM vector, zero key and 96-bit zero nonce.
        let envelope = Envelope {
            data: STANDARD.encode(hex::decode("530f8afbc74536b9a963b4f1c4cb738b").unwrap()),
            nonce: STANDARD.encode([0; 12]),
            algorithm: "AES-256-GCM".into(),
            key_derivation: "direct".into(),
        };
        assert_eq!(
            SecretEncryption::from_key(&[0; 32])
                .unwrap()
                .decrypt(&serde_json::to_vec(&envelope).unwrap())
                .unwrap(),
            b""
        );
    }
    #[tokio::test]
    async fn redemption_has_one_winner() {
        let store = MemorySecretStore::new();
        store.store_kv("grant", b"value", None).await.unwrap();
        let (left, right) = tokio::join!(store.take_kv("grant"), store.take_kv("grant"));
        assert_eq!(
            usize::from(left.unwrap().is_some()) + usize::from(right.unwrap().is_some()),
            1
        );
    }
    #[tokio::test]
    async fn expired_records_are_absent_for_reads_and_redemption() {
        let store = MemorySecretStore::new();
        store
            .store_kv("read", b"expired", Some(Duration::ZERO))
            .await
            .unwrap();
        store
            .store_kv("take", b"expired", Some(Duration::ZERO))
            .await
            .unwrap();
        assert!(store.get_kv("read").await.unwrap().is_none());
        assert!(store.take_kv("take").await.unwrap().is_none());
    }
    #[tokio::test]
    async fn compare_and_consume_does_not_delete_a_replacement() {
        let store = EncryptedSecretStore::new(
            MemorySecretStore::new(),
            SecretEncryption::from_key(&[7; 32]).unwrap(),
        );
        store.store_kv("grant", b"first", None).await.unwrap();
        let observed = store.get_kv("grant").await.unwrap().unwrap();
        store.store_kv("grant", b"replacement", None).await.unwrap();
        assert!(!store.consume_kv("grant", &observed).await.unwrap());
        assert_eq!(
            store.get_kv("grant").await.unwrap().unwrap(),
            b"replacement"
        );
        let (left, right) = tokio::join!(
            store.consume_kv("grant", b"replacement"),
            store.consume_kv("grant", b"replacement")
        );
        assert_eq!(usize::from(left.unwrap()) + usize::from(right.unwrap()), 1);
    }
    #[tokio::test]
    async fn authentication_failure_preserves_the_stored_ciphertext() {
        let mut store = EncryptedSecretStore::new(
            MemorySecretStore::new(),
            SecretEncryption::from_key(&[7; 32]).unwrap(),
        );
        store.store_kv("credential", b"secret", None).await.unwrap();
        store.encryption = SecretEncryption::from_key(&[8; 32]).unwrap();
        assert!(matches!(
            store.take_kv("credential").await,
            Err(SecretStoreError::Authentication)
        ));
        assert!(store.inner.get_kv("credential").await.unwrap().is_some());
        store.encryption = SecretEncryption::from_key(&[7; 32]).unwrap();
        assert_eq!(
            store.take_kv("credential").await.unwrap().unwrap(),
            b"secret"
        );
    }
    #[tokio::test]
    async fn postgres_encrypted_records_expire_and_redeem_atomically() {
        let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
            return;
        };
        let pool = sqlx::PgPool::connect(&url).await.unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS kv_store (key VARCHAR(255) PRIMARY KEY, value BYTEA NOT NULL, expires_at TIMESTAMPTZ, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW())").execute(&pool).await.unwrap();
        let key = format!("secret_store_test:{}", uuid::Uuid::new_v4());
        let store = EncryptedSecretStore::new(
            PostgresSecretStore::new(pool.clone()),
            SecretEncryption::from_key(&[7; 32]).unwrap(),
        );
        store
            .store_kv(&key, b"credential", Some(Duration::from_secs(60)))
            .await
            .unwrap();
        assert_eq!(store.get_kv(&key).await.unwrap().unwrap(), b"credential");
        let persisted: Vec<u8> = sqlx::query_scalar("SELECT value FROM kv_store WHERE key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(persisted, b"credential");
        assert!(!store.consume_kv(&key, b"wrong record").await.unwrap());
        assert_eq!(store.get_kv(&key).await.unwrap().unwrap(), b"credential");
        let (left, right) = tokio::join!(
            store.consume_kv(&key, b"credential"),
            store.consume_kv(&key, b"credential")
        );
        assert_eq!(usize::from(left.unwrap()) + usize::from(right.unwrap()), 1);
        store
            .store_kv(&key, b"expired", Some(Duration::ZERO))
            .await
            .unwrap();
        assert!(store.get_kv(&key).await.unwrap().is_none());
        assert!(store.take_kv(&key).await.unwrap().is_none());
        store.delete_kv(&key).await.unwrap();
    }
    #[test]
    fn ciphertext_authentication_rejects_wrong_key_and_bad_nonce() {
        let encryption = SecretEncryption::from_key(&[7; 32]).unwrap();
        let bytes = encryption.encrypt(b"credential").unwrap();
        assert_eq!(encryption.decrypt(&bytes).unwrap(), b"credential");
        assert!(matches!(
            SecretEncryption::from_key(&[8; 32])
                .unwrap()
                .decrypt(&bytes),
            Err(SecretStoreError::Authentication)
        ));
        let mut envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
        envelope.nonce = STANDARD.encode([0; 11]);
        assert!(matches!(
            encryption.decrypt(&serde_json::to_vec(&envelope).unwrap()),
            Err(SecretStoreError::Authentication)
        ));
    }
}
