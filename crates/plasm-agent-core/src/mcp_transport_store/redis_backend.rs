//! Shared Redis connection + JSON key helpers for MCP multi-replica stores.

use std::time::Duration;

use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use tracing::{debug, warn};

#[derive(Clone)]
pub struct RedisBackend {
    conn: ConnectionManager,
    ttl: Duration,
}

impl RedisBackend {
    pub async fn connect(redis_url: &str, ttl: Duration) -> redis::RedisResult<Self> {
        let client = redis::Client::open(redis_url)?;
        let conn = ConnectionManager::new(client).await?;
        Ok(Self { conn, ttl })
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub async fn ping(&self) -> redis::RedisResult<()> {
        let mut conn = self.conn.clone();
        let _: String = redis::cmd("PING").query_async(&mut conn).await?;
        Ok(())
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        let mut conn = self.conn.clone();
        let raw: Option<String> = conn.get(key).await.unwrap_or_else(|err| {
            warn!(?err, key, "redis get failed");
            None
        });
        raw.and_then(|s| {
            serde_json::from_str(&s)
                .map_err(|err| {
                    warn!(?err, key, "invalid redis JSON");
                    err
                })
                .ok()
        })
    }

    pub async fn set_json<T: serde::Serialize>(&self, key: &str, value: &T) {
        let Ok(payload) = serde_json::to_string(value) else {
            return;
        };
        let mut conn = self.conn.clone();
        let ttl_secs = self.ttl.as_secs().max(60);
        if let Err(err) = conn.set_ex::<_, _, ()>(key, payload, ttl_secs).await {
            warn!(?err, key, "redis set failed");
        }
    }

    pub async fn delete(&self, key: &str) {
        let mut conn = self.conn.clone();
        let _: redis::RedisResult<()> = conn.del(key).await;
    }

    /// Delete all keys whose names start with `prefix` (Redis `SCAN` + `DEL`).
    pub(crate) async fn delete_keys_matching_prefix(&self, prefix: &str) -> u64 {
        let pattern = format!("{prefix}*");
        let mut conn = self.conn.clone();
        let mut cursor: u64 = 0;
        let mut deleted = 0u64;
        loop {
            let scan_result: redis::RedisResult<(u64, Vec<String>)> = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await;
            let (next_cursor, keys) = match scan_result {
                Ok(pair) => pair,
                Err(err) => {
                    warn!(?err, prefix, "redis SCAN failed during prefix delete");
                    break;
                }
            };
            if !keys.is_empty() {
                match conn.del::<_, u64>(keys).await {
                    Ok(n) => deleted = deleted.saturating_add(n),
                    Err(err) => warn!(?err, prefix, "redis DEL failed during prefix delete"),
                }
            }
            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }
        deleted
    }

    pub async fn touch(&self, key: &str) {
        let mut conn = self.conn.clone();
        let ttl_secs = self.ttl.as_secs().max(60) as i64;
        if let Err(err) = conn.expire::<_, ()>(key, ttl_secs).await {
            debug!(?err, key, "redis TTL refresh failed");
        }
    }

    pub async fn get_bytes(&self, key: &str) -> Option<Vec<u8>> {
        let mut conn = self.conn.clone();
        conn.get(key).await.unwrap_or_else(|err| {
            warn!(?err, key, "redis get bytes failed");
            None
        })
    }

    pub async fn set_bytes(&self, key: &str, value: &[u8]) -> bool {
        let mut conn = self.conn.clone();
        let ttl_secs = self.ttl.as_secs().max(60);
        conn.set_ex::<_, _, ()>(key, value, ttl_secs)
            .await
            .map(|()| true)
            .unwrap_or_else(|err| {
                warn!(?err, key, "redis set bytes failed");
                false
            })
    }
}
