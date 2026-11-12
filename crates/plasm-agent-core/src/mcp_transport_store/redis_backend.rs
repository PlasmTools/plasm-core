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
    pub(crate) async fn delete_session_publication(
        &self,
        session_key: &str,
    ) -> redis::RedisResult<u64> {
        let mut conn = self.conn.clone();
        let deleted: u64 = redis::cmd("DEL")
            .arg(session_key)
            .arg(format!("{session_key}:example_receipts"))
            .arg(format!("{session_key}:learning"))
            .query_async(&mut conn)
            .await?;
        Ok(deleted)
    }

    pub(crate) async fn purge_session_publications(&self, prefix: &str) -> u64 {
        let mut conn = self.conn.clone();
        let mut cursor = 0u64;
        let mut deleted = 0u64;
        loop {
            let scan: redis::RedisResult<(u64, Vec<String>)> = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(format!("{prefix}*"))
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await;
            let (next, keys) = match scan {
                Ok(result) => result,
                Err(error) => {
                    warn!(%error, "session publication scan failed");
                    break;
                }
            };
            let bases: std::collections::BTreeSet<_> = keys
                .iter()
                .map(|key| {
                    key.strip_suffix(":learning")
                        .or_else(|| key.strip_suffix(":example_receipts"))
                        .unwrap_or(key)
                        .to_owned()
                })
                .collect();
            for base in bases {
                match self.delete_session_publication(&base).await {
                    Ok(count) => deleted = deleted.saturating_add(count),
                    Err(error) => warn!(%error, "session publication purge failed atomically"),
                }
            }
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        deleted
    }

    pub(crate) async fn begin_learning_execution(
        &self,
        session_key: &str,
        attempt: &str,
    ) -> redis::RedisResult<u64> {
        let mut conn = self.conn.clone();
        redis::cmd("EVAL").arg(r#"
if redis.call('EXISTS', KEYS[1]) == 0 then return redis.error_reply('execute session unavailable') end
local clock = redis.call('TIME')
local now = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)
local review_until = tonumber(redis.call('HGET', KEYS[2], 'review_until') or '0')
if review_until > now then return redis.error_reply('workflow review is sealing; retry execution') end
local prior = redis.call('HGET', KEYS[2], 'attempt:' .. ARGV[1])
if prior then return tonumber(prior) end
local issued = tonumber(redis.call('HGET', KEYS[2], 'issued') or '0')
local completed = tonumber(redis.call('HGET', KEYS[2], 'completed') or '0')
if not issued or not completed or issued >= 9007199254740991 then return redis.error_reply('invalid learning execution watermark') end
local token = issued + 1
redis.call('HSET', KEYS[2], 'issued', token, 'completed', completed, 'attempt:' .. ARGV[1], token)
return token
"#).arg(2).arg(session_key).arg(format!("{session_key}:learning"))
            .arg(attempt).query_async(&mut conn).await
    }

    pub(crate) async fn complete_learning_execution(
        &self,
        session_key: &str,
        token: u64,
    ) -> redis::RedisResult<bool> {
        let mut conn = self.conn.clone();
        let result: i64 = redis::cmd("EVAL")
            .arg(
                r#"
local issued = tonumber(redis.call('HGET', KEYS[1], 'issued') or '0')
local completed = tonumber(redis.call('HGET', KEYS[1], 'completed') or '0')
local token = tonumber(ARGV[1])
if not token or token <= 0 or token > issued or token ~= math.floor(token) then return 0 end
if token > completed then
    redis.call('HSET', KEYS[1], 'done:' .. ARGV[1], 1)
    while completed < issued and redis.call('HDEL', KEYS[1], 'done:' .. string.format('%.0f', completed + 1)) == 1 do
        completed = completed + 1
    end
    redis.call('HSET', KEYS[1], 'completed', completed)
end
if completed == issued then return 1 end
return 0
"#,
            )
            .arg(1)
            .arg(format!("{session_key}:learning"))
            .arg(token)
            .query_async(&mut conn)
            .await?;
        Ok(result == 1)
    }

    #[cfg(test)]
    pub(crate) async fn learning_evidence_complete(
        &self,
        session_key: &str,
    ) -> redis::RedisResult<bool> {
        let mut conn = self.conn.clone();
        let result: i64 = redis::cmd("EVAL")
            .arg(
                r#"
local issued = redis.call('HGET', KEYS[1], 'issued')
local completed = redis.call('HGET', KEYS[1], 'completed')
if issued and issued ~= '0' and issued == completed then return 1 end
return 0
"#,
            )
            .arg(1)
            .arg(format!("{session_key}:learning"))
            .query_async(&mut conn)
            .await?;
        Ok(result == 1)
    }

    pub(crate) async fn begin_learning_review(
        &self,
        session_key: &str,
        fence: &str,
        lease_ms: u64,
    ) -> redis::RedisResult<Option<u64>> {
        let mut conn = self.conn.clone();
        let issued: u64 = redis::cmd("EVAL").arg(r#"
if redis.call('EXISTS', KEYS[1]) == 0 then return 0 end
local issued = tonumber(redis.call('HGET', KEYS[2], 'issued') or '0')
local completed = tonumber(redis.call('HGET', KEYS[2], 'completed') or '0')
local clock = redis.call('TIME')
local now = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)
local until_time = tonumber(redis.call('HGET', KEYS[2], 'review_until') or '0')
if issued == 0 or issued ~= completed or until_time > now then return 0 end
redis.call('HSET', KEYS[2], 'review_token', ARGV[1], 'review_issued', issued, 'review_until', now + tonumber(ARGV[2]))
return issued
"#).arg(2).arg(session_key).arg(format!("{session_key}:learning")).arg(fence).arg(lease_ms)
            .query_async(&mut conn).await?;
        Ok((issued > 0).then_some(issued))
    }

    pub(crate) async fn seal_learning_review(
        &self,
        session_key: &str,
        fence: &str,
        issued: u64,
        review_id: &str,
        digest: &str,
    ) -> redis::RedisResult<bool> {
        let mut conn = self.conn.clone();
        let result: i64 = redis::cmd("EVAL").arg(r#"
local token = redis.call('HGET', KEYS[1], 'review_token')
local clock = redis.call('TIME')
local now = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)
local until_time = tonumber(redis.call('HGET', KEYS[1], 'review_until') or '0')
local issued = tonumber(redis.call('HGET', KEYS[1], 'issued') or '0')
local completed = tonumber(redis.call('HGET', KEYS[1], 'completed') or '0')
local captured = tonumber(redis.call('HGET', KEYS[1], 'review_issued') or '0')
if token ~= ARGV[1] or until_time <= now or issued ~= tonumber(ARGV[2]) or issued ~= captured or issued ~= completed then return 0 end
redis.call('HSET', KEYS[1], 'sealed:' .. ARGV[3], ARGV[4] .. ':' .. ARGV[2])
redis.call('HDEL', KEYS[1], 'review_token', 'review_until', 'review_issued')
return 1
"#).arg(1).arg(format!("{session_key}:learning")).arg(fence).arg(issued).arg(review_id).arg(digest)
            .query_async(&mut conn).await?;
        Ok(result == 1)
    }

    pub(crate) async fn abort_learning_review(
        &self,
        session_key: &str,
        fence: &str,
    ) -> redis::RedisResult<()> {
        let mut conn = self.conn.clone();
        let _: i64 = redis::cmd("EVAL")
            .arg(
                r#"
if redis.call('HGET', KEYS[1], 'review_token') == ARGV[1] then
    redis.call('HDEL', KEYS[1], 'review_token', 'review_until', 'review_issued')
end
return 1
"#,
            )
            .arg(1)
            .arg(format!("{session_key}:learning"))
            .arg(fence)
            .query_async(&mut conn)
            .await?;
        Ok(())
    }

    /// Publish exposure and its logical ledger together. Equal revisions may
    /// only refresh metadata when their symbol ledger is identical.
    pub(crate) async fn commit_session_exposure(
        &self,
        session_key: &str,
        ledger_key: Option<&str>,
        payload: &str,
        ledger: &[u8],
    ) -> redis::RedisResult<bool> {
        // Store receipt values as JSON strings so Redis cjson cannot turn
        // nested empty arrays in typed contracts into JSON objects.
        let descriptor: serde_json::Value = serde_json::from_str(payload).map_err(|error| {
            redis::RedisError::from((
                redis::ErrorKind::TypeError,
                "invalid session descriptor",
                error.to_string(),
            ))
        })?;
        let receipts: std::collections::BTreeMap<String, String> = descriptor
            .get("example_fetch_receipts")
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flat_map(|map| map.iter())
            .map(|(id, receipt)| (id.clone(), receipt.to_string()))
            .collect();
        let receipt_payload = serde_json::to_string(&receipts).map_err(|error| {
            redis::RedisError::from((
                redis::ErrorKind::TypeError,
                "invalid example receipts",
                error.to_string(),
            ))
        })?;
        let mut conn = self.conn.clone();
        let result: i64 = redis::cmd("EVAL")
            .arg(r#"
local incoming = cjson.decode(ARGV[1])
local old_raw = redis.call('GET', KEYS[1])
if old_raw then
    local old = cjson.decode(old_raw)
    if incoming.domain_revision < old.domain_revision then return 0 end
    if incoming.domain_revision > old.domain_revision + 1 and cjson.encode(incoming.symbol_ledger_bytes) ~= cjson.encode(old.symbol_ledger_bytes) then return 0 end
    if incoming.domain_revision == old.domain_revision then
        if cjson.encode(incoming.symbol_ledger_bytes) ~= cjson.encode(old.symbol_ledger_bytes) then return 0 end
    end
end
local ttl = tonumber(ARGV[3])
if not ttl or ttl < 1 then return redis.error_reply('invalid session ttl') end
local receipts_key = KEYS[3]
local previous_receipts = redis.call('GET', receipts_key)
local receipts = previous_receipts and cjson.decode(previous_receipts) or {}
for id, receipt in pairs(cjson.decode(ARGV[4])) do
    receipts[id] = receipt
end
local receipts_payload = cjson.encode(receipts)
redis.call('SET', KEYS[1], ARGV[1], 'EX', ttl)
if ARGV[5] == '1' then redis.call('SET', KEYS[2], ARGV[2], 'EX', ttl) end
redis.call('SET', receipts_key, receipts_payload, 'EX', ttl)
return 1
"#)
            .arg(3)
            .arg(session_key)
            .arg(ledger_key.unwrap_or(session_key))
            .arg(format!("{session_key}:example_receipts"))
            .arg(payload)
            .arg(ledger)
            .arg(self.ttl.as_secs().max(1))
            .arg(receipt_payload)
            .arg(if ledger_key.is_some() { "1" } else { "0" })
            .query_async(&mut conn)
            .await?;
        Ok(result == 1)
    }

    /// Atomically consume an authorization across replicas. Never a best-effort cache write.
    pub(crate) async fn claim_plan_authorization(
        &self,
        key: &str,
        ttl: u64,
    ) -> redis::RedisResult<Option<String>> {
        let mut conn = self.conn.clone();
        redis::cmd("EVAL")
            .arg("local old = redis.call('GET', KEYS[1]); if old then return old end; redis.call('SET', KEYS[1], ARGV[1], 'EX', ARGV[2]); return nil")
            .arg(1).arg(key).arg("\"running\"").arg(ttl)
            .query_async(&mut conn).await
    }

    pub(crate) async fn finish_plan_authorization(
        &self,
        key: &str,
        state: &str,
        ttl: u64,
    ) -> redis::RedisResult<()> {
        let mut conn = self.conn.clone();
        redis::cmd("SET")
            .arg(key)
            .arg(state)
            .arg("EX")
            .arg(ttl)
            .query_async(&mut conn)
            .await
    }
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

    /// Credential commits fail explicitly; ordinary best-effort cache helpers are unsuitable.
    pub(crate) async fn commit_credential_json(
        &self,
        operation_key: &str,
        reference_key: &str,
        payload: &str,
        now: u64,
        lifetime_seconds: u64,
    ) -> redis::RedisResult<String> {
        let mut conn = self.conn.clone();
        redis::cmd("EVAL")
            .arg(
                r#"
            local previous = redis.call('GET', KEYS[1])
            if previous then
                local record = cjson.decode(previous)
                local remaining = record.expires_at_unix - tonumber(ARGV[3])
                if remaining > 0 then
                    local reference_key = string.sub(KEYS[2], 1, -35) .. record.reference
                    redis.call('SET', reference_key, previous, 'EX', remaining)
                    return previous
                end
            end
            redis.call('MSET', KEYS[1], ARGV[1], KEYS[2], ARGV[1])
            redis.call('EXPIRE', KEYS[1], ARGV[2])
            redis.call('EXPIRE', KEYS[2], ARGV[2])
            return ARGV[1]
        "#,
            )
            .arg(2)
            .arg(operation_key)
            .arg(reference_key)
            .arg(payload)
            .arg(lifetime_seconds)
            .arg(now)
            .query_async(&mut conn)
            .await
    }

    pub(crate) async fn get_string_strict(&self, key: &str) -> redis::RedisResult<Option<String>> {
        let mut conn = self.conn.clone();
        conn.get(key).await
    }

    /// Session persistence must report failed writes and deletes to the protocol owner.
    pub(crate) async fn set_string_strict(
        &self,
        key: &str,
        payload: &str,
    ) -> redis::RedisResult<()> {
        let mut conn = self.conn.clone();
        conn.set_ex(key, payload, self.ttl.as_secs().max(60)).await
    }

    pub(crate) async fn delete_strict(&self, key: &str) -> redis::RedisResult<()> {
        let mut conn = self.conn.clone();
        conn.del(key).await
    }

    pub(crate) async fn touch_strict(&self, key: &str) -> redis::RedisResult<()> {
        let mut conn = self.conn.clone();
        conn.expire(key, self.ttl.as_secs().max(60) as i64).await
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
