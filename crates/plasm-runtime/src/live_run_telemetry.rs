//! Per-run outbound HTTP counters and trace rows for live execute progress (MCP / op notifications).

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::http_trace::{HttpTraceEntry, HttpTraceOutcome};

tokio::task_local! {
    static ACTIVE_LIVE_RUN_TELEMETRY: Option<Arc<LiveRunTelemetry>>;
}

/// Session-scoped HTTP telemetry for one live plan run.
#[derive(Debug)]
pub struct LiveRunTelemetry {
    http_calls: AtomicU64,
    last_latency_ms: AtomicU64,
    started_at: Instant,
    http_trace_entries: Mutex<Vec<HttpTraceEntry>>,
}

impl LiveRunTelemetry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            http_calls: AtomicU64::new(0),
            last_latency_ms: AtomicU64::new(0),
            started_at: Instant::now(),
            http_trace_entries: Mutex::new(Vec::new()),
        }
    }

    pub fn record_http_trace(
        &self,
        method: impl Into<String>,
        url: impl Into<String>,
        duration: Duration,
        outcome: HttpTraceOutcome,
    ) {
        self.http_calls.fetch_add(1, Ordering::Relaxed);
        self.last_latency_ms
            .store(duration.as_millis() as u64, Ordering::Relaxed);
        if let Ok(mut g) = self.http_trace_entries.lock() {
            g.push(HttpTraceEntry {
                method: method.into(),
                url: url.into(),
                duration_ms: duration.as_millis() as u64,
                outcome,
            });
        }
    }

    pub fn record_http_completion(&self, duration: Duration) {
        self.record_http_trace(
            "HTTP",
            "",
            duration,
            HttpTraceOutcome::Ok,
        );
    }

    /// Take HTTP trace rows accumulated since the previous drain (per plan line / expression).
    pub fn drain_http_trace_entries(&self) -> Vec<HttpTraceEntry> {
        self.http_trace_entries
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
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
pub fn record_live_http_trace(
    method: impl Into<String>,
    url: impl Into<String>,
    duration: Duration,
    outcome: HttpTraceOutcome,
) {
    let Ok(Some(tel)) = ACTIVE_LIVE_RUN_TELEMETRY.try_with(|slot| slot.clone()) else {
        return;
    };
    tel.record_http_trace(method, url, duration, outcome);
}

/// Record one outbound HTTP completion for the active live-run telemetry scope, if any.
pub fn record_live_http_completion(duration: Duration) {
    record_live_http_trace("HTTP", "", duration, HttpTraceOutcome::Ok);
}

/// Drain per-line HTTP trace rows from the active live-run telemetry scope, if any.
#[must_use]
pub fn drain_active_live_http_trace_entries() -> Vec<HttpTraceEntry> {
    let Ok(Some(tel)) = ACTIVE_LIVE_RUN_TELEMETRY.try_with(|slot| slot.clone()) else {
        return Vec::new();
    };
    tel.drain_http_trace_entries()
}

pub async fn with_live_run_telemetry<Fut, T>(telemetry: Arc<LiveRunTelemetry>, fut: Fut) -> T
where
    Fut: Future<Output = T>,
{
    ACTIVE_LIVE_RUN_TELEMETRY.scope(Some(telemetry), fut).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_trace::HttpTraceOutcome;

    #[tokio::test]
    async fn telemetry_records_within_scope() {
        let tel = Arc::new(LiveRunTelemetry::new());
        with_live_run_telemetry(Arc::clone(&tel), async {
            record_live_http_trace(
                "GET",
                "https://pokeapi.co/api/v2/type/electric",
                Duration::from_millis(120),
                HttpTraceOutcome::Ok,
            );
            record_live_http_trace(
                "GET",
                "https://pokeapi.co/api/v2/pokemon/25",
                Duration::from_millis(340),
                HttpTraceOutcome::Ok,
            );
        })
        .await;
        assert_eq!(tel.http_calls(), 2);
        assert_eq!(tel.last_latency_ms(), 340);
        assert_eq!(tel.drain_http_trace_entries().len(), 2);
    }

    #[tokio::test]
    async fn drain_active_scope_clears_pending_rows() {
        let tel = Arc::new(LiveRunTelemetry::new());
        with_live_run_telemetry(Arc::clone(&tel), async {
            record_live_http_trace(
                "GET",
                "https://example.test/a",
                Duration::from_millis(10),
                HttpTraceOutcome::Ok,
            );
            let rows = drain_active_live_http_trace_entries();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].method, "GET");
            assert!(drain_active_live_http_trace_entries().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn telemetry_outside_scope_is_noop() {
        record_live_http_completion(Duration::from_millis(50));
        let tel = Arc::new(LiveRunTelemetry::new());
        assert_eq!(tel.http_calls(), 0);
    }
}
