//! Cross-pod registry: logical session UUID → execute `(prompt_hash, session_id)`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use super::redis_backend::RedisBackend;

const LOGICAL_KEY_PREFIX: &str = "mcp:execute:logical:";
const BY_SESSION_KEY_PREFIX: &str = "mcp:execute:logical:by_session:";

#[derive(serde::Serialize, serde::Deserialize)]
struct LogicalBindingRecord {
    prompt_hash: String,
    session_id: String,
}

fn logical_key(uuid: &Uuid) -> String {
    format!("{LOGICAL_KEY_PREFIX}{uuid}")
}

fn by_session_key(prompt_hash: &str, session_id: &str) -> String {
    format!("{BY_SESSION_KEY_PREFIX}{prompt_hash}:{session_id}")
}

#[derive(Clone, Hash, Eq, PartialEq)]
struct SessionBindingKey {
    prompt_hash: String,
    session_id: String,
}

/// In-memory cache with optional Redis mirror for multi-replica `plasm-mcp`.
#[derive(Clone, Default)]
pub struct LogicalExecuteBindingRegistry {
    local: Arc<RwLock<HashMap<Uuid, (String, String)>>>,
    by_session: Arc<RwLock<HashMap<SessionBindingKey, Uuid>>>,
    redis: Arc<RwLock<Option<Arc<RedisBackend>>>>,
}

impl LogicalExecuteBindingRegistry {
    pub fn new_in_memory() -> Self {
        Self::default()
    }

    pub async fn attach_redis(&self, backend: Arc<RedisBackend>) {
        *self.redis.write().await = Some(backend);
    }

    async fn redis(&self) -> Option<Arc<RedisBackend>> {
        self.redis.read().await.clone()
    }

    pub async fn get(&self, logical_id: &Uuid) -> Option<(String, String)> {
        {
            let g = self.local.read().await;
            if let Some(pair) = g.get(logical_id) {
                if let Some(redis) = self.redis().await.as_ref() {
                    redis.touch(&logical_key(logical_id)).await;
                }
                return Some(pair.clone());
            }
        }
        let redis = self.redis().await?;
        let record: LogicalBindingRecord = redis.get_json(&logical_key(logical_id)).await?;
        let pair = (record.prompt_hash, record.session_id);
        let mut g = self.local.write().await;
        let mut rev = self.by_session.write().await;
        g.insert(*logical_id, pair.clone());
        rev.insert(
            SessionBindingKey {
                prompt_hash: pair.0.clone(),
                session_id: pair.1.clone(),
            },
            *logical_id,
        );
        Some(pair)
    }

    pub async fn insert(&self, logical_id: Uuid, prompt_hash: String, session_id: String) {
        {
            let mut g = self.local.write().await;
            g.insert(logical_id, (prompt_hash.clone(), session_id.clone()));
            let mut rev = self.by_session.write().await;
            rev.insert(
                SessionBindingKey {
                    prompt_hash: prompt_hash.clone(),
                    session_id: session_id.clone(),
                },
                logical_id,
            );
        }
        if let Some(redis) = self.redis().await.as_ref() {
            let record = LogicalBindingRecord {
                prompt_hash: prompt_hash.clone(),
                session_id: session_id.clone(),
            };
            redis.set_json(&logical_key(&logical_id), &record).await;
            redis
                .set_json(
                    &by_session_key(&prompt_hash, &session_id),
                    &logical_id.to_string(),
                )
                .await;
        }
    }

    pub async fn remove(&self, logical_id: &Uuid) {
        let removed = {
            let mut g = self.local.write().await;
            g.remove(logical_id)
        };
        if let Some((ph, sid)) = removed {
            let mut rev = self.by_session.write().await;
            rev.remove(&SessionBindingKey {
                prompt_hash: ph.clone(),
                session_id: sid.clone(),
            });
            if let Some(redis) = self.redis().await.as_ref() {
                redis.delete(&logical_key(logical_id)).await;
                redis.delete(&by_session_key(&ph, &sid)).await;
            }
        }
    }

    pub async fn find_by_execute(&self, prompt_hash: &str, session_id: &str) -> Option<Uuid> {
        let key = SessionBindingKey {
            prompt_hash: prompt_hash.to_string(),
            session_id: session_id.to_string(),
        };
        {
            let rev = self.by_session.read().await;
            if let Some(uuid) = rev.get(&key) {
                return Some(*uuid);
            }
        }
        let redis = self.redis().await?;
        let uuid_str: String = redis
            .get_json(&by_session_key(prompt_hash, session_id))
            .await?;
        uuid_str.parse().ok()
    }

    /// Remove logical binding rows for a stale execute `(prompt_hash, session_id)` pair.
    pub(crate) async fn delete_for_execute(&self, prompt_hash: &str, session_id: &str) {
        if let Some(uuid) = self.find_by_execute(prompt_hash, session_id).await {
            self.remove(&uuid).await;
        }
    }

    /// Clear local caches and Redis logical binding keys (catalog reload / cluster-wide reset).
    pub async fn purge_redis_and_local(&self) -> u64 {
        {
            let mut g = self.local.write().await;
            g.clear();
            let mut rev = self.by_session.write().await;
            rev.clear();
        }
        if let Some(redis) = self.redis().await {
            redis
                .delete_keys_matching_prefix(LOGICAL_KEY_PREFIX)
                .await
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reverse_index_find_by_execute() {
        let reg = LogicalExecuteBindingRegistry::new_in_memory();
        let uuid = Uuid::new_v4();
        reg.insert(uuid, "phabc".into(), "sid123".into()).await;
        assert_eq!(
            reg.get(&uuid).await,
            Some(("phabc".to_string(), "sid123".to_string()))
        );
        assert_eq!(reg.find_by_execute("phabc", "sid123").await, Some(uuid));
    }
}
