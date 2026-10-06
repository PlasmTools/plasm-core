//! Composite [`AuditSpanStore`]: Iceberg lake writes + hot SQL projections on the same catalog DB.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;

use crate::append_port::{AuditSpanReader, AuditSpanWriter, TenantId, TimeWindow, TraceListFilter};
use crate::config::IcebergConnectParams;
use crate::iceberg_writer::IcebergSink;
use crate::metrics::{
    record_heads_backfill, record_list_summaries, record_load_trace_detail,
    record_projection_dedupe, record_projection_trace_heads, ListSummariesSource,
    LoadTraceDetailSource, ProjectionDedupeHits, ProjectionHeadRowCounts,
};
use crate::model::{AuditEvent, DurableTraceDetail, TraceHeadRow, TraceSpanRow, TraceSummary};
use crate::projection::ProjectionStore;
use crate::storage_error::TraceSinkStorageError;

/// Iceberg durability + Postgres projections (idempotency index, trace heads, list).
pub struct PersistedTraceSink {
    iceberg: Arc<IcebergSink>,
    projection: Arc<ProjectionStore>,
}

impl PersistedTraceSink {
    /// Open SQL projections on `params.catalog`, migrate, and optionally backfill `trace_heads`
    /// from Iceberg when the projection table is empty (existing lake, new deployment).
    pub async fn connect(
        params: &IcebergConnectParams,
        iceberg: Arc<IcebergSink>,
        segment_ttl_secs: u64,
        segment_gc_interval_secs: u64,
    ) -> Result<Arc<Self>, TraceSinkStorageError> {
        let projection = Arc::new(
            ProjectionStore::connect(
                params.catalog.as_str(),
                segment_ttl_secs,
                segment_gc_interval_secs,
            )
            .await?,
        );
        projection.migrate().await?;

        let n = projection.count_trace_heads().await?;
        if n == 0 {
            let heads = iceberg.scan_all_trace_heads().await?;
            projection.bulk_upsert_trace_heads(&heads).await?;
            record_heads_backfill(heads.len() as u64);
            tracing::info!(
                target: "plasm_trace_sink.projection",
                rows = heads.len(),
                "backfilled trace_heads projection from Iceberg (empty SQL table)"
            );
        }

        Ok(Arc::new(Self {
            iceberg,
            projection,
        }))
    }

    /// Background purge of expired `trace_segments` rows (no-op when TTL disabled).
    pub fn start_background_tasks(self: &Arc<Self>) {
        Arc::clone(&self.projection).spawn_segment_gc_loop();
    }
}

#[async_trait]
impl AuditSpanWriter for PersistedTraceSink {
    async fn append_audit_events(
        &self,
        events: &[AuditEvent],
    ) -> Result<(), TraceSinkStorageError> {
        self.iceberg.append_audit_events(events).await?;
        self.projection.insert_ingested_events(events).await?;
        self.projection.insert_trace_segments(events).await?;
        Ok(())
    }

    async fn append_trace_spans(&self, rows: &[TraceSpanRow]) -> Result<(), TraceSinkStorageError> {
        Ok(self.iceberg.append_trace_spans(rows).await?)
    }

    async fn append_audit_events_with_trace_spans(
        &self,
        events: &[AuditEvent],
        spans: &[TraceSpanRow],
    ) -> Result<(), TraceSinkStorageError> {
        self.iceberg
            .append_audit_events_with_trace_spans(events, spans)
            .await?;
        self.projection.insert_ingested_events(events).await?;
        self.projection.insert_trace_segments(events).await?;
        Ok(())
    }

    async fn append_trace_heads(&self, rows: &[TraceHeadRow]) -> Result<(), TraceSinkStorageError> {
        self.iceberg.append_trace_heads(rows).await?;
        self.projection.upsert_trace_heads(rows).await?;
        Ok(())
    }
}

#[async_trait]
impl AuditSpanReader for PersistedTraceSink {
    async fn existing_event_ids(
        &self,
        ids: &[uuid::Uuid],
        tenant_partitions: Option<&[String]>,
    ) -> Result<HashSet<uuid::Uuid>, TraceSinkStorageError> {
        let mut set = self
            .projection
            .existing_event_ids(ids, tenant_partitions)
            .await?;
        let sql_hits = set.len() as u64;
        let missing: Vec<uuid::Uuid> = ids.iter().copied().filter(|i| !set.contains(i)).collect();
        if missing.is_empty() {
            record_projection_dedupe(ProjectionDedupeHits {
                sql: sql_hits,
                lake: 0,
            });
            return Ok(set);
        }
        let ice = self
            .iceberg
            .existing_event_ids(&missing, tenant_partitions)
            .await?;
        let lake_hits = ice.len() as u64;
        record_projection_dedupe(ProjectionDedupeHits {
            sql: sql_hits,
            lake: lake_hits,
        });
        set.extend(ice);
        Ok(set)
    }

