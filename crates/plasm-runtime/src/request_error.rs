use crate::http_auth_failure::{format_http_status_error, OutboundAuthorizationFact};
use thiserror::Error;

/// HTTP response evidence; external API detail is data, not a fabricated fault message.
#[derive(Debug, Clone)]
pub struct HttpStatusFailure {
    pub method: String,
    pub url: String,
    pub status: u16,
    pub detail: String,
    pub empty_body: bool,
    pub authorization: OutboundAuthorizationFact,
    pub login_token_tail: Option<String>,
    pub retry_budget_exhausted: bool,
}

impl HttpStatusFailure {
    pub fn without_request(status: u16, detail: String) -> Self {
        Self {
            method: String::new(),
            url: String::new(),
            status,
            detail,
            empty_body: false,
            authorization: OutboundAuthorizationFact::absent(),
            login_token_tail: None,
            retry_budget_exhausted: false,
        }
    }
}

impl std::fmt::Display for HttpStatusFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.status == 401 {
            write!(
                f,
                "{}",
                format_http_status_error(
                    &self.method,
                    &self.url,
                    self.status,
                    &self.detail,
                    &self.authorization,
                    self.login_token_tail.as_deref(),
                )
            )?;
        } else if self.method.is_empty() && self.url.is_empty() {
            write!(f, "HTTP {} from API: {}", self.status, self.detail)?;
        } else if self.empty_body {
            write!(
                f,
                "{} {} — HTTP {} with empty body",
                self.method, self.url, self.status
            )?;
        } else {
            write!(
                f,
                "{} {} — HTTP {} from API: {}",
                self.method, self.url, self.status, self.detail
            )?;
        }
        if self.retry_budget_exhausted {
            write!(f, " (retry budget exhausted)")?;
        }
        Ok(())
    }
}

impl std::error::Error for HttpStatusFailure {}

#[derive(Debug, Error)]
pub enum RequestFailure {
    #[error(transparent)]
    HttpStatus(#[from] Box<HttpStatusFailure>),
    #[error("{method} — HTTP {status}: response body is not valid JSON: {source}")]
    ResponseJson {
        method: String,
        status: u16,
        #[source]
        source: serde_json::Error,
    },
    #[error("Fibery command failed ({code}): {message}")]
    CommandRejected { code: String, message: String },
    #[error("Fibery command succeeded but returned no rows (empty `result` array). For `user_get_me`, the API token may not resolve `$my-id` — use a personal workspace API token and reconnect in Plasm.")]
    CommandRowsEmpty,
    #[error("Entity not found (`{field}` is null in the API response){hint}")]
    EntityNull {
        field: String,
        hint: crate::ResponseNarrowHint,
    },
    #[error("{0}")]
    GraphQlMutation(#[from] crate::api_error_detail::GraphQlMutationFailure),
    #[error("MockServer {operation:?} failed with HTTP {status}")]
    MockServer {
        operation: MockServerOperation,
        status: u16,
    },
    #[cfg(test)]
    #[error("test transport rejected an unexpected request")]
    UnexpectedTestRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockServerOperation {
    CreateExpectation,
    ClearExpectations,
    Reset,
}

#[derive(Debug, Error)]
pub enum RateLimitCause {
    #[error(transparent)]
    Upstream(#[from] Box<HttpStatusFailure>),
    #[error("HTTP concurrency queue timeout waiting for {scope}")]
    QueueTimeout {
        scope: String,
        #[source]
        source: tokio::time::error::Elapsed,
    },
}

impl From<HttpStatusFailure> for RequestFailure {
    fn from(failure: HttpStatusFailure) -> Self {
        Self::HttpStatus(Box::new(failure))
    }
}

impl From<HttpStatusFailure> for RateLimitCause {
    fn from(failure: HttpStatusFailure) -> Self {
        Self::Upstream(Box::new(failure))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn request_error_owners_have_bounded_footprints() {
        assert!(std::mem::size_of::<RequestFailure>() < 128);
        assert!(std::mem::size_of::<RateLimitCause>() < 128);
    }

    #[test]
    fn boxed_status_preserves_evidence_sources_and_upstream_category() {
        let evidence = HttpStatusFailure {
            method: "POST".into(),
            url: "https://example.test/records".into(),
            status: 429,
            detail: "request quota exhausted".into(),
            empty_body: false,
            authorization: OutboundAuthorizationFact::from_header(Some("Bearer fixture-TAIL")),
            login_token_tail: Some("TAIL".into()),
            retry_budget_exhausted: true,
        };
        let diagnostic = evidence.to_string();
        for error in [
            crate::RuntimeError::RequestError {
                source: evidence.clone().into(),
                attempts: 3,
                status: Some(429),
                body: Some(Box::new(serde_json::json!({"retry": true}))),
            },
            crate::RuntimeError::RateLimited {
                status: 429,
                host: "example.test".into(),
                retry_after: Some(std::time::Duration::from_secs(2)),
                attempts: 3,
                source: evidence.clone().into(),
            },
        ] {
            let source = error.source().expect("typed runtime source");
            assert_eq!(source.to_string(), diagnostic);
            let status = if let Some(RequestFailure::HttpStatus(status)) =
                source.downcast_ref::<RequestFailure>()
            {
                status.as_ref()
            } else if let Some(RateLimitCause::Upstream(status)) =
                source.downcast_ref::<RateLimitCause>()
            {
                status.as_ref()
            } else {
                panic!("expected concrete HTTP status source");
            };
            let concrete: &dyn Error = status;
            assert!(concrete.is::<HttpStatusFailure>());
            assert_eq!(status.method, evidence.method);
            assert_eq!(status.url, evidence.url);
            assert_eq!(status.status, evidence.status);
            assert_eq!(status.detail, evidence.detail);
            assert_eq!(status.empty_body, evidence.empty_body);
            assert_eq!(status.authorization, evidence.authorization);
            assert_eq!(status.login_token_tail, evidence.login_token_tail);
            assert_eq!(
                status.retry_budget_exhausted,
                evidence.retry_budget_exhausted
            );
            let failure = crate::ExecutionFailure::from(error);
            assert_eq!(failure.cause, crate::FailureCause::Upstream);
            assert_eq!(failure.code, "upstream_rejection");
        }
    }

    #[test]
    fn invalid_response_preserves_json_source() {
        let source = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let error = crate::RuntimeError::RequestError {
            source: RequestFailure::ResponseJson {
                method: "GET".to_owned(),
                status: 200,
                source,
            },
            attempts: 1,
            status: Some(200),
            body: None,
        };
        let failure = error.source().unwrap();
        assert!(failure.is::<RequestFailure>());
        assert!(failure.source().unwrap().is::<serde_json::Error>());
    }
}
