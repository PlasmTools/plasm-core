//! Resilient HTTP transport: global/per-host concurrency, rate-limit detection, safe-method retries.

use crate::auth::ResolvedAuth;
use crate::error::RuntimeError;
use crate::execution::ExecutionConfig;
use crate::http_trace::HttpTraceOutcome;
use crate::http_transport::{
    compiled_method_label, host_key_from_url, http_retryable_is_rate_limited, is_safe_http_method,
    join_base_url_path, HttpAttemptResult, HttpTransport,
};
use async_trait::async_trait;
use plasm_compile::CompiledRequest;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore, SemaphorePermit};
use tracing::{debug, Instrument};

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

/// Decorator around any [`HttpTransport`] with semaphores and safe-method retries.
///
/// Default engine construction wraps [`ReqwestHttpTransport`]. NAPI / test
/// engines that inject a custom client via [`crate::ExecutionEngine::new_with_transport`]
/// receive the same GET/HEAD/OPTIONS retry law.
pub struct ResilientHttpTransport {
    inner: Arc<dyn HttpTransport>,
    policy: HttpResiliencePolicy,
    global: Arc<Semaphore>,
    per_host: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl ResilientHttpTransport {
    pub fn new(inner: impl HttpTransport + 'static, policy: HttpResiliencePolicy) -> Self {
        Self::wrap(Arc::new(inner), policy)
    }

    pub fn wrap(inner: Arc<dyn HttpTransport>, policy: HttpResiliencePolicy) -> Self {
        let global = Arc::new(Semaphore::new(policy.global_max_inflight));
        Self {
            inner,
            policy,
            global,
            per_host: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn semaphore_acquire_timeout() -> Duration {
        static TIMEOUT: OnceLock<Duration> = OnceLock::new();
        *TIMEOUT.get_or_init(|| {
            std::env::var("PLASM_HTTP_SEMAPHORE_ACQUIRE_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|ms| *ms > 0)
                .map(Duration::from_millis)
                .unwrap_or(Duration::from_secs(30))
        })
    }

    async fn acquire_permit<'a>(
        sem: &'a Semaphore,
        label: &str,
    ) -> Result<SemaphorePermit<'a>, RuntimeError> {
        Self::acquire_permit_with_timeout(sem, label, Self::semaphore_acquire_timeout()).await
    }

    async fn acquire_permit_with_timeout<'a>(
        sem: &'a Semaphore,
        label: &str,
        timeout: Duration,
    ) -> Result<SemaphorePermit<'a>, RuntimeError> {
        let host = label.to_string();
        tokio::time::timeout(timeout, sem.acquire())
            .await
            .map_err(|_| RuntimeError::RateLimited {
                status: 429,
                host,
                retry_after: Some(timeout),
                attempts: 0,
                message: format!("HTTP concurrency queue timeout waiting for {label}"),
            })?
            .map_err(|_| RuntimeError::ConfigurationError {
                message: format!("HTTP concurrency semaphore closed ({label})"),
            })
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
            .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)));
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

    async fn run_with_retries<F, Fut>(
        &self,
        url: &str,
        method: &'static str,
        host: &str,
        started: Instant,
        mut attempt_fn: F,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<HttpAttemptResult, RuntimeError>>,
    {
        let mut attempt = 0u32;
        async {
            loop {
                attempt += 1;
                tracing::Span::current().record("attempt", attempt);
                let outcome = attempt_fn().await;
                match self
                    .process_attempt(url, method, host, attempt, started, outcome)
                    .await
                {
                    Ok(Some(ok)) => break Ok(ok),
                    Ok(None) => continue,
                    Err(e) => break Err(e),
                }
            }
        }
        .instrument(crate::spans::http_retry())
        .await
    }
}

