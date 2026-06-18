//! Trace session open, finalize, and idle/disconnect sweeps.

use plasm_trace::SessionTraceData;

use super::resume::{active_from_completed, find_completed_index, CompletedResumeCriteria};
use super::state::{ActiveTrace, CompletedTrace};
use super::{
    now_ms, trace_id_for_mcp_logical_session, trace_id_for_mcp_transport_session, TraceHub,
    TraceSessionMeta, TraceSsePayload,
};

impl TraceHub {
    /// Ensure an active trace exists for this MCP **transport** session key (legacy / tests).
    ///
    /// `trace_id` is [`trace_id_for_mcp_transport_session`] for `(meta.tenant_id, mcp_key)`.
    /// Prefer [`Self::ensure_logical_session`] for production MCP traffic.
    pub async fn ensure_session(&self, mcp_key: &str, meta: TraceSessionMeta) -> uuid::Uuid {
        let trace_id = trace_id_for_mcp_transport_session(meta.tenant_id.as_str(), mcp_key);
        let session_trace_key = mcp_key.to_string();
        self.ensure_session_inner(
            session_trace_key,
            trace_id,
            meta,
            None,
            Some(mcp_key.to_string()),
        )
        .await
    }

    /// Ensure an active trace for an MCP **logical session** (agent-scoped), not transport.
    ///
    /// `logical_session_id` is the canonical UUID string from `plasm_context`. `mcp_transport_id`
    /// is optional `MCP-Session-Id` for correlation on summaries and audit payloads.
    pub async fn ensure_logical_session(
        &self,
        logical_session_id: &str,
        mcp_transport_id: Option<&str>,
        meta: TraceSessionMeta,
    ) -> uuid::Uuid {
        let trace_id =
            trace_id_for_mcp_logical_session(meta.tenant_id.as_str(), logical_session_id);
        let session_trace_key = logical_session_id.to_string();
        self.ensure_session_inner(
            session_trace_key,
            trace_id,
            meta,
            Some(logical_session_id.to_string()),
            mcp_transport_id.map(str::to_string),
        )
        .await
    }

    async fn ensure_session_inner(
        &self,
        session_trace_key: String,
        trace_id: uuid::Uuid,
        meta: TraceSessionMeta,
        logical_session_id: Option<String>,
        mcp_transport_session_id: Option<String>,
    ) -> uuid::Uuid {
        loop {
            let eviction = {
                let g = self.inner.read().await;
                match g.active.get(&session_trace_key) {
                    None => false,
                    Some(a) => a.meta.tenant_id != meta.tenant_id || a.trace_id != trace_id,
                }
            };
            if eviction {
                self.finalize_mcp_session(&session_trace_key).await;
                continue;
            }
            break;
        }

        loop {
            let mut g = self.inner.write().await;
            if let Some(a) = g.active.get(&session_trace_key) {
                if a.meta.tenant_id == meta.tenant_id && a.trace_id == trace_id {
                    return trace_id;
                }
                drop(g);
                self.finalize_mcp_session(&session_trace_key).await;
                continue;
            }

            let last_activity_ms = now_ms();
            let cap = self.config.bounds.max_timeline_events;
            let criteria =
                CompletedResumeCriteria::strict(trace_id, meta.tenant_id.as_str());
            let resumed = find_completed_index(&g.completed, &session_trace_key, criteria)
                .and_then(|pos| g.completed.remove(pos));
            let mut active = if let Some(c) = resumed {
                active_from_completed(c, cap, last_activity_ms)
            } else {
                ActiveTrace {
                    trace_id,
                    session_trace_key: session_trace_key.clone(),
                    logical_session_id: logical_session_id.clone(),
                    mcp_transport_session_id: mcp_transport_session_id.clone(),
                    meta: meta.clone(),
                    data: SessionTraceData::new_with_timeline_cap(session_trace_key.clone(), cap),
                    started_ms: last_activity_ms,
                    last_activity_ms,
                    seq: 0,
                }
            };
            active.logical_session_id = logical_session_id;
            active.mcp_transport_session_id = mcp_transport_session_id;
            active.meta = meta;
            let _tx =
                Self::broadcast_tx(&mut g, trace_id, self.config.bounds.sse_broadcast_capacity);
            g.active.insert(session_trace_key.clone(), active);
            crate::trace_hub_metrics::record_trace_hub_queue_state(
                g.completed.len(),
                g.active.len(),
                false,
                self.config.bounds.max_completed_traces as i64,
            );
            return trace_id;
        }
    }

