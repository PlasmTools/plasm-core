//! Resilient HTTP transport: global/per-host concurrency, rate-limit detection, safe-method retries.

use crate::auth::ResolvedAuth;
use crate::error::RuntimeError;
use crate::execution::ExecutionConfig;
use crate::http_transport::{
    compiled_method_label, host_key_from_url, is_safe_http_method, join_base_url_path,
    HttpAttemptResult, HttpTransport, ReqwestHttpTransport,
};
use async_trait::async_trait;
use plasm_compile::CompiledRequest;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore};
use tracing::debug;

/// Retry and concurrency policy for outbound HTTP.
#[derive(Debug, Clone)]
pub struct HttpResiliencePolicy {
    pub global_max_inflight: usize,
    pub per_host_max_inflight: usize,
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub total_retry_budget: Duration,
}

impl From<&ExecutionConfig> for HttpResiliencePolicy {
    fn from(config: &ExecutionConfig) -> Self {
        Self {
            global_max_inflight: config.max_concurrent_requests.max(1),
            per_host_max_inflight: config.per_host_max_inflight.max(1),
            max_attempts: config.http_max_attempts.max(1),
            initial_backoff: Duration::from_millis(config.http_retry_initial_backoff_ms.max(1)),
            max_backoff: Duration::from_millis(config.http_retry_max_backoff_ms.max(1)),
            total_retry_budget: Duration::from_millis(config.http_retry_total_budget_ms.max(1)),
        }
    }
}

/// Decorator around [`ReqwestHttpTransport`] with semaphores and safe-method retries.
pub struct ResilientHttpTransport {
    inner: ReqwestHttpTransport,
    policy: HttpResiliencePolicy,
    global: Arc<Semaphore>,
    per_host: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl ResilientHttpTransport {
    pub fn new(inner: ReqwestHttpTransport, policy: HttpResiliencePolicy) -> Self {
        let global = Arc::new(Semaphore::new(policy.global_max_inflight));
        Self {
            inner,
            policy,
            global,
            per_host: Mutex::new(HashMap::new()),
        }
    }

    async fn host_semaphore(&self, host: &str) -> Arc<Semaphore> {
        let mut map = self.per_host.lock().await;
        map.entry(host.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(self.policy.per_host_max_inflight)))
            .clone()
    }

    fn compute_delay(&self, attempt: u32, retry_after: Option<Duration>, url: &str) -> Duration {
        let exp = self
            .policy
            .initial_backoff
            .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)) as u32);
        let base = exp.min(self.policy.max_backoff);
        let base = retry_after.map(|r| r.max(base)).unwrap_or(base);
        jitter_duration(base, url, attempt)
    }

    async fn process_attempt(
        &self,
        url: &str,
        method: &'static str,
        host: &str,
        attempt: u32,
        started: Instant,
        outcome: Result<HttpAttemptResult, RuntimeError>,
    ) -> Result<Option<(serde_json::Value, Option<String>)>, RuntimeError> {
        let safe = is_safe_http_method(method);
        let max = self.policy.max_attempts;

        match outcome {
            Ok(HttpAttemptResult::Success(json, link)) => Ok(Some((json, link))),
            Ok(HttpAttemptResult::Retryable {
                status,
                retry_after,
                message,
            }) => {
                if !safe || attempt >= max {
                    return Err(finalize_retryable_failure(
                        status,
                        host,
                        retry_after,
                        attempt,
                        message,
                    ));
                }
                if started.elapsed() >= self.policy.total_retry_budget {
                    return Err(finalize_retryable_failure(
                        status,
                        host,
                        retry_after,
                        attempt,
                        format!("{message} (retry budget exhausted)"),
                    ));
                }
                let delay = self.compute_delay(attempt, retry_after, url);
                crate::runtime_metrics::record_http_retry(status, delay);
                debug!(
                    target: "plasm_runtime::http_resilience",
                    method,
                    url = %url,
                    host = %host,
                    status,
                    attempt,
                    delay_ms = delay.as_millis(),
                    "retrying outbound HTTP"
                );
                tokio::time::sleep(delay).await;
                Ok(None)
            }
            Ok(HttpAttemptResult::Failed(mut e)) => {
                e.set_attempts(attempt);
                Err(e)
            }
            Err(mut e) => {
                if safe
                    && attempt < max
                    && started.elapsed() < self.policy.total_retry_budget
                    && transport_error_is_retryable(&e)
                {
                    let delay = self.compute_delay(attempt, None, url);
                    crate::runtime_metrics::record_http_retry(0, delay);
                    debug!(
                        target: "plasm_runtime::http_resilience",
                        method,
                        url = %url,
                        host = %host,
                        attempt,
                        delay_ms = delay.as_millis(),
                        error = %e,
                        "retrying outbound HTTP after transport error"
                    );
                    tokio::time::sleep(delay).await;
                    Ok(None)
                } else {
                    e.set_attempts(attempt);
                    Err(e)
                }
            }
        }
    }
}