fn finalize_retryable_failure(
    status: u16,
    host: &str,
    retry_after: Option<Duration>,
    attempts: u32,
    message: String,
) -> RuntimeError {
    if http_retryable_is_rate_limited(status, retry_after, &message) {
        crate::runtime_metrics::record_http_rate_limited();
        RuntimeError::RateLimited {
            status,
            host: host.to_string(),
            retry_after,
            attempts,
            message,
        }
    } else {
        RuntimeError::RequestError {
            message,
            attempts,
            status: None,
            body: None,
        }
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
    fn injects_host_auth(&self) -> bool {
        self.inner.injects_host_auth()
    }

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
        let _global = Self::acquire_permit(&self.global, "global HTTP concurrency").await?;
        let _host = Self::acquire_permit(
            host_sem.as_ref(),
            &format!("per-host HTTP concurrency ({host})"),
        )
        .await?;

        let result = self
            .run_with_retries(&url, method, &host, started, || {
                self.inner
                    .compiled_http_attempt(base_url, request, auth.clone())
            })
            .await;
        let elapsed = started.elapsed();
        crate::runtime_metrics::record_outbound_http_request(method, &url, result.is_ok(), elapsed);
        crate::live_run_telemetry::record_live_http_trace(
            method,
            &url,
            elapsed,
            http_trace_outcome(&result),
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
        let _global = Self::acquire_permit(&self.global, "global HTTP concurrency").await?;
        let _host = Self::acquire_permit(
            host_sem.as_ref(),
            &format!("per-host HTTP concurrency ({host})"),
        )
        .await?;

        let result = self
            .run_with_retries(url, "GET", &host, started, || {
                self.inner.absolute_get_attempt(url, auth.clone())
            })
            .await;
        let elapsed = started.elapsed();
        crate::runtime_metrics::record_outbound_http_request("GET", url, result.is_ok(), elapsed);
        crate::live_run_telemetry::record_live_http_trace(
            "GET",
            url,
            elapsed,
            http_trace_outcome(&result),
        );
        result
    }
}

fn http_trace_outcome<T, E: std::fmt::Display>(result: &Result<T, E>) -> HttpTraceOutcome {
    match result {
        Ok(_) => HttpTraceOutcome::Ok,
        Err(e) => HttpTraceOutcome::Error {
            message: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_transport::{HttpTransport, ReqwestHttpTransport};
    use futures_util::future::join_all;
    use indexmap::IndexMap;
    use plasm_compile::{CompiledRequest, HttpBodyFormat, HttpMethod};
    use plasm_core::Value;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const SHARED_BEARER: &str = "Bearer shared-search-token";
    const FANOUT_N: usize = 8;

    #[derive(Clone, Copy)]
    enum MockMode {
        AlwaysOk,
        First500ThenOk,
        Always401,
        HoldMs(u64),
    }

    struct MockStats {
        inflight: AtomicUsize,
        peak: AtomicUsize,
        hits: AtomicUsize,
        auths: Mutex<Vec<Option<String>>>,
        per_target: Mutex<HashMap<String, usize>>,
    }

    impl MockStats {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                inflight: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                hits: AtomicUsize::new(0),
                auths: Mutex::new(Vec::new()),
                per_target: Mutex::new(HashMap::new()),
            })
        }
    }

    fn header_value(req: &str, name: &str) -> Option<String> {
        req.lines()
            .skip(1)
            .take_while(|l| !l.is_empty())
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case(name)
                    .then(|| value.trim().to_string())
            })
    }

    fn request_target(req: &str) -> String {
        req.lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string()
    }

    fn http_reply(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    async fn read_http_head(stream: &mut tokio::net::TcpStream) -> std::io::Result<String> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
            if buf.len() > 16 * 1024 {
                break;
            }
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    async fn spawn_search_fanout_mock(mode: MockMode) -> (String, Arc<MockStats>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("addr");
        let stats = MockStats::new();
        let serve_stats = Arc::clone(&stats);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let stats = Arc::clone(&serve_stats);
                tokio::spawn(async move {
                    let Ok(req) = read_http_head(&mut stream).await else {
                        return;
                    };
                    let target = request_target(&req);
                    let auth = header_value(&req, "Authorization");
                    stats.hits.fetch_add(1, Ordering::SeqCst);
                    {
                        let mut auths = stats.auths.lock().await;
                        auths.push(auth);
                    }
                    let attempt = {
                        let mut map = stats.per_target.lock().await;
                        let n = map.entry(target).or_insert(0);
                        *n += 1;
                        *n
                    };
                    let cur = stats.inflight.fetch_add(1, Ordering::SeqCst) + 1;
                    stats.peak.fetch_max(cur, Ordering::SeqCst);
                    if let MockMode::HoldMs(ms) = mode {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                    }
                    stats.inflight.fetch_sub(1, Ordering::SeqCst);
                    let reply = match mode {
                        MockMode::AlwaysOk | MockMode::HoldMs(_) => {
                            http_reply("200 OK", r#"{"ok":true}"#)
                        }
                        MockMode::First500ThenOk if attempt == 1 => {
                            http_reply("500 Internal Server Error", "Internal Server Error")
                        }
                        MockMode::First500ThenOk => http_reply("200 OK", r#"{"ok":true}"#),
                        MockMode::Always401 => http_reply(
                            "401 Unauthorized",
                            r#"{"message":"access token is missing, invalid or expired"}"#,
                        ),
                    };
                    let _ = stream.write_all(reply.as_bytes()).await;
                });
            }
        });
        (format!("http://{addr}"), stats)
    }

    fn search_shaped_get(slot: usize) -> CompiledRequest {
        let mut query = IndexMap::new();
        query.insert("q".into(), Value::String(format!("slot-{slot}")));
        let headers = Value::Object(IndexMap::from([(
            "Authorization".into(),
            Value::String(SHARED_BEARER.into()),
        )]));
        CompiledRequest {
            credential: None,
            method: HttpMethod::Get,
            path: "/records".into(),
            query: Some(Value::Object(query)),
            body: None,
            body_format: HttpBodyFormat::Json,
            multipart: None,
            headers: Some(headers),
        }
    }

    fn fast_retry_policy(per_host: usize) -> HttpResiliencePolicy {
        HttpResiliencePolicy {
            global_max_inflight: 64,
            per_host_max_inflight: per_host,
            max_attempts: 4,
            initial_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(20),
            total_retry_budget: Duration::from_secs(5),
        }
    }

    async fn fanout_compiled_gets(
        transport: &ResilientHttpTransport,
        base: &str,
        n: usize,
    ) -> Vec<Result<(serde_json::Value, Option<String>), RuntimeError>> {
        let jobs: Vec<_> = (0..n)
            .map(|i| {
                let request = search_shaped_get(i);
                async move { transport.send_compiled_http(base, &request, None).await }
            })
            .collect();
        join_all(jobs).await
    }

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

    #[tokio::test]
    async fn semaphore_acquire_times_out_when_no_permits() {
        let sem = Arc::new(Semaphore::new(1));
        let _held = sem.acquire().await.expect("hold sole permit");
        let started = Instant::now();
        let err = ResilientHttpTransport::acquire_permit_with_timeout(
            sem.as_ref(),
            "test-global",
            Duration::from_millis(50),
        )
        .await
        .expect_err("must timeout");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "queue wait should fail fast"
        );
        match err {
            RuntimeError::RateLimited { message, .. } => {
                assert!(message.contains("HTTP concurrency queue timeout"));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fanout_preserves_shared_bearer_on_every_get() {
        let (base, stats) = spawn_search_fanout_mock(MockMode::AlwaysOk).await;
        let transport = ResilientHttpTransport::new(
            ReqwestHttpTransport::new(reqwest::Client::new()),
            fast_retry_policy(24),
        );
        let results = fanout_compiled_gets(&transport, &base, FANOUT_N).await;
        assert!(
            results.iter().all(|r| r.is_ok()),
            "shared-bearer fanout must succeed: {results:?}"
        );
        let auths = stats.auths.lock().await;
        assert_eq!(auths.len(), FANOUT_N);
        assert!(
            auths.iter().all(|h| h.as_deref() == Some(SHARED_BEARER)),
            "Authorization must be present and identical on every fanout GET: {auths:?}"
        );
        assert_eq!(stats.hits.load(Ordering::SeqCst), FANOUT_N);
    }

    #[tokio::test]
    async fn fanout_retries_get_500_then_succeeds_with_stable_bearer() {
        let (base, stats) = spawn_search_fanout_mock(MockMode::First500ThenOk).await;
        let transport = ResilientHttpTransport::new(
            ReqwestHttpTransport::new(reqwest::Client::new()),
            fast_retry_policy(24),
        );
        let results = fanout_compiled_gets(&transport, &base, FANOUT_N).await;
        assert!(
            results.iter().all(|r| r.is_ok()),
            "GET 500 must retry on safe methods: {results:?}"
        );
        let auths = stats.auths.lock().await;
        assert_eq!(auths.len(), FANOUT_N * 2, "one 500 then one 200 per slot");
        assert!(
            auths.iter().all(|h| h.as_deref() == Some(SHARED_BEARER)),
            "retry must resend the same Authorization: {auths:?}"
        );
        assert_eq!(stats.hits.load(Ordering::SeqCst), FANOUT_N * 2);
    }

    #[tokio::test]
    async fn fanout_401_is_terminal_and_authorization_was_sent() {
        let (base, stats) = spawn_search_fanout_mock(MockMode::Always401).await;
        let transport = ResilientHttpTransport::new(
            ReqwestHttpTransport::new(reqwest::Client::new()),
            fast_retry_policy(24),
        );
        let results = fanout_compiled_gets(&transport, &base, FANOUT_N).await;
        assert!(
            results.iter().all(|r| r.is_err()),
            "401 must stay terminal: {results:?}"
        );
        let auths = stats.auths.lock().await;
        assert_eq!(
            auths.len(),
            FANOUT_N,
            "401 must not be retried (hits would be 4N)"
        );
        assert!(
            auths.iter().all(|h| h.as_deref() == Some(SHARED_BEARER)),
            "401 is backend; client must still send Authorization: {auths:?}"
        );
    }

    #[tokio::test]
    async fn fanout_respects_per_host_inflight_cap() {
        let (base, stats) = spawn_search_fanout_mock(MockMode::HoldMs(40)).await;
        let transport = ResilientHttpTransport::new(
            ReqwestHttpTransport::new(reqwest::Client::new()),
            fast_retry_policy(2),
        );
        let results = fanout_compiled_gets(&transport, &base, FANOUT_N).await;
        assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
        let peak = stats.peak.load(Ordering::SeqCst);
        assert!(
            peak <= 2,
            "per-host inflight cap must hold under fanout, peak={peak}"
        );
        assert!(peak >= 1, "mock must observe at least one in-flight GET");
    }

    struct SequenceInner {
        hits: AtomicUsize,
        fail_status: u16,
    }

    #[async_trait]
    impl HttpTransport for SequenceInner {
        async fn send_compiled_http(
            &self,
            _base_url: &str,
            _request: &CompiledRequest,
            _auth: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            let n = self.hits.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                return Err(RuntimeError::RequestError {
                    message: format!("HTTP {}", self.fail_status),
                    attempts: 1,
                    status: Some(self.fail_status),
                    body: None,
                });
            }
            Ok((serde_json::json!({ "ok": true }), None))
        }

        async fn get_json_absolute(
            &self,
            _url: &str,
            _auth: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            Err(RuntimeError::ConfigurationError {
                message: "absolute GET unused".into(),
            })
        }
    }

    fn post_search_shaped() -> CompiledRequest {
        let mut req = search_shaped_get(0);
        req.method = HttpMethod::Post;
        req
    }

    #[tokio::test]
    async fn custom_transport_get_500_then_200_is_retried() {
        let inner = Arc::new(SequenceInner {
            hits: AtomicUsize::new(0),
            fail_status: 500,
        });
        let transport = ResilientHttpTransport::wrap(inner.clone(), fast_retry_policy(24));
        let started = Instant::now();
        let result = transport
            .send_compiled_http("https://example.test", &search_shaped_get(0), None)
            .await;
        assert!(
            result.is_ok(),
            "NAPI-shaped GET 500 must retry on the shared decorator: {result:?}"
        );
        assert_eq!(inner.hits.load(Ordering::SeqCst), 2);
        assert!(
            started.elapsed() >= Duration::from_millis(2),
            "retry must sleep; elapsed={:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn custom_transport_post_500_is_terminal() {
        let inner = Arc::new(SequenceInner {
            hits: AtomicUsize::new(0),
            fail_status: 500,
        });
        let transport = ResilientHttpTransport::wrap(inner.clone(), fast_retry_policy(24));
        let result = transport
            .send_compiled_http("https://example.test", &post_search_shaped(), None)
            .await;
        assert!(result.is_err(), "POST 500 must stay terminal: {result:?}");
        assert_eq!(inner.hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn new_with_transport_retries_custom_get_500() {
        let inner = Arc::new(SequenceInner {
            hits: AtomicUsize::new(0),
            fail_status: 500,
        });
        let engine = crate::ExecutionEngine::new_with_transport(
            crate::ExecutionConfig {
                http_retry_initial_backoff_ms: 5,
                http_retry_max_backoff_ms: 20,
                http_max_attempts: 4,
                http_retry_total_budget_ms: 5_000,
                ..crate::ExecutionConfig::default()
            },
            inner.clone(),
            None,
        );
        let result = engine
            .transport
            .send_compiled_http("https://example.test", &search_shaped_get(0), None)
            .await;
        assert!(
            result.is_ok(),
            "new_with_transport must wrap resilience: {result:?}"
        );
        assert_eq!(inner.hits.load(Ordering::SeqCst), 2);
    }
}
