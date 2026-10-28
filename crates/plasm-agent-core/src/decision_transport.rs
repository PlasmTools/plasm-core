//! Bounded transport for repeatable, read-only model judgments. Never retries API effects.
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

type Result<T> = std::result::Result<T, DecisionTransportError>;

#[derive(Debug, Error)]
pub enum DecisionTransportError {
    #[error("decision transport attempt budget must be positive")]
    EmptyAttemptBudget,
    #[error("decision context limit rejected this page")]
    ContextLimit,
    #[error("decision provider returned HTTP {status}: {message}")]
    ProviderRejected {
        status: reqwest::StatusCode,
        message: String,
        retry_after: Option<Duration>,
    },
    #[error("decision response body could not be read")]
    ResponseBody {
        status: reqwest::StatusCode,
        retry_after: Option<Duration>,
        #[source]
        source: reqwest::Error,
    },
    #[error("decision transport failed")]
    Request(#[source] reqwest::Error),
    #[error("decision transport exhausted {attempts} attempts: {source}")]
    RetryExhausted {
        attempts: usize,
        #[source]
        source: Box<DecisionTransportError>,
    },
    #[error("decision retry deadline expired")]
    DeadlineExpired,
    #[error(transparent)]
    Record(#[from] DecisionRecordError),
}

impl DecisionTransportError {
    /// Retry only transport/provider failures, never decoding or persistence.
    /// The caller still owns attempt admission and the overall deadline.
    pub(crate) fn retry_delay(&self, attempt: usize) -> Option<Duration> {
        let after = match self {
            Self::ProviderRejected {
                status,
                retry_after,
                ..
            } if transient(status.as_u16()) => *retry_after,
            Self::Request(error) if request_retryable(error) => None,
            Self::ResponseBody {
                status,
                retry_after,
                ..
            } if body_retryable(*status) => *retry_after,
            _ => return None,
        };
        Some(retry_wait(attempt, Duration::from_millis(500), after))
    }
}

fn request_retryable(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_body() || error.is_request()
}

fn body_retryable(status: reqwest::StatusCode) -> bool {
    status.is_success() || transient(status.as_u16())
}

fn retry_wait(attempt: usize, backoff: Duration, after: Option<Duration>) -> Duration {
    let entropy = uuid::Uuid::new_v4().as_u128() as u64;
    let backoff = backoff.saturating_mul(1u32 << attempt.saturating_sub(1).min(10));
    after
        .unwrap_or_default()
        .max(backoff + Duration::from_millis(entropy % 251))
}

#[derive(Debug, Error)]
pub enum DecisionRecordError {
    #[error("decision attempt record JSON encoding failed")]
    Json(#[from] serde_json::Error),
    #[error("decision attempt record could not be persisted")]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Copy)]
pub(crate) struct DecisionRetryPolicy {
    pub attempts: usize,
    pub deadline: Duration,
    pub attempt_timeout: Duration,
    pub backoff: Duration,
}

impl Default for DecisionRetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 5,
            deadline: Duration::from_secs(300),
            attempt_timeout: Duration::from_secs(120),
            backoff: Duration::from_millis(500),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum DecisionAttempt {
    Response {
        attempt: usize,
        http_status: u16,
        raw_response: String,
    },
    Transport {
        attempt: usize,
        error: String,
    },
}

/// Provider-authoritative context rejection, distinct from transient transport loss.
fn context_limit(status: u16, raw: &str) -> bool {
    #[derive(Deserialize)]
    struct ProviderError {
        detail: Detail,
    }
    #[derive(Deserialize)]
    struct Detail {
        error_type: String,
    }
    #[derive(Deserialize)]
    struct RouterError {
        error: RouterDetail,
    }
    #[derive(Deserialize)]
    struct RouterDetail {
        code: u16,
        message: String,
        metadata: Option<RouterMetadata>,
    }
    #[derive(Deserialize)]
    struct RouterMetadata {
        error_type: Option<String>,
    }
    fn provider(raw: &str) -> bool {
        serde_json::from_str::<ProviderError>(raw)
            .is_ok_and(|e| e.detail.error_type == "max_tokens_exceeded")
    }
    // OpenRouter chat errors use error.metadata.error_type, including errors
    // delivered after HTTP 200 was committed. Never classify message prose.
    // https://openrouter.ai/docs/api_reference/errors-and-debugging
    if matches!(status, 200 | 400)
        && serde_json::from_str::<RouterError>(raw).is_ok_and(|e| {
            e.error.code == 400
                && e.error.metadata.is_some_and(|metadata| {
                    metadata.error_type.as_deref() == Some("context_length_exceeded")
                })
        })
    {
        return true;
    }
    status == 400
        && (provider(raw)
            || serde_json::from_str::<RouterError>(raw).is_ok_and(|e| {
                e.error.code == 400
                    && e.error
                        .message
                        .strip_prefix("HTTP 400: ")
                        .is_some_and(provider)
            }))
}

fn transient(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500 | 502..=504 | 520..=524 | 529)
}

// Retain only the provider's structured message, never an arbitrary HTML body.
fn provider_message(raw: &str) -> String {
    #[derive(Deserialize)]
    struct Envelope {
        error: Detail,
    }
    #[derive(Deserialize)]
    struct Detail {
        message: String,
    }
    serde_json::from_str::<Envelope>(raw)
        .ok()
        .map(|e| {
            e.error
                .message
                .chars()
                .take(512)
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect()
        })
        .unwrap_or_else(|| "no structured provider message; inspect the recorded response".into())
}

#[cfg(test)]
#[test]
fn provider_messages_are_structured_bounded_and_unicode_safe() {
    let body = serde_json::json!({"error":{"message":"é\n".repeat(600)}}).to_string();
    let message = provider_message(&body);
    assert_eq!(message.chars().count(), 512);
    assert!(!message.contains('\n'));
    assert!(!provider_message("<html>unstructured body</html>").contains("<html>"));
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    Some(
        (at.with_timezone(&chrono::Utc) - chrono::Utc::now())
            .to_std()
            .unwrap_or_default(),
    )
}

pub(crate) async fn request_decision(
    client: &reqwest::Client,
    endpoint: &str,
    key: &str,
    body: &str,
    policy: DecisionRetryPolicy,
    mut record: impl FnMut(&DecisionAttempt) -> std::result::Result<(), DecisionRecordError>,
) -> Result<String> {
    if policy.attempts == 0 {
        return Err(DecisionTransportError::EmptyAttemptBudget);
    }
    let operation = async {
        for attempt in 1..=policy.attempts {
            let response = client
                .post(endpoint)
                .bearer_auth(key)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_owned())
                .timeout(policy.attempt_timeout)
                .send()
                .await;
            let (retry, delay, failure, terminal_error) = match response {
                Ok(response) => {
                    let status = response.status();
                    let delay = retry_after(response.headers());
                    match response.text().await {
                        Ok(raw) => {
                            record(&DecisionAttempt::Response {
                                attempt,
                                http_status: status.as_u16(),
                                raw_response: raw.clone(),
                            })?;
                            if context_limit(status.as_u16(), &raw) {
                                return Err(DecisionTransportError::ContextLimit);
                            }
                            if status.is_success() {
                                return Ok(raw);
                            }
                            (
                                transient(status.as_u16()),
                                delay,
                                format!("Decision provider HTTP {status}"),
                                DecisionTransportError::ProviderRejected {
                                    status,
                                    message: provider_message(&raw),
                                    retry_after: delay,
                                },
                            )
                        }
                        Err(error) => {
                            record(&DecisionAttempt::Transport {
                                attempt,
                                error: error.to_string(),
                            })?;
                            (
                                body_retryable(status),
                                delay,
                                error.to_string(),
                                DecisionTransportError::ResponseBody {
                                    status,
                                    retry_after: delay,
                                    source: error,
                                },
                            )
                        }
                    }
                }
                Err(error) => {
                    record(&DecisionAttempt::Transport {
                        attempt,
                        error: error.to_string(),
                    })?;
                    (
                        request_retryable(&error),
                        None,
                        error.to_string(),
                        DecisionTransportError::Request(error),
                    )
                }
            };
            if !retry || attempt == policy.attempts {
                return Err(if retry && attempt > 1 {
                    DecisionTransportError::RetryExhausted {
                        attempts: attempt,
                        source: Box::new(terminal_error),
                    }
                } else {
                    terminal_error
                });
            }
            // Jitter each operation independently; Retry-After remains a lower bound.
            let wait = retry_wait(attempt, policy.backoff, delay);
            tracing::warn!(
                attempt,
                wait_ms = wait.as_millis(),
                failure,
                "retrying read-only decision request"
            );
            tokio::time::sleep(wait).await;
        }
        Err(DecisionTransportError::EmptyAttemptBudget)
    };
    tokio::time::timeout(policy.deadline, operation)
        .await
        .map_err(|_| DecisionTransportError::DeadlineExpired)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    async fn exercise(
        statuses: Vec<u16>,
        policy: DecisionRetryPolicy,
    ) -> (Result<String>, Vec<String>, Vec<DecisionAttempt>) {
        exercise_body(statuses, "judgment", policy).await
    }

    async fn exercise_body(
        statuses: Vec<u16>,
        raw: &'static str,
        policy: DecisionRetryPolicy,
    ) -> (Result<String>, Vec<String>, Vec<DecisionAttempt>) {
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured = requests.clone();
        let router = axum::Router::new().route(
            "/",
            axum::routing::post(move |body: String| {
                let captured = captured.clone();
                let statuses = statuses.clone();
                async move {
                    let mut seen = captured.lock().unwrap();
                    let status = statuses[seen.len().min(statuses.len() - 1)];
                    seen.push(body);
                    (axum::http::StatusCode::from_u16(status).unwrap(), raw)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut attempts = Vec::new();
        let result = request_decision(
            &reqwest::Client::new(),
            &endpoint,
            "test",
            "exact request",
            policy,
            |r| {
                attempts.push(r.clone());
                Ok(())
            },
        )
        .await;
        server.abort();
        let seen = requests.lock().unwrap().clone();
        (result, seen, attempts)
    }

    #[tokio::test]
    async fn context_rejection_is_typed_and_never_retries_the_same_page() {
        let (result, requests, attempts) = exercise_body(vec![400, 200],
            r#"{"error":{"code":400,"message":"HTTP 400: {\"detail\":{\"error_type\":\"max_tokens_exceeded\"}}"}}"#,
            DecisionRetryPolicy::default()).await;
        assert!(matches!(
            result.unwrap_err(),
            DecisionTransportError::ContextLimit
        ));
        assert_eq!(requests.len(), 1);
        assert_eq!(attempts.len(), 1);
    }

    #[test]
    fn context_limit_requires_the_structured_provider_code() {
        let provider = r#"{"detail":{"error_type":"max_tokens_exceeded"}}"#;
        let routed =
            serde_json::json!({"error":{"code":400,"message":format!("HTTP 400: {provider}")}})
                .to_string();
        assert!(context_limit(400, provider));
        assert!(context_limit(400, &routed));
        for raw in [
            "max_tokens_exceeded",
            r#"{"detail":{"error_type":"invalid_question"}}"#,
            r#"{"error":{"code":400,"message":"input contains max_tokens_exceeded"}}"#,
        ] {
            assert!(!context_limit(400, raw));
        }
        assert!(!context_limit(500, &routed));
        let chat = r#"{"error":{"code":400,"message":"request too long","metadata":{"error_type":"context_length_exceeded"}}}"#;
        assert!(context_limit(400, chat));
        assert!(context_limit(200, chat));
        assert!(!context_limit(500, chat));
        assert!(!context_limit(
            400,
            &chat.replace("context_length_exceeded", "invalid_request")
        ));
    }

    #[test]
    fn retry_after_supports_seconds_and_http_dates() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "12".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(12)));
        let date = (chrono::Utc::now() + chrono::Duration::seconds(30))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(reqwest::header::RETRY_AFTER, date.parse().unwrap());
        let delay = retry_after(&headers).unwrap();
        assert!(delay > Duration::from_secs(28) && delay <= Duration::from_secs(30));
        headers.insert(reqwest::header::RETRY_AFTER, "invalid".parse().unwrap());
        assert_eq!(retry_after(&headers), None);
    }

    #[tokio::test]
    async fn transient_responses_retry_identical_judgment_and_record_every_attempt() {
        let (result, requests, attempts) =
            exercise(vec![529, 429, 200], DecisionRetryPolicy::default()).await;
        assert_eq!(result.unwrap(), "judgment");
        assert_eq!(requests, vec!["exact request"; 3]);
        assert_eq!(attempts.len(), 3);
    }

    #[tokio::test]
    async fn edge_gateway_failures_retry_the_same_read_only_judgment() {
        let policy = DecisionRetryPolicy {
            backoff: Duration::ZERO,
            ..Default::default()
        };
        for status in 520..=524 {
            let (result, requests, attempts) = exercise(vec![status, 200], policy).await;
            assert_eq!(result.unwrap(), "judgment", "HTTP {status}");
            assert_eq!(requests, vec!["exact request"; 2], "HTTP {status}");
            assert_eq!(attempts.len(), 2, "HTTP {status}");
        }
    }

    #[tokio::test]
    async fn truncated_bodies_preserve_authorization_and_retry_after() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for status in [401, 403, 429, 503, 200] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                stream.read(&mut request).await.unwrap();
                stream.write_all(format!("HTTP/1.1 {status} Response\r\nContent-Length: 100\r\nRetry-After: 31\r\nConnection: close\r\n\r\nshort").as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            });
            let error = request_decision(
                &reqwest::Client::new(),
                &endpoint,
                "abstract",
                "{}",
                DecisionRetryPolicy {
                    attempts: 1,
                    ..Default::default()
                },
                |_| Ok(()),
            )
            .await
            .unwrap_err();
            server.await.unwrap();
            assert!(matches!(error, DecisionTransportError::ResponseBody { .. }));
            if matches!(status, 401 | 403) {
                assert!(error.retry_delay(1).is_none());
            } else {
                assert!(error.retry_delay(1).unwrap() >= Duration::from_secs(31));
            }
        }
    }

