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

    pub async fn touch(&self, key: &str) {
        let mut conn = self.conn.clone();
        let ttl_secs = self.ttl.as_secs().max(60) as i64;
        if let Err(err) = conn.expire::<_, ()>(key, ttl_secs).await {
            debug!(?err, key, "redis TTL refresh failed");
        }
    }
}
