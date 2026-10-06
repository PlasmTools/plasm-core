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
    HttpStatus(#[from] HttpStatusFailure),
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
    Upstream(#[from] HttpStatusFailure),
    #[error("HTTP concurrency queue timeout waiting for {scope}")]
    QueueTimeout {
        scope: String,
        #[source]
        source: tokio::time::error::Elapsed,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

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