    #[tokio::test]
    async fn transient_outage_recovery_respects_explicit_attempt_policy() {
        for status in [429, 502, 503, 504] {
            let single = DecisionRetryPolicy {
                attempts: 1,
                backoff: Duration::ZERO,
                ..Default::default()
            };
            let (result, requests, records) = exercise(vec![status, 200], single).await;
            assert!(matches!(
                result,
                Err(DecisionTransportError::ProviderRejected { .. })
            ));
            assert_eq!(requests.len(), 1);
            assert_eq!(records.len(), 1);

            let bounded = DecisionRetryPolicy {
                attempts: 3,
                deadline: Duration::from_secs(30),
                ..single
            };
            let (result, requests, records) = exercise(vec![status, 200], bounded).await;
            assert_eq!(result.unwrap(), "judgment");
            assert_eq!(requests, vec!["exact request"; 2]);
            assert_eq!(records.len(), 2);
        }
    }

    #[tokio::test]
    async fn single_attempt_retains_provider_status_and_message_without_claiming_retries() {
        let (result, requests, records) = exercise_body(
            vec![500],
            r#"{"error":{"message":"Abstract upstream failure","code":500}}"#,
            DecisionRetryPolicy {
                attempts: 1,
                ..Default::default()
            },
        )
        .await;
        let error = result.unwrap_err();
        assert_eq!(requests.len(), 1);
        assert_eq!(records.len(), 1);
        assert!(
            matches!(error, DecisionTransportError::ProviderRejected { status, .. } if status.as_u16() == 500)
        );
        assert!(error.to_string().contains("Abstract upstream failure"));
        assert!(!error.to_string().contains("retries"));
    }

