//! Credential effects use the existing execute-session registry and its configured backend.

use async_trait::async_trait;
use plasm_runtime::credentials::{
    CredentialReference, CredentialScope, SessionCredentialStore, StoredCredential,
};
use plasm_runtime::RuntimeError;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

#[derive(Debug, thiserror::Error)]
pub(crate) enum CredentialPersistenceError {
    #[error("cannot encode credential effect: {source}")]
    EffectEncoding {
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid credential lifetime: {seconds} seconds")]
    InvalidLifetime { seconds: u64 },
    #[error("credential commit does not match the requested effect")]
    CommitEffectMismatch,
    #[error("credential reference is unavailable")]
    ReferenceUnavailable,
    #[error("credential reference scope or lifetime does not permit this request")]
    ReferenceScopeOrLifetime,
    #[error("cannot serialize credential record: {source}")]
    RecordEncoding {
        #[source]
        source: serde_json::Error,
    },
    #[error("durable credential commit failed: {source}")]
    DurableCommit {
        #[source]
        source: redis::RedisError,
    },
    #[error("invalid stored credential operation: {source}")]
    StoredOperation {
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid credential commit acknowledgement: {source}")]
    CommitAcknowledgement {
        #[source]
        source: serde_json::Error,
    },
    #[error("durable credential lookup failed: {source}")]
    DurableLookup {
        #[source]
        source: redis::RedisError,
    },
    #[error("invalid stored credential record: {source}")]
    StoredRecord {
        #[source]
        source: serde_json::Error,
    },
}

impl CredentialPersistenceError {
    pub(crate) fn into_runtime(self) -> RuntimeError {
        RuntimeError::CredentialProvider {
            source: Box::new(self),
        }
    }
}

#[derive(Default)]
pub struct CredentialMemory(pub(crate) RwLock<HashMap<String, String>>);

impl std::fmt::Debug for CredentialMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialMemory { .. }")
    }
}

pub(crate) struct HostCredentialStore {
    pub registry: crate::mcp_transport_store::execute_session_registry::ExecuteSessionRegistry,
    pub memory: Arc<CredentialMemory>,
}

impl std::fmt::Debug for HostCredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostCredentialStore { .. }")
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn reference_key(scope: &CredentialScope, reference: &CredentialReference) -> String {
    format!(
        "mcp:execute:session:{{{}}}:credential:{}",
        scope.session,
        reference.as_str()
    )
}

#[async_trait]
impl SessionCredentialStore for HostCredentialStore {
    async fn bind(
        &self,
        scope: CredentialScope,
        source: plasm_compile::CredentialSource,
        lifetime_seconds: u64,
    ) -> Result<CredentialReference, RuntimeError> {
        // This digest is an internal idempotency lookup key, never a public receipt/fingerprint.
        let input = serde_json::to_vec(&(&scope, source, lifetime_seconds)).map_err(|source| {
            CredentialPersistenceError::EffectEncoding { source }.into_runtime()
        })?;
        let digest = hex::encode(Sha256::digest(input));
        let operation_key = format!(
            "mcp:execute:session:{{{}}}:credential-operation:{digest}",
            scope.session
        );
        let reference =
            CredentialReference::parse(&format!("cr{}", uuid::Uuid::new_v4().simple()))?;
        let record = StoredCredential {
            reference,
            scope,
            source,
            expires_at_unix: now()
                .checked_add(lifetime_seconds)
                .filter(|expires| *expires > now())
                .ok_or_else(|| {
                    CredentialPersistenceError::InvalidLifetime {
                        seconds: lifetime_seconds,
                    }
                    .into_runtime()
                })?,
        };
        let committed = self
            .registry
            .commit_credential_record(
                &self.memory,
                &operation_key,
                &reference_key(&record.scope, &record.reference),
                &record,
            )
            .await
            .map_err(CredentialPersistenceError::into_runtime)?;
        if committed.scope != record.scope
            || committed.source != record.source
            || committed.expires_at_unix <= now()
        {
            return Err(CredentialPersistenceError::CommitEffectMismatch.into_runtime());
        }
        Ok(committed.reference)
    }

