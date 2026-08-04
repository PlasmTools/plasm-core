//! Iceberg-backed application state: strict idempotent ingest and read APIs.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use tracing::Instrument;
use uuid::Uuid;

use crate::append_port::{AuditSpanStore, TenantId, TimeWindow, TraceListFilter};
use crate::model::{
    AuditEvent, DurableTraceDetail, TraceHeadRow, TraceSpanRow, TraceSummary,
    AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT,
};
use crate::projector::project_trace_spans;
use plasm_trace::{SessionTraceCountersSnapshot, SessionTraceData};

/// Shared state: all durable data lives in Iceberg via [`AuditSpanStore`].
pub struct AppState {
    store: Arc<dyn AuditSpanStore>,
}

/// Distinct `tenant_partition` values for [`AuditEvent::tenant_partition`], sorted for stable SQL.
pub(crate) fn unique_tenant_partitions(events: &[AuditEvent]) -> Vec<String> {
    let mut v: Vec<String> = events.iter().map(|e| e.tenant_partition()).collect();
    v.sort();
    v.dedup();
    v
}

impl AppState {
    pub fn new(store: Arc<dyn AuditSpanStore>) -> Arc<Self> {
        Arc::new(Self { store })
    }

    /// Returns `(accepted, duplicate_skipped)` using strict idempotency on `event_id` (batch + Iceberg).
    pub async fn ingest_batch(&self, events: Vec<AuditEvent>) -> (usize, usize) {
        let mut duplicate_skipped = 0usize;
        let mut pending = Vec::new();
        let mut seen_batch = HashSet::new();

        for ev in events {
            if !seen_batch.insert(ev.event_id) {
                duplicate_skipped += 1;
                continue;
            }
            pending.push(ev);
        }

        if pending.is_empty() {
            return (0, duplicate_skipped);
        }

        let ids: Vec<Uuid> = pending.iter().map(|e| e.event_id).collect();
        let partition_keys = unique_tenant_partitions(&pending);
        let tenant_partition_filter: Option<&[String]> = if partition_keys.is_empty() {
            None
        } else {
            Some(&partition_keys)
        };

        let t_dedupe = Instant::now();
        let existing = match self
            .store
            .existing_event_ids(&ids, tenant_partition_filter)
            .instrument(tracing::debug_span!(
                "plasm_trace_sink.ingest.existing_event_ids",
                id_count = ids.len(),
                partition_key_count = tenant_partition_filter.map(|s| s.len()).unwrap_or(0),
            ))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "Iceberg existing_event_ids query failed");
                return (0, duplicate_skipped);
            }
        };
        tracing::info!(
            target: "plasm_trace_sink::ingest_timing",
            phase = "existing_event_ids",
            elapsed_ms = t_dedupe.elapsed().as_millis() as u64,
            id_count = ids.len(),
            partition_key_count = tenant_partition_filter.map(|s| s.len()).unwrap_or(0),
        );

        let mut accepted = Vec::new();
        for ev in pending {
            if existing.contains(&ev.event_id) {
                duplicate_skipped += 1;
            } else {
                accepted.push(ev);
            }
        }

        if accepted.is_empty() {
            return (0, duplicate_skipped);
        }

        let mut spans: Vec<TraceSpanRow> = Vec::new();
        for ev in &accepted {
            spans.extend(project_trace_spans(ev));
        }

        let t_append = Instant::now();
        if let Err(e) = self
            .store
            .append_audit_events_with_trace_spans(&accepted, &spans)
            .instrument(tracing::debug_span!(
                "plasm_trace_sink.ingest.append_audit_trace",
                event_count = accepted.len(),
                span_count = spans.len(),
            ))
            .await
        {
            tracing::error!(error = %e, "Iceberg append audit_events+trace_spans failed");
            return (0, duplicate_skipped);
        }
        tracing::info!(
            target: "plasm_trace_sink::ingest_timing",
            phase = "append_audit_trace",
            elapsed_ms = t_append.elapsed().as_millis() as u64,
            event_count = accepted.len(),
        );

        let t_heads = Instant::now();
        if let Err(e) = self
            .update_trace_heads(&accepted)
            .instrument(tracing::debug_span!(
                "plasm_trace_sink.ingest.update_trace_heads",
                accepted_event_count = accepted.len(),
            ))
            .await
        {
            tracing::error!(error = %e, "Iceberg append trace_heads failed");
        }
        tracing::info!(
            target: "plasm_trace_sink::ingest_timing",
            phase = "update_trace_heads",
            elapsed_ms = t_heads.elapsed().as_millis() as u64,
        );

        (accepted.len(), duplicate_skipped)
    }

    pub async fn trace_events(&self, trace_id: Uuid) -> anyhow::Result<Vec<AuditEvent>> {
        self.store.load_trace_events(trace_id).await
    }

    pub async fn trace_events_for_tenant(
        &self,
        tenant: &TenantId,
        trace_id: Uuid,
    ) -> anyhow::Result<Vec<AuditEvent>> {
        self.store
            .load_trace_events_for_tenant(tenant, trace_id)
            .await
    }

    pub async fn billing_usage(
        &self,
        tenant: Option<&TenantId>,
        window: TimeWindow,
    ) -> anyhow::Result<Vec<TraceSpanRow>> {
        match tenant {
            Some(t) => self.store.load_billing_usage_scoped(t, window).await,
            None => self.store.load_billing_usage_global(window).await,
        }
    }

    pub async fn list_traces(
        &self,
        filter: TraceListFilter<'_>,
    ) -> anyhow::Result<Vec<TraceSummary>> {
        self.store.list_trace_summaries(filter).await
    }

    pub async fn trace_detail(
        &self,
        tenant: &TenantId,
        trace_id: Uuid,
    ) -> anyhow::Result<Option<DurableTraceDetail>> {
        self.store.load_trace_detail(tenant, trace_id).await
    }

    async fn update_trace_heads(&self, accepted: &[AuditEvent]) -> anyhow::Result<()> {
        use std::collections::HashMap;
        if accepted.is_empty() {
            return Ok(());
        }
        let mut by_trace: HashMap<uuid::Uuid, Vec<&AuditEvent>> = HashMap::new();
        for ev in accepted {
            by_trace.entry(ev.trace_id).or_default().push(ev);
        }
        let trace_ids = by_trace.keys().copied().collect::<Vec<_>>();
        let t_load = Instant::now();
        let existing = self
            .store
            .load_latest_trace_heads(&trace_ids)
            .instrument(tracing::debug_span!(
                "plasm_trace_sink.ingest.load_latest_trace_heads",
                trace_id_count = trace_ids.len(),
            ))
            .await?;
        tracing::info!(
            target: "plasm_trace_sink::ingest_timing",
            phase = "load_latest_trace_heads",
            elapsed_ms = t_load.elapsed().as_millis() as u64,
            trace_id_count = trace_ids.len(),
        );
        let mut existing_by_id = HashMap::new();
        for h in existing {
            existing_by_id.insert(h.trace_id, h);
        }

        let mut rows = Vec::with_capacity(by_trace.len());
        for (trace_id, evs) in by_trace {
            let mut min_ms = i64::MAX;
            let mut max_ms = i64::MIN;
            let mut max_call = None::<i64>;
            let mut tenant_partition = "__none__".to_string();
            let mut tenant_id = "__none__".to_string();
            let mut project_slug = "main".to_string();
            let mut workspace_slug = String::new();
            let mut mcp_session_id = None::<String>;
            let prev = existing_by_id.get(&trace_id);
            let mcp_for_state = prev
                .and_then(|p| p.mcp_session_id.clone())
                .or_else(|| evs.first().and_then(|e| e.mcp_session_id.clone()))
                .unwrap_or_default();
            let mut data = if let Some(p) = prev {
                if !p.totals_json.trim().is_empty() {
                    serde_json::from_str::<SessionTraceCountersSnapshot>(&p.totals_json)
                        .map(|s| s.into_session_data(mcp_for_state.clone()))
                        .unwrap_or_else(|_| SessionTraceData::new(mcp_for_state.clone()))
                } else {
                    SessionTraceData::new(mcp_for_state.clone())
                }
            } else {
                SessionTraceData::new(mcp_for_state.clone())
            };

            for ev in &evs {
                let ms = ev.emitted_at.timestamp_millis();
                min_ms = min_ms.min(ms);
                max_ms = max_ms.max(ms);
                max_call = max_call.max(ev.call_index);
                tenant_partition = ev.tenant_partition();
                tenant_id = ev
                    .tenant_id
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| tenant_partition.clone());
                let ps = ev.audit_project_slug();
                if !ps.is_empty() {
                    project_slug = ps;
                }
                let ws = ev.audit_workspace_slug();
                if !ws.is_empty() {
                    workspace_slug = ws;
                }
                if mcp_session_id.is_none() {
                    mcp_session_id = ev.mcp_session_id.clone();
                }
                if ev.event_kind == AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT {
                    if let Some(te) = crate::trace_event_decode::decode_mcp_trace_segment(
                        ev,
                        Some("trace head: could not decode TraceEvent from audit payload"),
                    ) {
                        data.apply_event_counters(&te);
                    }
                }
            }
            let started_at_ms = prev
                .map(|p| p.started_at_ms.min(min_ms))
                .unwrap_or(min_ms.max(0));
            let prev_max_call = prev.and_then(|p| p.max_call_index);
            let merged_max_call = prev_max_call.max(max_call);
            let batch_ended = max_ms.max(0);
            let merged_ended = Some(match prev.and_then(|p| p.ended_at_ms) {
                Some(pe) => pe.max(batch_ended),
                None => batch_ended,
            });
            let totals_json = serde_json::to_string(&SessionTraceCountersSnapshot::from(&data))
                .unwrap_or_default();
            rows.push(TraceHeadRow {
                trace_id,
                tenant_partition,
                tenant_id,
                project_slug,
                mcp_session_id,
                status: "completed".to_string(),
                started_at_ms,
                ended_at_ms: merged_ended,
                updated_at_ms: batch_ended,
                expression_lines: data.aggregate_expression_lines as i64,
                max_call_index: merged_max_call,
                totals_json,
                workspace_slug,
            });
        }
        let t_append = Instant::now();
        self.store
            .append_trace_heads(&rows)
            .instrument(tracing::debug_span!(
                "plasm_trace_sink.ingest.append_trace_heads",
                row_count = rows.len(),
            ))
            .await?;
        tracing::info!(
            target: "plasm_trace_sink::ingest_timing",
            phase = "append_trace_heads",
            elapsed_ms = t_append.elapsed().as_millis() as u64,
            row_count = rows.len(),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::unique_tenant_partitions;
    use crate::model::AuditEvent;
    use chrono::Utc;
    use uuid::Uuid;

    fn minimal_event(tenant_id: Option<&str>) -> AuditEvent {
        AuditEvent {
            event_id: Uuid::new_v4(),
            schema_version: 1,
            emitted_at: Utc::now(),
            ingested_at: Utc::now(),
            trace_id: Uuid::new_v4(),
            mcp_session_id: None,
            logical_session_id: None,
            plasm_prompt_hash: None,
            plasm_execute_session: None,
            run_id: None,
            call_index: None,
            line_index: None,
            tenant_id: tenant_id.map(|s| s.to_string()),
            principal_sub: None,
            workspace_slug: None,
            project_slug: None,
            event_kind: "mcp_trace_segment".to_string(),
            request_units: 0,
            payload: serde_json::json!({}),
        }
    }

    #[test]
    fn unique_tenant_partitions_dedupes_and_sorts() {
        let a = minimal_event(Some("t1"));
        let b = minimal_event(Some("t2"));
        let c = minimal_event(Some("t1"));
        let none = minimal_event(None);
        let out = unique_tenant_partitions(&[a, b, c, none]);
        assert_eq!(
            out,
            vec!["__none__".to_string(), "t1".to_string(), "t2".to_string()]
        );
    }
}
