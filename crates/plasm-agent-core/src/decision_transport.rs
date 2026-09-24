//! Bounded transport for repeatable, read-only model judgments. Never retries API effects.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

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
#[derive(Debug)]
pub(crate) struct DecisionContextLimit;
impl std::fmt::Display for DecisionContextLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Jev context limit exceeded; repartition the decision page")
    }
}
impl std::error::Error for DecisionContextLimit {}

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
    }
    fn provider(raw: &str) -> bool {
        serde_json::from_str::<ProviderError>(raw)
            .is_ok_and(|e| e.detail.error_type == "max_tokens_exceeded")
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
    matches!(status, 408 | 425 | 429 | 500 | 502..=504 | 529)
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
    mut record: impl FnMut(&DecisionAttempt) -> Result<()>,
) -> Result<String> {
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
            let (retry, delay, failure) = match response {
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
                            if status.is_success() {
                                return Ok(raw);
                            }
                            if context_limit(status.as_u16(), &raw) {
                                return Err(DecisionContextLimit.into());
                            }
                            (
                                transient(status.as_u16()),
                                delay,
                                format!("Jev Decisions HTTP {status}"),
                            )
                        }
                        Err(error) => {
                            record(&DecisionAttempt::Transport {
                                attempt,
                                error: error.to_string(),
                            })?;
                            (
                                transient(status.as_u16()) || status.is_success(),
                                delay,
                                error.to_string(),
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
                        error.is_timeout() || error.is_connect() || error.is_body(),
                        None,
                        error.to_string(),
                    )
                }
            };
            if !retry || attempt == policy.attempts {
                bail!("{failure}; decision transport stopped after {attempt} attempt(s)");
            }
            // Jitter each operation independently; Retry-After remains a lower bound.
            let entropy = uuid::Uuid::new_v4().as_u128() as u64;
            let backoff = policy.backoff.saturating_mul(1u32 << (attempt - 1).min(10));
            let jitter = Duration::from_millis(entropy % 251);
            let wait = delay.unwrap_or_default().max(backoff + jitter);
            tracing::warn!(
                attempt,
                wait_ms = wait.as_millis(),
                failure,
                "retrying read-only decision request"
            );
            tokio::time::sleep(wait).await;
        }
        bail!("decision transport requires a positive attempt budget")
    };
    tokio::time::timeout(policy.deadline, operation)
        .await
        .context("Jev Decisions total retry deadline exhausted")?
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
        assert!(result.unwrap_err().is::<DecisionContextLimit>());
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