    async fn resolve(
        &self,
        reference: &CredentialReference,
        scope: &CredentialScope,
    ) -> Result<plasm_compile::CredentialSource, RuntimeError> {
        let record = self
            .registry
            .load_credential_record(&self.memory, &reference_key(scope, reference))
            .await
            .map_err(CredentialPersistenceError::into_runtime)?
            .ok_or_else(|| CredentialPersistenceError::ReferenceUnavailable.into_runtime())?;
        if record.reference != *reference
            || record.scope != *scope
            || record.expires_at_unix <= now()
        {
            return Err(CredentialPersistenceError::ReferenceScopeOrLifetime.into_runtime());
        }
        Ok(record.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_transport_store::execute_session_registry::ExecuteSessionRegistry;
    use plasm_compile::CredentialSource;

    fn scope() -> CredentialScope {
        CredentialScope {
            session: "one".into(),
            catalog_revision: "revision".into(),
            origin: "https://example.test".into(),
            slot: "read".into(),
            resource: serde_json::json!({"record":"a"}),
        }
    }

    #[tokio::test]
    async fn corrupt_record_preserves_json_cause_at_runtime_boundary() {
        use std::error::Error;
        let (registry, json) = ExecuteSessionRegistry::with_test_json_store();
        let reference =
            CredentialReference::parse(&format!("cr{}", uuid::Uuid::new_v4().simple())).unwrap();
        json.write()
            .await
            .insert(reference_key(&scope(), &reference), "{".into());
        let store = HostCredentialStore {
            registry,
            memory: Default::default(),
        };
        let error = store.resolve(&reference, &scope()).await.unwrap_err();
        let cause = error
            .source()
            .unwrap()
            .downcast_ref::<CredentialPersistenceError>()
            .unwrap();
        assert!(matches!(
            cause,
            CredentialPersistenceError::StoredRecord { .. }
        ));
        assert!(cause
            .source()
            .unwrap()
            .downcast_ref::<serde_json::Error>()
            .is_some());
    }

    #[tokio::test]
    async fn invalid_lifetime_is_semantic_and_never_committed() {
        use std::error::Error;
        let store = HostCredentialStore {
            registry: ExecuteSessionRegistry::new_in_memory(),
            memory: Default::default(),
        };
        let error = store
            .bind(scope(), CredentialSource::Host {}, 0)
            .await
            .unwrap_err();
        assert!(matches!(
            error
                .source()
                .unwrap()
                .downcast_ref::<CredentialPersistenceError>(),
            Some(CredentialPersistenceError::InvalidLifetime { seconds: 0 })
        ));
        assert!(store.memory.0.read().await.is_empty());
    }

    #[test]
    fn redis_cause_survives_provider_boundary() {
        use std::error::Error;
        let source = redis::RedisError::from((redis::ErrorKind::IoError, "synthetic timeout"));
        let error = CredentialPersistenceError::DurableCommit { source }.into_runtime();
        let cause = error
            .source()
            .unwrap()
            .downcast_ref::<CredentialPersistenceError>()
            .unwrap();
        assert!(cause
            .source()
            .unwrap()
            .downcast_ref::<redis::RedisError>()
            .is_some());
    }

    #[tokio::test]
    #[ignore = "requires a disposable PLASM_TEST_REDIS_URL with ACL administration"]
    async fn redis_commit_rehydrates_and_write_failure_is_not_acknowledged() {
        use crate::mcp_transport_store::RedisBackend;
        use std::time::Duration;
        let redis_url = std::env::var("PLASM_TEST_REDIS_URL").expect("disposable Redis URL");
        let registry = ExecuteSessionRegistry::new_in_memory();
        registry
            .attach_redis(Arc::new(
                RedisBackend::connect(&redis_url, Duration::from_secs(1))
                    .await
                    .unwrap(),
            ))
            .await;
        let first = HostCredentialStore {
            registry,
            memory: Default::default(),
        };
        let mut scope = scope();
        scope.session = uuid::Uuid::new_v4().to_string();
        let (left, right) = tokio::join!(
            first.bind(scope.clone(), CredentialSource::Host {}, 3600),
            first.bind(scope.clone(), CredentialSource::Host {}, 3600)
        );
        let reference = left.unwrap();
        assert_eq!(reference, right.unwrap());
        assert!(first.memory.0.read().await.is_empty());
        let registry = ExecuteSessionRegistry::new_in_memory();
        registry
            .attach_redis(Arc::new(
                RedisBackend::connect(&redis_url, Duration::from_secs(1))
                    .await
                    .unwrap(),
            ))
            .await;
        let second = HostCredentialStore {
            registry,
            memory: Default::default(),
        };
        assert_eq!(
            second.resolve(&reference, &scope).await.unwrap(),
            CredentialSource::Host {}
        );
        let mut admin = redis::Client::open(redis_url.as_str())
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let key = reference_key(&scope, &reference);
        let stored: String = redis::cmd("GET")
            .arg(&key)
            .query_async(&mut admin)
            .await
            .unwrap();
        let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(stored["source"], serde_json::json!({"scheme":"host"}));
        assert_eq!(
            stored.as_object().unwrap().len(),
            4,
            "only reference, scope, source and expiry are persisted"
        );
        let ttl: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut admin)
            .await
            .unwrap();
        assert!(
            ttl > 3500,
            "declared credential lifetime must not inherit the short session cache TTL"
        );
        let _: i64 = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut admin)
            .await
            .unwrap();
        assert_eq!(
            first
                .bind(scope.clone(), CredentialSource::Host {}, 3600)
                .await
                .unwrap(),
            reference
        );
        assert_eq!(
            second.resolve(&reference, &scope).await.unwrap(),
            CredentialSource::Host {}
        );

        let username = format!("credential-test-{}", uuid::Uuid::new_v4().simple());
        let _: String = redis::cmd("ACL")
            .arg("SETUSER")
            .arg(&username)
            .arg("on")
            .arg(">synthetic-test-password")
            .arg("~*")
            .arg("+@connection")
            .arg("+get")
            .arg("+eval")
            .query_async(&mut admin)
            .await
            .unwrap();
        let mut restricted_url = url::Url::parse(&redis_url).unwrap();
        restricted_url.set_username(&username).unwrap();
        restricted_url
            .set_password(Some("synthetic-test-password"))
            .unwrap();
        let registry = ExecuteSessionRegistry::new_in_memory();
        registry
            .attach_redis(Arc::new(
                RedisBackend::connect(restricted_url.as_str(), Duration::from_secs(60))
                    .await
                    .unwrap(),
            ))
            .await;
        let denied = HostCredentialStore {
            registry,
            memory: Default::default(),
        };
        assert!(denied
            .bind(scope, CredentialSource::Host {}, 3600)
            .await
            .is_err());
        assert!(
            denied.memory.0.read().await.is_empty(),
            "failed durable commit must not leave a hot-only record"
        );
        let _: i64 = redis::cmd("ACL")
            .arg("DELUSER")
            .arg(username)
            .query_async(&mut admin)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn immutable_references_deduplicate_and_rehydrate() {
        let (registry, _) = ExecuteSessionRegistry::with_test_json_store();
        let first = HostCredentialStore {
            registry: registry.clone(),
            memory: Default::default(),
        };
        let second = HostCredentialStore {
            registry,
            memory: Default::default(),
        };
        let (left, right) = tokio::join!(
            first.bind(scope(), CredentialSource::Host {}, 86400),
            second.bind(scope(), CredentialSource::Host {}, 86400)
        );
        let reference = left.unwrap();
        assert_eq!(reference, right.unwrap());
        assert_eq!(
            second.resolve(&reference, &scope()).await.unwrap(),
            CredentialSource::Host {}
        );
        let changed = first
            .bind(scope(), CredentialSource::Host {}, 3600)
            .await
            .unwrap();
        assert_ne!(changed, reference);
        assert_eq!(
            first.resolve(&reference, &scope()).await.unwrap(),
            CredentialSource::Host {}
        );
        for altered in [
            CredentialScope {
                session: "two".into(),
                ..scope()
            },
            CredentialScope {
                catalog_revision: "another".into(),
                ..scope()
            },
            CredentialScope {
                origin: "https://other.test".into(),
                ..scope()
            },
            CredentialScope {
                resource: serde_json::json!({"record":"b"}),
                ..scope()
            },
            CredentialScope {
                slot: "write".into(),
                ..scope()
            },
        ] {
            assert!(second.resolve(&reference, &altered).await.is_err());
        }
    }
}
