use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[test]
fn only_explicit_in_flight_credit_reservation_rejections_retry() {
    let rejection = |message: &str| ProviderTransportError::ProviderRejected {
        status: reqwest::StatusCode::PAYMENT_REQUIRED,
        message: message.into(),
        retry_after: None,
    };
    assert!(rejection("This request would exceed your available credits given your current in-flight requests. Retry after in-flight requests settle, or add credits.").retry_delay(0).is_some());
    assert!(rejection("Insufficient credits").retry_delay(0).is_none());
    assert!(rejection("Payment required").retry_delay(0).is_none());
}

struct StallThenRespond {
    calls: AtomicUsize,
    bodies: Mutex<Vec<String>>,
    stalls: usize,
    status: Option<u16>,
}

#[async_trait::async_trait]
impl ProviderTransport for StallThenRespond {
    async fn send(
        &self,
        body: &str,
        observer: &mut AttemptRecorder<'_>,
    ) -> Result<ProviderResponse> {
        self.bodies.lock().unwrap().push(body.to_owned());
        observer.phase(AttemptPhase::Dispatch, None, None)?;
        if let Some(status) = self.status {
            observer.phase(
                AttemptPhase::ResponseBody,
                Some(status),
                Some(Duration::from_secs(31)),
            )?;
        }
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.stalls {
            std::future::pending::<()>().await;
        }
        Ok(ProviderResponse {
            status: reqwest::StatusCode::OK,
            retry_after: None,
            body: "judgment".into(),
        })
    }
}

fn adapter(stalls: usize, status: Option<u16>) -> StallThenRespond {
    StallThenRespond {
        calls: AtomicUsize::new(0),
        bodies: Mutex::new(Vec::new()),
        stalls,
        status,
    }
}
fn fast_policy() -> ProviderRetryPolicy {
    ProviderRetryPolicy {
        attempts: 3,
        deadline: Duration::from_secs(1),
        attempt_timeout: Duration::from_millis(20),
        backoff: Duration::ZERO,
    }
}

#[tokio::test]
async fn stalled_attempt_recovers_with_identical_bytes_and_complete_lifecycle() {
    let transport = adapter(1, None);
    let mut records = Vec::new();
    let result = request_provider(&transport, "immutable judgment", fast_policy(), |e| {
        records.push(e.clone());
        Ok(())
    })
    .await;
    assert_eq!(result.unwrap(), "judgment");
    assert_eq!(
        *transport.bodies.lock().unwrap(),
        vec!["immutable judgment"; 2]
    );
    assert_eq!(
        records
            .iter()
            .filter(|e| matches!(
                e,
                ProviderAttempt::Lifecycle {
                    event: "failed",
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|e| matches!(
                e,
                ProviderAttempt::Lifecycle {
                    event: "completed",
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(matches!(
        records.first(),
        Some(ProviderAttempt::Lifecycle {
            event: "started",
            phase: AttemptPhase::Admission,
            ..
        })
    ));
}

#[tokio::test]
async fn all_stalled_attempts_keep_typed_timeout_and_attempt_ceiling() {
    let transport = adapter(usize::MAX, None);
    let error = request_provider(&transport, "immutable", fast_policy(), |_| Ok(()))
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderTransportError::RetryExhausted { attempts: 3, source } if matches!(*source, ProviderTransportError::AttemptTimeout { phase: AttemptPhase::Dispatch, .. }))
    );
    assert_eq!(transport.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn slow_authorization_body_is_terminal_and_retry_after_is_preserved() {
    for status in [401, 403, 429, 503] {
        let transport = adapter(usize::MAX, Some(status));
        let error = request_provider(
            &transport,
            "immutable",
            ProviderRetryPolicy {
                attempts: 1,
                ..fast_policy()
            },
            |_| Ok(()),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, ProviderTransportError::AttemptTimeout { status: Some(observed), .. } if observed == status)
        );
        if status == 401 || status == 403 {
            assert!(error.retry_delay(1).is_none());
        } else {
            assert!(error.retry_delay(1).unwrap() >= Duration::from_secs(31));
        }
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn outer_cancellation_persists_phase_without_claiming_transmission() {
    let transport = adapter(usize::MAX, None);
    let mut events = Vec::new();
    let result = tokio::time::timeout(
        Duration::from_millis(20),
        request_provider(
            &transport,
            "immutable",
            ProviderRetryPolicy::default(),
            |e| {
                events.push(e.clone());
                Ok(())
            },
        ),
    )
    .await;
    assert!(result.is_err());
    assert!(matches!(
        events.last(),
        Some(ProviderAttempt::Lifecycle {
            event: "cancelled",
            phase: AttemptPhase::Dispatch,
            ..
        })
    ));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn receipt_failure_prevents_dispatch() {
    let transport = adapter(0, None);
    let error = request_provider(&transport, "immutable", fast_policy(), |_| {
        Err(ProviderRecordError::Io(std::io::Error::other("disk full")))
    })
    .await
    .unwrap_err();
    assert!(matches!(error, ProviderTransportError::Record(_)));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn slow_admission_receipt_cannot_dispatch_after_attempt_deadline() {
    let transport = adapter(0, None);
    let error = request_attempt(
        &transport,
        "immutable",
        1,
        Duration::from_millis(5),
        |event| {
            if matches!(
                event,
                ProviderAttempt::Lifecycle {
                    phase: AttemptPhase::Admission,
                    event: "started",
                    ..
                }
            ) {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(())
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        ProviderTransportError::AttemptTimeout {
            phase: AttemptPhase::Admission,
            ..
        }
    ));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn reqwest_stall_uses_the_same_injected_attempt_boundary() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let (endpoint, server) = super::test_support::spawn_server(move |body| {
        let seen = seen.clone();
        async move {
            assert_eq!(body, "immutable");
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                std::future::pending::<()>().await;
            }
            (200, "judgment".into())
        }
    })
    .await;
    let transport = ReqwestProviderTransport::new(reqwest::Client::new(), endpoint, "abstract");
    let result = request_provider(
        &transport,
        "immutable",
        ProviderRetryPolicy {
            attempt_timeout: Duration::from_millis(100),
            ..fast_policy()
        },
        |_| Ok(()),
    )
    .await;
    server.abort();
    assert_eq!(result.unwrap(), "judgment");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
