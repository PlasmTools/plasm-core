//! Segment push + SSE/durable emit under one hub write lock.

use plasm_trace::{TraceEvent, TraceSegment};

use super::resume::{CompletedResumeCriteria, ensure_active_locked};
use super::state::{trace_segment_kind, TraceIngestJob};
use super::{now_ms, TraceHub};

impl TraceHub {
    pub(super) async fn ensure_active_for_emit(&self, mcp_key: &str) -> bool {
        let mut g = self.inner.write().await;
        ensure_active_locked(
            &mut g,
            mcp_key,
            CompletedResumeCriteria::for_emit(),
            self.config.bounds.max_timeline_events,
            self.config.bounds.sse_broadcast_capacity,
            now_ms(),
            self.config.bounds.max_completed_traces as i64,
        )
    }

    pub(super) async fn bump_and_emit(&self, mcp_key: &str, segment: TraceSegment) {
        let kind = trace_segment_kind(&segment);
        let bounds = self.config.bounds;
        let (trace_id, seq, record, job_opt) = {
            let mut g = self.inner.write().await;
            if !ensure_active_locked(
                &mut g,
                mcp_key,
                CompletedResumeCriteria::for_emit(),
                bounds.max_timeline_events,
                bounds.sse_broadcast_capacity,
                now_ms(),
                bounds.max_completed_traces as i64,
            ) {
                drop(g);
                tracing::warn!(
                    target: "plasm_agent::trace_hub",
                    mcp_key,
                    kind,
                    "trace segment dropped: no active or resumable completed trace"
                );
                crate::trace_hub_metrics::record_trace_emit_dropped_no_active();
                return;
            }
            let Some(a) = g.active.get_mut(mcp_key) else {
                tracing::warn!(
                    target: "plasm_agent::trace_hub",
                    mcp_key,
                    kind,
                    "trace segment dropped: active trace missing after resume"
                );
                crate::trace_hub_metrics::record_trace_emit_dropped_no_active();
                return;
            };
            let t = now_ms();
            a.last_activity_ms = t;
            a.seq = a.seq.saturating_add(1);
            let seq = a.seq;
            let ev = TraceEvent::at(t, segment);
            let dropped = a.data.push_event(ev);
            if dropped > 0 {
                crate::metrics::record_trace_timeline_events_dropped(dropped);
            }
            let ev_ref = a
                .data
                .records
                .back()
                .expect("push_event always appends one record");
            let record = serde_json::to_value(ev_ref).unwrap_or_else(|_| serde_json::json!({}));
            let job_opt = self.ingest_tx.as_ref().map(|_| {
                TraceIngestJob::new_mcp_active_segment(a, ev_ref.clone(), Some(record.clone()))
            });
            (a.trace_id, seq, record, job_opt)
        };
        self.emit_json(trace_id, &super::TraceSsePayload::Patch { seq, record })
            .await;
        if let (Some(tx), Some(job)) = (self.ingest_tx.as_ref(), job_opt) {
            self.enqueue_durable_job_after_patch(tx, job, seq).await;
        }
    }
}