    async fn load_trace_events(
        &self,
        trace_id: uuid::Uuid,
    ) -> Result<Vec<AuditEvent>, TraceSinkStorageError> {
        Ok(self.iceberg.load_trace_events(trace_id).await?)
    }

    async fn load_trace_events_for_tenant(
        &self,
        tenant: &TenantId,
        trace_id: uuid::Uuid,
    ) -> Result<Vec<AuditEvent>, TraceSinkStorageError> {
        let tenant_part = tenant.as_str();
        let sql_head = self
            .projection
            .load_trace_head(tenant_part, trace_id)
            .await
            .ok()
            .flatten();
        self.iceberg
            .load_trace_events_for_tenant_with_head(tenant_part, trace_id, sql_head.as_ref())
            .await
            .map_err(Into::into)
    }

    async fn load_latest_trace_heads(
        &self,
        trace_ids: &[uuid::Uuid],
    ) -> Result<Vec<TraceHeadRow>, TraceSinkStorageError> {
        let mut from_sql = self.projection.load_latest_trace_heads(trace_ids).await?;
        let sql_rows = from_sql.len() as u64;
        let have: HashSet<uuid::Uuid> = from_sql.iter().map(|h| h.trace_id).collect();
        let missing: Vec<uuid::Uuid> = trace_ids
            .iter()
            .copied()
            .filter(|id| !have.contains(id))
            .collect();
        let mut lake_rows = 0u64;
        if !missing.is_empty() {
            let extra = self.iceberg.load_latest_trace_heads(&missing).await?;
            lake_rows = extra.len() as u64;
            from_sql.extend(extra);
        }
        record_projection_trace_heads(ProjectionHeadRowCounts {
            sql: sql_rows,
            lake: lake_rows,
        });
        Ok(from_sql)
    }

    async fn load_billing_usage_scoped(
        &self,
        tenant: &TenantId,
        window: TimeWindow,
    ) -> Result<Vec<TraceSpanRow>, TraceSinkStorageError> {
        Ok(self
            .iceberg
            .load_billing_usage_scoped(tenant, window)
            .await?)
    }

    async fn load_billing_usage_global(
        &self,
        window: TimeWindow,
    ) -> Result<Vec<TraceSpanRow>, TraceSinkStorageError> {
        Ok(self.iceberg.load_billing_usage_global(window).await?)
    }

    async fn list_trace_summaries(
        &self,
        filter: TraceListFilter<'_>,
    ) -> Result<Vec<TraceSummary>, TraceSinkStorageError> {
        match self.projection.list_trace_summaries(filter).await {
            Ok(rows) => {
                record_list_summaries(ListSummariesSource::Projection);
                Ok(rows)
            }
            Err(e) => {
                tracing::warn!(
                    target: "plasm_trace_sink.projection",
                    error = %e,
                    "list_trace_summaries: projection query failed; falling back to Iceberg scan"
                );
                record_list_summaries(ListSummariesSource::IcebergFallback);
                Ok(self.iceberg.list_trace_summaries(filter).await?)
            }
        }
    }

    async fn load_trace_detail(
        &self,
        tenant: &TenantId,
        trace_id: uuid::Uuid,
    ) -> Result<Option<DurableTraceDetail>, TraceSinkStorageError> {
        let tenant_part = tenant.as_str();
        match self
            .projection
            .load_trace_detail(tenant_part, trace_id)
            .await
        {
            Ok(Some(detail)) => {
                record_load_trace_detail(LoadTraceDetailSource::Projection);
                return Ok(Some(detail));
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    target: "plasm_trace_sink.projection",
                    error = %e,
                    %trace_id,
                    "load_trace_detail: projection query failed; falling back to Iceberg"
                );
            }
        }

        let sql_head = self
            .projection
            .load_trace_head(tenant_part, trace_id)
            .await
            .ok()
            .flatten();
        let detail = self
            .iceberg
            .load_trace_detail_with_head(tenant, trace_id, sql_head.as_ref())
            .await?;
        record_load_trace_detail(LoadTraceDetailSource::IcebergFallback);
        if let Some(ref d) = detail {
            if self.projection.summary_within_segment_ttl(&d.summary) {
                if let Err(e) = self
                    .projection
                    .backfill_trace_segments_from_detail(tenant_part, trace_id, &d.records)
                    .await
                {
                    tracing::warn!(
                        target: "plasm_trace_sink.projection",
                        error = %e,
                        %trace_id,
                        "load_trace_detail: segment backfill after Iceberg read failed"
                    );
                }
            }
        }
        Ok(detail)
    }
}