    #[tokio::test]
    async fn exhausted_attempts_retain_the_last_typed_cause() {
        use std::error::Error;
        let (result, requests, _) = exercise_body(
            vec![503],
            r#"{"error":{"message":"Abstract overloaded provider","code":503}}"#,
            DecisionRetryPolicy {
                attempts: 2,
                backoff: Duration::ZERO,
                ..Default::default()
            },
        )
        .await;
        let error = result.unwrap_err();
        assert_eq!(requests.len(), 2);
        let cause = error
            .source()
            .expect("provider cause must survive exhausted admission");
        assert!(cause.to_string().contains("503"));
        assert!(cause.to_string().contains("Abstract overloaded provider"));
    }

    #[tokio::test]
    async fn fatal_response_does_not_retry() {
        let (result, requests, _) = exercise(vec![401, 200], DecisionRetryPolicy::default()).await;
        assert!(result.is_err());
        assert_eq!(requests.len(), 1);
    }

    #[tokio::test]
    async fn retry_budget_and_total_deadline_are_bounded() {
        let policy = DecisionRetryPolicy {
            attempts: 2,
            backoff: Duration::ZERO,
            ..Default::default()
        };
        let (result, requests, _) = exercise(vec![529], policy).await;
        assert!(result.is_err());
        assert_eq!(requests.len(), 2);
        let policy = DecisionRetryPolicy {
            deadline: Duration::from_millis(100),
            backoff: Duration::from_secs(1),
            ..policy
        };
        let (result, requests, _) = exercise(vec![529], policy).await;
        assert!(result.unwrap_err().to_string().contains("deadline"));
        assert_eq!(requests.len(), 1);
    }
}
