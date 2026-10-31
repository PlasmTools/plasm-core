//! One transport attempt and cancellation-safe lifecycle evidence. No retry loop.
use super::*;
use async_trait::async_trait;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptPhase {
    Admission,
    Dispatch,
    ResponseBody,
}

pub struct ProviderResponse {
    pub status: reqwest::StatusCode,
    pub retry_after: Option<Duration>,
    pub body: String,
}

/// An adapter performs exactly one repeatable provider request, never domain effects.
#[async_trait]
pub trait ProviderTransport: Send + Sync {
    async fn send(
        &self,
        body: &str,
        observer: &mut AttemptRecorder<'_>,
    ) -> Result<ProviderResponse>;
}

pub struct ReqwestProviderTransport {
    client: reqwest::Client,
    endpoint: String,
    key: String,
}

impl ReqwestProviderTransport {
    pub fn new(
        client: reqwest::Client,
        endpoint: impl Into<String>,
        key: impl Into<String>,
    ) -> Self {
        Self {
            client,
            endpoint: endpoint.into(),
            key: key.into(),
        }
    }
}

#[async_trait]
impl ProviderTransport for ReqwestProviderTransport {
    async fn send(
        &self,
        body: &str,
        observer: &mut AttemptRecorder<'_>,
    ) -> Result<ProviderResponse> {
        observer.phase(AttemptPhase::Dispatch, None, None)?;
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_owned())
            .send()
            .await
            .map_err(ProviderTransportError::Request)?;
        let status = response.status();
        let retry_after = retry_after(response.headers());
        observer.phase(
            AttemptPhase::ResponseBody,
            Some(status.as_u16()),
            retry_after,
        )?;
        let body =
            response
                .text()
                .await
                .map_err(|source| ProviderTransportError::ResponseBody {
                    status,
                    retry_after,
                    source,
                })?;
        Ok(ProviderResponse {
            status,
            retry_after,
            body,
        })
    }
}

type Record<'a> =
    dyn FnMut(&ProviderAttempt) -> std::result::Result<(), ProviderRecordError> + Send + 'a;

/// Drop records cancellation (including an outer deadline). Persistence errors on
/// normal paths propagate; drop cannot return an error, so it emits an error trace.
pub struct AttemptRecorder<'a> {
    request_hash: String,
    request_bytes: usize,
    record: &'a mut Record<'a>,
    attempt: usize,
    attempt_id: String,
    started_at_unix_ms: u128,
    started: std::time::Instant,
    deadline: tokio::time::Instant,
    phase: AttemptPhase,
    status: Option<u16>,
    retry_after: Option<Duration>,
    finished: bool,
}

impl<'a> AttemptRecorder<'a> {
    pub(super) fn start(
        record: &'a mut Record<'a>,
        attempt: usize,
        body: &str,
        deadline: tokio::time::Instant,
    ) -> Result<Self> {
        let mut this = Self {
            request_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
            request_bytes: body.len(),
            record,
            attempt,
            attempt_id: uuid::Uuid::new_v4().to_string(),
            started_at_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            started: std::time::Instant::now(),
            deadline,
            phase: AttemptPhase::Admission,
            status: None,
            retry_after: None,
            finished: false,
        };
        this.phase(AttemptPhase::Admission, None, None)?;
        Ok(this)
    }
    pub fn phase(
        &mut self,
        phase: AttemptPhase,
        status: Option<u16>,
        retry_after: Option<Duration>,
    ) -> Result<()> {
        self.phase = phase;
        self.status = status;
        self.retry_after = retry_after;
        (self.record)(&ProviderAttempt::Lifecycle {
            request_hash: self.request_hash.clone(),
            request_bytes: self.request_bytes,
            attempt_id: self.attempt_id.clone(),
            started_at_unix_ms: self.started_at_unix_ms,
            attempt: self.attempt,
            phase,
            event: "started",
            elapsed_ms: self.started.elapsed().as_millis(),
            http_status: status,
            retry_after_ms: retry_after.map(|d| d.as_millis()),
        })?;
        // Synchronous receipt persistence cannot be preempted by Tokio's timer.
        // Recheck before the adapter is permitted to perform its next I/O phase.
        if tokio::time::Instant::now() >= self.deadline {
            return Err(self.timeout_error());
        }
        Ok(())
    }
    pub(super) fn timeout_error(&self) -> ProviderTransportError {
        ProviderTransportError::AttemptTimeout {
            phase: self.phase,
            status: self.status,
            retry_after: self.retry_after,
        }
    }
    pub(super) fn finish(&mut self, outcome: &Result<ProviderResponse>) -> Result<()> {
        let event = match outcome {
            Ok(response) => ProviderAttempt::Response {
                request_hash: self.request_hash.clone(),
                request_bytes: self.request_bytes,
                attempt_id: self.attempt_id.clone(),
                attempt: self.attempt,
                http_status: response.status.as_u16(),
                raw_response: response.body.clone(),
            },
            Err(error) => ProviderAttempt::Transport {
                request_hash: self.request_hash.clone(),
                request_bytes: self.request_bytes,
                attempt_id: self.attempt_id.clone(),
                attempt: self.attempt,
                error: error.to_string(),
            },
        };
        (self.record)(&event)?;
        (self.record)(&ProviderAttempt::Lifecycle {
            request_hash: self.request_hash.clone(),
            request_bytes: self.request_bytes,
            attempt_id: self.attempt_id.clone(),
            started_at_unix_ms: self.started_at_unix_ms,
            attempt: self.attempt,
            phase: self.phase,
            event: if outcome.is_ok() {
                "completed"
            } else {
                "failed"
            },
            elapsed_ms: self.started.elapsed().as_millis(),
            http_status: self.status,
            retry_after_ms: self.retry_after.map(|d| d.as_millis()),
        })?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for AttemptRecorder<'_> {
    fn drop(&mut self) {
        if !self.finished {
            if let Err(error) = (self.record)(&ProviderAttempt::Lifecycle {
                request_hash: self.request_hash.clone(),
                request_bytes: self.request_bytes,
                attempt_id: self.attempt_id.clone(),
                started_at_unix_ms: self.started_at_unix_ms,
                attempt: self.attempt,
                phase: self.phase,
                event: "cancelled",
                elapsed_ms: self.started.elapsed().as_millis(),
                http_status: self.status,
                retry_after_ms: self.retry_after.map(|d| d.as_millis()),
            }) {
                tracing::error!(%error, attempt = self.attempt, "provider cancellation receipt could not be persisted");
            }
        }
    }
}