    /// Finalize every active trace whose hub key (logical session id or legacy transport key) is
    /// **not** in `live_trace_session_keys` (no active MCP transport still holds that session).
    pub async fn finalize_disconnected_sessions(
        &self,
        live_trace_session_keys: &std::collections::HashSet<String>,
    ) -> Vec<String> {
        let stale: Vec<String> = {
            let g = self.inner.read().await;
            g.active
                .keys()
                .filter(|k| !live_trace_session_keys.contains(k.as_str()))
                .cloned()
                .collect()
        };
        for k in &stale {
            self.finalize_mcp_session(k).await;
        }
        stale
    }

    /// Finalize active traces whose key is **still** in `live_trace_session_keys` but have had no
    /// activity for `idle_ms` (0 = disabled). The next tool activity **resumes** the same trace root.
    pub async fn finalize_idle_traces(
        &self,
        live_trace_session_keys: &std::collections::HashSet<String>,
        idle_ms: u64,
    ) -> Vec<String> {
        if idle_ms == 0 {
            return Vec::new();
        }
        let now = now_ms();
        let stale: Vec<String> = {
            let g = self.inner.read().await;
            g.active
                .iter()
                .filter(|(k, a)| {
                    live_trace_session_keys.contains(k.as_str())
                        && now.saturating_sub(a.last_activity_ms) >= idle_ms
                })
                .map(|(k, _)| k.clone())
                .collect()
        };
        for k in &stale {
            self.finalize_mcp_session(k).await;
        }
        stale
    }

    pub async fn finalize_mcp_session(&self, trace_session_key: &str) {
        let (trace_id, seq, ended_ms, detail_for_archive) = {
            let mut g = self.inner.write().await;
            let Some(active) = g.active.remove(trace_session_key) else {
                return;
            };
            let ended_ms = now_ms();
            let seq = active.seq.saturating_add(1);
            let trace_id = active.trace_id;
            let completed = CompletedTrace {
                trace_id,
                session_trace_key: active.session_trace_key.clone(),
                logical_session_id: active.logical_session_id.clone(),
                mcp_transport_session_id: active.mcp_transport_session_id.clone(),
                meta: active.meta.clone(),
                data: active.data.clone(),
                started_ms: active.started_ms,
                ended_ms,
                last_seq_emitted: seq,
            };
            let detail_for_archive = self
                .local_trace_archive
                .as_ref()
                .map(|_| Self::completed_to_detail(&completed));
            let evicted_oldest_completed =
                g.completed.len() >= self.config.bounds.max_completed_traces;
            if evicted_oldest_completed {
                g.completed.pop_front();
            }
            g.completed.push_back(completed);
            g.tx_by_trace.remove(&trace_id);
            let completed_len = g.completed.len();
            let active_len = g.active.len();
            crate::trace_hub_metrics::record_trace_hub_queue_state(
                completed_len,
                active_len,
                evicted_oldest_completed,
                self.config.bounds.max_completed_traces as i64,
            );
            (trace_id, seq, ended_ms, detail_for_archive)
        };
        if let (Some(arch), Some(detail)) = (self.local_trace_archive.as_ref(), detail_for_archive)
        {
            let arch = arch.clone();
            tokio::spawn(async move {
                if let Err(e) = arch.persist_trace(&detail).await {
                    tracing::warn!(
                        target: "plasm_agent::trace_hub",
                        error = %e,
                        "PLASM_TRACE_ARCHIVE_DIR: failed to persist completed trace (non-fatal)"
                    );
                }
            });
        }
        self.emit_json(
            trace_id,
            &TraceSsePayload::Terminal {
                seq,
                status: "completed".into(),
                ended_at_ms: Some(ended_ms),
            },
        )
        .await;
    }
}
