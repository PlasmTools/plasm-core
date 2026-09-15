//! Environment overrides for outbound HTTP resilience ([`crate::execution::ExecutionConfig`]).

use crate::execution::ExecutionConfig;

fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|&n| n > 0)
}

fn env_u32(key: &str) -> Option<u32> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|&n| n > 0)
}

fn env_u64_ms(key: &str) -> Option<u64> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|&n| n > 0)
}

impl ExecutionConfig {
    /// Apply optional `PLASM_HTTP_*` environment overrides (invalid / zero values ignored).
    pub fn apply_http_env_overrides(&mut self) {
        if let Some(n) = env_usize("PLASM_HTTP_MAX_INFLIGHT") {
            self.max_concurrent_requests = n;
        }
        if let Some(n) = env_usize("PLASM_HTTP_PER_HOST_MAX_INFLIGHT") {
            self.per_host_max_inflight = n;
        }
        if let Some(n) = env_usize("PLASM_HTTP_HYDRATE_CONCURRENCY") {
            self.hydrate_concurrency = n;
        }
        if let Some(map) = parse_backend_max_inflight_env() {
            self.backend_max_inflight = map;
        }
        if let Some(n) = env_u32("PLASM_HTTP_MAX_ATTEMPTS") {
            self.http_max_attempts = n;
        }
        if let Some(n) = env_u64_ms("PLASM_HTTP_RETRY_INITIAL_MS") {
            self.http_retry_initial_backoff_ms = n;
        }
        if let Some(n) = env_u64_ms("PLASM_HTTP_RETRY_MAX_MS") {
            self.http_retry_max_backoff_ms = n;
        }
        if let Some(n) = env_u64_ms("PLASM_HTTP_RETRY_BUDGET_MS") {
            self.http_retry_total_budget_ms = n;
        }
    }
}

/// Parse `PLASM_HTTP_BACKEND_MAX_INFLIGHT=simple_note=1,github=4` (comma-separated `entry_id=N`).
fn parse_backend_max_inflight_env() -> Option<std::collections::HashMap<String, usize>> {
    let raw = std::env::var("PLASM_HTTP_BACKEND_MAX_INFLIGHT").ok()?;
    let mut map = std::collections::HashMap::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((id, nraw)) = part.split_once('=').or_else(|| part.split_once(':')) else {
            continue;
        };
        let id = id.trim();
        let Ok(n) = nraw.trim().parse::<usize>() else {
            continue;
        };
        if id.is_empty() || n == 0 {
            continue;
        }
        map.insert(id.to_string(), n);
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

#[cfg(test)]
mod tests {
    use crate::execution::ExecutionConfig;

    #[test]
    fn effective_hydrate_concurrency_respects_backend_cap() {
        let mut cfg = ExecutionConfig::default();
        cfg.hydrate_concurrency = 16;
        cfg.backend_max_inflight
            .insert("simple_note".into(), 1);
        assert_eq!(cfg.effective_hydrate_concurrency(Some("simple_note")), 1);
        assert_eq!(cfg.effective_hydrate_concurrency(Some("github")), 16);
        assert_eq!(cfg.effective_hydrate_concurrency(None), 16);
    }
}
