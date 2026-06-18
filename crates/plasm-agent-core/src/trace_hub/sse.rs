//! Per-trace SSE broadcast channels and JSON patch/snapshot/terminal emit.

use tokio::sync::broadcast;
use uuid::Uuid;

use super::detail::tenant_visible_to_viewer;
use super::state::TraceHubInner;
use super::{TraceHub, TraceSsePayload};

impl TraceHub {
    pub(crate) fn broadcast_tx(
        inner: &mut TraceHubInner,
        trace_id: Uuid,
        sse_broadcast_capacity: usize,
    ) -> broadcast::Sender<String> {
        inner
            .tx_by_trace
            .entry(trace_id)
            .or_insert_with(|| broadcast::channel(sse_broadcast_capacity).0)
            .clone()
    }

    pub async fn subscribe_trace_async(
        &self,
        trace_id: Uuid,
    ) -> Option<broadcast::Receiver<String>> {
        let g = self.inner.read().await;
        g.tx_by_trace.get(&trace_id).map(|tx| tx.subscribe())
    }

    pub(super) async fn emit_json(&self, trace_id: Uuid, payload: &TraceSsePayload) {
        let json = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
        let g = self.inner.read().await;
        if let Some(tx) = g.tx_by_trace.get(&trace_id) {
            let _ = tx.send(json);
        }
    }

    /// Initial SSE payload after subscribe: full detail snapshot.
    ///
    /// `seq` matches the latest emitted patch (or [`super::state::CompletedTrace::last_seq_emitted`] after
    /// terminal) so clients can align snapshot ordering with the patch stream.
    pub async fn sse_snapshot_payload(
        &self,
        trace_id: Uuid,
        viewer_tenant_id: Option<&str>,
    ) -> Option<String> {
        let seq = {
            let g = self.inner.read().await;
            if let Some(a) = g.active.values().find(|a| a.trace_id == trace_id) {
                a.seq
            } else {
                g.completed
                    .iter()
                    .filter(|c| {
                        c.trace_id == trace_id
                            && tenant_visible_to_viewer(viewer_tenant_id, &c.meta.tenant_id)
                    })
                    .max_by_key(|c| c.ended_ms)
                    .map(|c| c.last_seq_emitted)
                    .unwrap_or(0)
            }
        };
        let detail = self.get_detail(trace_id, viewer_tenant_id).await?;
        let payload = TraceSsePayload::Snapshot {
            seq,
            detail: Box::new(detail),
        };
        serde_json::to_string(&payload).ok()
    }
}
