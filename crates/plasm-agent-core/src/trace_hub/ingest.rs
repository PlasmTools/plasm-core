//! Durable trace ingest worker and bounded `mpsc` enqueue after SSE patches.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;

use super::state::TraceIngestJob;
use super::TraceHub;
use crate::trace_sink_emit::TraceIngestClient;

async fn trace_ingest_worker(
    mut rx: mpsc::Receiver<TraceIngestJob>,
    ingest: Arc<dyn TraceIngestClient>,
    backlog: Arc<AtomicUsize>,
    queue_cap: i64,
) {
    while let Some(job) = rx.recv().await {
        backlog.fetch_sub(1, Ordering::Relaxed);
        crate::trace_hub_metrics::record_trace_hub_ingest_dequeued(queue_cap);
        let wait_ms = job.enqueued_at.elapsed().as_millis() as u64;
        crate::trace_hub_metrics::record_trace_hub_ingest_queue_wait_ms(wait_ms, queue_cap);
        crate::trace_sink_emit::spawn_emit_mcp_trace_segment(
            ingest.as_ref(),
            &job.fields,
            &job.trace_event,
            job.precomputed_payload,
        );
    }
}

/// Start bounded ingest channel + worker when a [`TraceIngestClient`] is configured.
pub(super) fn start_ingest_channel(
    trace_ingest: Arc<dyn TraceIngestClient>,
    queue_capacity: usize,
    backlog: Arc<AtomicUsize>,
) -> mpsc::Sender<TraceIngestJob> {
    let queue_cap_i64 = queue_capacity as i64;
    let (tx, rx) = mpsc::channel(queue_capacity);
    tokio::spawn(trace_ingest_worker(rx, trace_ingest, backlog, queue_cap_i64));
    tx
}

impl TraceHub {
    /// After the SSE `patch` is sent: block the MCP/HTTP caller until the job is accepted into the
    /// bounded `mpsc` (backpressure). Does not hold [`super::state::TraceHubInner`] locks across `.await`.
    pub(super) async fn enqueue_durable_job_after_patch(
        &self,
        tx: &mpsc::Sender<TraceIngestJob>,
        job: TraceIngestJob,
        patch_seq: u64,
    ) {
        let queue_cap = self.config.bounds.ingest_queue_capacity as i64;
        let wait_start = Instant::now();
        match tx.send(job).await {
            Ok(()) => {
                self.ingest_channel_backlog.fetch_add(1, Ordering::Relaxed);
                let wait_ms = wait_start.elapsed().as_millis() as u64;
                crate::trace_hub_metrics::record_trace_hub_ingest_send_wait_ms(wait_ms, queue_cap);
                let depth = self.ingest_channel_backlog.load(Ordering::Relaxed) as u64;
                crate::trace_hub_metrics::record_trace_hub_ingest_accepted(depth, queue_cap);
            }
            Err(e) => {
                let job = e.0;
                crate::trace_hub_metrics::record_trace_hub_ingest_enqueue_failed(
                    "closed", queue_cap,
                );
                tracing::warn!(
                    target: "plasm_agent::trace_hub",
                    trace_id = %job.fields.trace_id,
                    tenant_id = %job.fields.tenant_id.as_deref().unwrap_or(""),
                    queue_reason = "closed",
                    queue_capacity = queue_cap,
                    "durable trace ingest channel closed (SSE patch already delivered)"
                );
                self.emit_json(
                    job.fields.trace_id,
                    &super::TraceSsePayload::DurableIngest {
                        seq: patch_seq,
                        status: "enqueue_dropped".to_string(),
                        reason: "closed".to_string(),
                    },
                )
                .await;
            }
        }
    }
}
