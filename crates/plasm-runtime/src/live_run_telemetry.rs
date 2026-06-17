//! Per-run outbound HTTP counters for live execute progress (MCP App / op notifications).

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

tokio::task_local! {
    static ACTIVE_LIVE_RUN_TELEMETRY: Option<Arc<LiveRunTelemetry>>;
}

/// Session-scoped HTTP telemetry for one live plan run.
#[derive(Debug)]
pub struct LiveRunTelemetry {
    http_calls: AtomicU64,
    last_latency_ms: AtomicU64,
    started_at: Instant,
}

impl LiveRunTelemetry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            http_calls: AtomicU64::new(0),
            last_latency_ms: AtomicU64::new(0),
            started_at: Instant::now(),
        }
    }

    pub fn record_http_completion(&self, duration: Duration) {
        self.http_calls.fetch_add(1, Ordering::Relaxed);
        self.last_latency_ms
            .store(duration.as_millis() as u64, Ordering::Relaxed);
    }

    #[must_use]
    pub fn http_calls(&self) -> u64 {
        self.http_calls.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn last_latency_ms(&self) -> u64 {
        self.last_latency_ms.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }
}

impl Default for LiveRunTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

/// Record one outbound HTTP completion for the active live-run telemetry scope, if any.
pub fn record_live_http_completion(duration: Duration) {
    let Ok(Some(tel)) = ACTIVE_LIVE_RUN_TELEMETRY.try_with(|slot| slot.clone()) else {
        return;
    };
    tel.record_http_completion(duration);
}

pub async fn with_live_run_telemetry<Fut, T>(telemetry: Arc<LiveRunTelemetry>, fut: Fut) -> T
where
    Fut: Future<Output = T>,
{
    ACTIVE_LIVE_RUN_TELEMETRY
        .scope(Some(telemetry), fut)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn telemetry_records_within_scope() {
        let tel = Arc::new(LiveRunTelemetry::new());
        with_live_run_telemetry(Arc::clone(&tel), async {
            record_live_http_completion(Duration::from_millis(120));
            record_live_http_completion(Duration::from_millis(340));
        })
        .await;
        assert_eq!(tel.http_calls(), 2);
        assert_eq!(tel.last_latency_ms(), 340);
    }

    #[tokio::test]
    async fn telemetry_outside_scope_is_noop() {
        record_live_http_completion(Duration::from_millis(50));
        let tel = Arc::new(LiveRunTelemetry::new());
        assert_eq!(tel.http_calls(), 0);
    }
}