fn finalize_retryable_failure(
    status: u16,
    host: &str,
    retry_after: Option<Duration>,
    attempts: u32,
    message: String,
) -> RuntimeError {
    if status == 429 {
        crate::runtime_metrics::record_http_rate_limited();
        RuntimeError::RateLimited {
            status,
            host: host.to_string(),
            retry_after,
            attempts,
            message,
        }
    } else {
        RuntimeError::RequestError { message, attempts }
    }
}

fn transport_error_is_retryable(err: &RuntimeError) -> bool {
    match err {
        RuntimeError::RequestError { message, .. } => {
            message.contains("timeout")
                || message.contains("timed out")
                || message.contains("connection")
                || message.contains("dns")
                || message.contains("connect")
        }
        _ => false,
    }
}

/// Full jitter: `delay in [base/2, base]` seeded by url + attempt.
fn jitter_duration(base: Duration, url: &str, attempt: u32) -> Duration {
    let base_ms = base.as_millis().min(u128::from(u64::MAX)) as u64;
    if base_ms == 0 {
        return Duration::ZERO;
    }
    let mut h = 0u64;
    for b in url.bytes() {
        h = h.wrapping_mul(31).wrapping_add(u64::from(b));
    }
    h = h.wrapping_add(u64::from(attempt).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let spread = base_ms / 2;
    let extra = if spread == 0 { 0 } else { h % spread };
    Duration::from_millis(base_ms / 2 + extra)
}

#[async_trait]
impl HttpTransport for ResilientHttpTransport {
    async fn send_compiled_http(
        &self,
        base_url: &str,
        request: &CompiledRequest,
        auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let url = join_base_url_path(base_url, request.url_path());
        let method = compiled_method_label(&request.method);
        let started = Instant::now();
        let host = host_key_from_url(&url);
        let host_sem = self.host_semaphore(&host).await;
        let _global =
            self.global
                .acquire()
                .await
                .map_err(|_| RuntimeError::ConfigurationError {
                    message: "global HTTP concurrency semaphore closed".to_string(),
                })?;
        let _host = host_sem
            .acquire()
            .await
            .map_err(|_| RuntimeError::ConfigurationError {
                message: format!("per-host HTTP semaphore closed for {host}"),
            })?;

        let mut attempt = 0u32;
        let result = loop {
            attempt += 1;
            let outcome = self
                .inner
                .compiled_http_attempt(base_url, request, auth.clone())
                .await;
            match self
                .process_attempt(&url, method, &host, attempt, started, outcome)
                .await
            {
                Ok(Some(ok)) => break Ok(ok),
                Ok(None) => continue,
                Err(e) => break Err(e),
            }
        };
        crate::runtime_metrics::record_outbound_http_request(
            method,
            &url,
            result.is_ok(),
            started.elapsed(),
        );
        result
    }

    async fn get_json_absolute(
        &self,
        url: &str,
        auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let started = Instant::now();
        let host = host_key_from_url(url);
        let host_sem = self.host_semaphore(&host).await;
        let _global =
            self.global
                .acquire()
                .await
                .map_err(|_| RuntimeError::ConfigurationError {
                    message: "global HTTP concurrency semaphore closed".to_string(),
                })?;
        let _host = host_sem
            .acquire()
            .await
            .map_err(|_| RuntimeError::ConfigurationError {
                message: format!("per-host HTTP semaphore closed for {host}"),
            })?;

        let mut attempt = 0u32;
        let result = loop {
            attempt += 1;
            let outcome = self.inner.absolute_get_attempt(url, auth.clone()).await;
            match self
                .process_attempt(url, "GET", &host, attempt, started, outcome)
                .await
            {
                Ok(Some(ok)) => break Ok(ok),
                Ok(None) => continue,
                Err(e) => break Err(e),
            }
        };
        crate::runtime_metrics::record_outbound_http_request(
            "GET",
            url,
            result.is_ok(),
            started.elapsed(),
        );
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_within_band() {
        let d = jitter_duration(Duration::from_millis(1000), "https://api.example.com/x", 2);
        assert!(d >= Duration::from_millis(500));
        assert!(d <= Duration::from_millis(1000));
    }

    #[test]
    fn host_key_normalizes() {
        assert_eq!(
            host_key_from_url("https://API.GitHub.com/repos"),
            "api.github.com"
        );
    }

    #[test]
    fn safe_method_detection() {
        assert!(is_safe_http_method("GET"));
        assert!(!is_safe_http_method("POST"));
    }
}
