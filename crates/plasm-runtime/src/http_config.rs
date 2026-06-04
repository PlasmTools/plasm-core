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
