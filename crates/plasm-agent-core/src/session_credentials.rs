//! Credential effects use the existing execute-session registry and its configured backend.

use async_trait::async_trait;
use plasm_runtime::credentials::{
    credential_error, CredentialReference, CredentialScope, SessionCredentialStore,
    StoredCredential,
};
use plasm_runtime::RuntimeError;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

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
        let input = serde_json::to_vec(&(&scope, source, lifetime_seconds))
            .map_err(|_| credential_error("cannot encode credential effect"))?;
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
                .ok_or_else(|| credential_error("invalid credential lifetime"))?,
        };
        let committed = self
            .registry
            .commit_credential_record(
                &self.memory,
                &operation_key,
                &reference_key(&record.scope, &record.reference),
                &record,
            )
            .await?;
        if committed.scope != record.scope
            || committed.source != record.source
            || committed.expires_at_unix <= now()
        {
            return Err(credential_error(
                "credential commit does not match the requested effect",
            ));
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
            .await?
            .ok_or_else(|| credential_error("credential reference is unavailable"))?;
        if record.reference != *reference
            || record.scope != *scope
            || record.expires_at_unix <= now()
        {
            return Err(credential_error(
                "credential reference scope or lifetime does not permit this request",
            ));
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
