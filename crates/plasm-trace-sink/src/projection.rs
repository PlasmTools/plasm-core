//! Hot projections on the same SqlCatalog Postgres database as Iceberg (`plasm_trace_sink` schema).
//! Iceberg remains the durable lake; this layer accelerates idempotency, trace head lookups, and listing.
//!
//! **`trace_segments`** is a bounded hot cache for detail reads. When
//! [`ProjectionStore::segment_ttl_secs`] is non-zero, traces older than the TTL fall back to Iceberg
//! and expired rows are purged in the background.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use sqlx::Row;
use uuid::Uuid;

use crate::append_port::{TraceListFilter, TraceListStatusFilter};
use crate::iceberg_writer::trace_detail_record_from_audit_event;
use crate::metrics::record_segment_projection_gc;
use crate::model::{
    AuditEvent, DurableTraceDetail, TraceDetailRecord, TraceHeadRow, TraceSummary, TraceTotals,
    AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT,
};
use crate::trace_totals::trace_totals_from_head_row;

/// Connection to projection tables (same Postgres as JanKaul SqlCatalog).
pub struct ProjectionStore {
    pool: PgPool,
    /// `0` = no TTL (segments kept until manual purge).
    segment_ttl_secs: u64,
    segment_gc_interval_secs: u64,
}

impl ProjectionStore {
    /// Connect using the same JDBC URL as Iceberg SqlCatalog (`postgresql://…` / `postgres://…` only).
    pub async fn connect(
        catalog_url: &str,
        segment_ttl_secs: u64,
        segment_gc_interval_secs: u64,
    ) -> anyhow::Result<Self> {
        let url = catalog_url.trim();
        if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
            anyhow::bail!(
                "trace projections require a Postgres SqlCatalog URL (postgresql:// or postgres://); sqlite metadata is not supported (got {}).",
                url.chars().take(48).collect::<String>()
            );
        }
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect(url)
            .await?;
        Ok(Self {
            pool,
            segment_ttl_secs,
            segment_gc_interval_secs,
        })
    }

    #[must_use]
    pub fn segment_ttl_enabled(&self) -> bool {
        self.segment_ttl_secs > 0
    }

    /// Spawn periodic `trace_segments` purge when TTL is enabled.
    pub fn spawn_segment_gc_loop(self: Arc<Self>) {
        if !self.segment_ttl_enabled() {
            return;
        }
        let interval_secs = self.segment_gc_interval_secs;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
            interval.tick().await;
            loop {
                interval.tick().await;
                match self.purge_expired_trace_segments().await {
                    Ok(n) => {
                        record_segment_projection_gc(n);
                        if n > 0 {
                            tracing::info!(
                                target: "plasm_trace_sink.projection",
                                rows = n,
                                ttl_secs = self.segment_ttl_secs,
                                "purged expired trace_segments projection rows"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "plasm_trace_sink.projection",
                            error = %e,
                            "trace_segments TTL purge failed"
                        );
                    }
                }
            }
        });
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        migrate_postgres(&self.pool).await
    }

    pub async fn count_trace_heads(&self) -> anyhow::Result<i64> {
        let v: i64 =
            sqlx::query_scalar("SELECT COUNT(*)::bigint FROM plasm_trace_sink.trace_heads")
                .fetch_one(&self.pool)
                .await?;
        Ok(v)
    }

    pub async fn bulk_upsert_trace_heads(&self, rows: &[TraceHeadRow]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        for chunk in rows.chunks(256) {
            self.upsert_trace_heads(chunk).await?;
        }
        Ok(())
    }

    pub async fn insert_ingested_events(
        &self,
        events: &[crate::model::AuditEvent],
    ) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for ev in events {
            sqlx::query(
                r#"INSERT INTO plasm_trace_sink.ingested_events (event_id, tenant_partition, ingested_at)
                   VALUES ($1, $2, $3)
                   ON CONFLICT (event_id) DO NOTHING"#,
            )
            .bind(ev.event_id)
            .bind(ev.tenant_partition())
            .bind(ev.ingested_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn existing_event_ids(
        &self,
        ids: &[Uuid],
        tenant_partitions: Option<&[String]>,
    ) -> anyhow::Result<HashSet<Uuid>> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let mut out = HashSet::new();
        let p = &self.pool;
        for chunk in ids.chunks(512) {
            let ids_vec = chunk.to_vec();
            let rows: Vec<Uuid> = match tenant_partitions {
                Some(parts) if !parts.is_empty() => {
                    let parts_vec: Vec<String> = parts.to_vec();
                    sqlx::query_scalar(
                        r#"SELECT event_id FROM plasm_trace_sink.ingested_events
                           WHERE event_id = ANY($1) AND tenant_partition = ANY($2)"#,
                    )
                    .bind(ids_vec)
                    .bind(parts_vec)
                    .fetch_all(p)
                    .await?
                }
                _ => {
                    sqlx::query_scalar(
                        r#"SELECT event_id FROM plasm_trace_sink.ingested_events
                           WHERE event_id = ANY($1)"#,
                    )
                    .bind(ids_vec)
                    .fetch_all(p)
                    .await?
                }
            };
            out.extend(rows);
        }
        Ok(out)
    }

    pub async fn upsert_trace_heads(&self, rows: &[TraceHeadRow]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for h in rows {
            sqlx::query(
                r#"INSERT INTO plasm_trace_sink.trace_heads (
                    trace_id, tenant_partition, tenant_id, project_slug, mcp_session_id,
                    status, started_at_ms, ended_at_ms, updated_at_ms, expression_lines,
                    max_call_index, totals_json, workspace_slug
                ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
                ON CONFLICT (trace_id) DO UPDATE SET
                    tenant_partition = EXCLUDED.tenant_partition,
                    tenant_id = EXCLUDED.tenant_id,
                    project_slug = EXCLUDED.project_slug,
                    mcp_session_id = EXCLUDED.mcp_session_id,
                    status = EXCLUDED.status,
                    started_at_ms = EXCLUDED.started_at_ms,
                    ended_at_ms = EXCLUDED.ended_at_ms,
                    updated_at_ms = EXCLUDED.updated_at_ms,
                    expression_lines = EXCLUDED.expression_lines,
                    max_call_index = EXCLUDED.max_call_index,
                    totals_json = EXCLUDED.totals_json,
                    workspace_slug = EXCLUDED.workspace_slug"#,
            )
            .bind(h.trace_id)
            .bind(&h.tenant_partition)
            .bind(&h.tenant_id)
            .bind(&h.project_slug)
            .bind(&h.mcp_session_id)
            .bind(&h.status)
            .bind(h.started_at_ms)
            .bind(h.ended_at_ms)
            .bind(h.updated_at_ms)
            .bind(h.expression_lines)
            .bind(h.max_call_index)
            .bind(&h.totals_json)
            .bind(&h.workspace_slug)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn load_latest_trace_heads(
        &self,
        trace_ids: &[Uuid],
    ) -> anyhow::Result<Vec<TraceHeadRow>> {
        if trace_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for chunk in trace_ids.chunks(256) {
            let q = "SELECT trace_id, tenant_partition, tenant_id, project_slug, mcp_session_id, \
                     status, started_at_ms, ended_at_ms, updated_at_ms, expression_lines, \
                     max_call_index, totals_json, workspace_slug \
                     FROM plasm_trace_sink.trace_heads WHERE trace_id = ANY($1)";
            let rows = sqlx::query(q)
                .bind(chunk.to_vec())
                .fetch_all(&self.pool)
                .await?;
            for r in rows {
                out.push(row_to_head_pg(&r)?);
            }
        }
        Ok(out)
    }

    pub async fn list_trace_summaries(
        &self,
        filter: TraceListFilter<'_>,
    ) -> anyhow::Result<Vec<TraceSummary>> {
        let tenant = filter.tenant.as_str();
        let limit = filter.limit.clamp(1, 500) as i64;
        let offset = filter.offset as i64;
        let project_filter = filter.project_slug.filter(|s| !s.is_empty());
        let status_filter: Option<&str> = match filter.status {
            TraceListStatusFilter::All => None,
            TraceListStatusFilter::Live => Some("live"),
            TraceListStatusFilter::Completed => Some("completed"),
        };

        let sql = r#"SELECT trace_id, tenant_partition, tenant_id, project_slug, mcp_session_id, status,
             started_at_ms, ended_at_ms, updated_at_ms, expression_lines, max_call_index, totals_json,
             workspace_slug
             FROM (
               SELECT trace_id, tenant_partition, tenant_id, project_slug, mcp_session_id, status,
                      started_at_ms, ended_at_ms, updated_at_ms, expression_lines, max_call_index, totals_json,
                      workspace_slug,
                      ROW_NUMBER() OVER (PARTITION BY trace_id ORDER BY updated_at_ms DESC) AS rn
               FROM plasm_trace_sink.trace_heads
               WHERE tenant_partition = $1
                 AND ($2::text IS NULL OR project_slug = $2)
                 AND ($3::text IS NULL OR status = $3)
             ) latest
             WHERE rn = 1
             ORDER BY started_at_ms DESC
             OFFSET $4 LIMIT $5"#;
        let rows = sqlx::query(sql)
            .bind(tenant)
            .bind(project_filter)
            .bind(status_filter)
            .bind(offset)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        let mut summaries = Vec::with_capacity(rows.len());
        for r in rows {
            let h = row_to_head_pg(&r)?;
            let totals: TraceTotals = trace_totals_from_head_row(&h);
            summaries.push(TraceSummary {
                trace_id: h.trace_id,
                mcp_session_id: h.mcp_session_id.unwrap_or_default(),
                status: h.status,
                started_at_ms: h.started_at_ms.max(0) as u64,
                ended_at_ms: h.ended_at_ms.map(|v| v.max(0) as u64),
                project_slug: h.project_slug,
                tenant_id: h.tenant_id,
                totals,
            });
        }
        Ok(summaries)
    }

    /// Hot read path for `GET /v1/traces/:id` — returns `None` when the head or segment rows are missing.
    pub async fn load_trace_detail(
        &self,
        tenant_partition: &str,
        trace_id: Uuid,
    ) -> anyhow::Result<Option<DurableTraceDetail>> {
        let head = self.load_trace_head(tenant_partition, trace_id).await?;
        let Some(head) = head else {
            return Ok(None);
        };
        if !self.trace_head_within_segment_ttl(&head) {
            return Ok(None);
        }
        let records = self
            .load_trace_segment_records(trace_id, tenant_partition)
            .await?;
        if records.is_empty() {
            return Ok(None);
        }
        let record_values: Vec<serde_json::Value> =
            records.iter().map(|r| r.record.clone()).collect();
        let totals =
            crate::trace_totals::trace_totals_from_head_or_records(&head, &record_values);
        Ok(Some(DurableTraceDetail {
            summary: TraceSummary {
                trace_id,
                mcp_session_id: head.mcp_session_id.unwrap_or_default(),
                status: head.status,
                started_at_ms: head.started_at_ms.max(0) as u64,
                ended_at_ms: head.ended_at_ms.map(|v| v.max(0) as u64),
                project_slug: head.project_slug,
                tenant_id: head.tenant_id,
                totals,
            },
            records,
        }))
    }

    pub async fn insert_trace_segments(&self, events: &[AuditEvent]) -> anyhow::Result<()> {
        let segment_events: Vec<&AuditEvent> = events
            .iter()
            .filter(|e| e.event_kind == AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT)
            .collect();
        if segment_events.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for ev in segment_events {
            let Some(detail_rec) = trace_detail_record_from_audit_event(ev) else {
                continue;
            };
            let record_json = serde_json::to_string(&detail_rec.record)?;
            sqlx::query(
                r#"INSERT INTO plasm_trace_sink.trace_segments (
                    event_id, trace_id, tenant_partition, emitted_at, call_index, line_index,
                    sort_key, record_kind, record_json
                ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
                ON CONFLICT (event_id) DO NOTHING"#,
            )
            .bind(ev.event_id)
            .bind(ev.trace_id)
            .bind(ev.tenant_partition())
            .bind(ev.emitted_at)
            .bind(ev.call_index)
            .bind(ev.line_index)
            .bind(ev.emitted_at.timestamp_millis())
            .bind(&detail_rec.kind)
            .bind(record_json)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// After an Iceberg cold read, persist segment rows so the next detail fetch hits SQL.
    pub async fn backfill_trace_segments_from_detail(
        &self,
        tenant_partition: &str,
        trace_id: Uuid,
        records: &[TraceDetailRecord],
    ) -> anyhow::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for rec in records {
            let event_id = rec
                .record
                .get("event_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .unwrap_or_else(Uuid::new_v4);
            let emitted_at = rec
                .record
                .get("emitted_at")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(chrono::Utc::now);
            let call_index = rec.record.get("call_index").and_then(|v| v.as_i64());
            let line_index = rec.record.get("line_index").and_then(|v| v.as_i64());
            let record_json = serde_json::to_string(&rec.record)?;
            sqlx::query(
                r#"INSERT INTO plasm_trace_sink.trace_segments (
                    event_id, trace_id, tenant_partition, emitted_at, call_index, line_index,
                    sort_key, record_kind, record_json
                ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
                ON CONFLICT (event_id) DO NOTHING"#,
            )
            .bind(event_id)
            .bind(trace_id)
            .bind(tenant_partition)
            .bind(emitted_at)
            .bind(call_index)
            .bind(line_index)
            .bind(emitted_at.timestamp_millis())
            .bind(&rec.kind)
            .bind(record_json)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Whether an Iceberg cold read should warm `trace_segments` (skip when outside TTL).
    #[must_use]
    pub fn summary_within_segment_ttl(&self, summary: &TraceSummary) -> bool {
        summary_within_segment_ttl(self.segment_ttl_secs, summary)
    }

    pub async fn purge_expired_trace_segments(&self) -> anyhow::Result<u64> {
        let Some(cutoff) = segment_ttl_cutoff(self.segment_ttl_secs) else {
            return Ok(0);
        };
        let r = sqlx::query("DELETE FROM plasm_trace_sink.trace_segments WHERE emitted_at < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    fn trace_head_within_segment_ttl(&self, head: &TraceHeadRow) -> bool {
        trace_head_within_segment_ttl(self.segment_ttl_secs, head)
    }

    pub async fn load_trace_head(
        &self,
        tenant_partition: &str,
        trace_id: Uuid,
    ) -> anyhow::Result<Option<TraceHeadRow>> {
        let row = sqlx::query(
            "SELECT trace_id, tenant_partition, tenant_id, project_slug, mcp_session_id, \
             status, started_at_ms, ended_at_ms, updated_at_ms, expression_lines, \
             max_call_index, totals_json, workspace_slug \
             FROM plasm_trace_sink.trace_heads \
             WHERE trace_id = $1 AND tenant_partition = $2",
        )
        .bind(trace_id)
        .bind(tenant_partition)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_head_pg(&r)).transpose()
    }

    async fn load_trace_segment_records(
        &self,
        trace_id: Uuid,
        tenant_partition: &str,
    ) -> anyhow::Result<Vec<TraceDetailRecord>> {
        let rows = sqlx::query(
            r#"SELECT record_kind, record_json FROM plasm_trace_sink.trace_segments
               WHERE trace_id = $1 AND tenant_partition = $2
               ORDER BY sort_key ASC, call_index ASC NULLS LAST, line_index ASC NULLS LAST"#,
        )
        .bind(trace_id)
        .bind(tenant_partition)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let kind: String = r.try_get("record_kind")?;
            let json: String = r.try_get("record_json")?;
            let record: serde_json::Value = serde_json::from_str(&json)?;
            out.push(TraceDetailRecord { kind, record });
        }
        Ok(out)
    }
}

fn row_to_head_pg(r: &sqlx::postgres::PgRow) -> anyhow::Result<TraceHeadRow> {
    Ok(TraceHeadRow {
        trace_id: r.try_get::<Uuid, _>("trace_id")?,
        tenant_partition: r.try_get::<String, _>("tenant_partition")?,
        tenant_id: r.try_get::<String, _>("tenant_id")?,
        project_slug: r.try_get::<String, _>("project_slug")?,
        mcp_session_id: r.try_get::<Option<String>, _>("mcp_session_id")?,
        status: r.try_get::<String, _>("status")?,
        started_at_ms: r.try_get::<i64, _>("started_at_ms")?,
        ended_at_ms: r.try_get::<Option<i64>, _>("ended_at_ms")?,
        updated_at_ms: r.try_get::<i64, _>("updated_at_ms")?,
        expression_lines: r.try_get::<i64, _>("expression_lines")?,
        max_call_index: r.try_get::<Option<i64>, _>("max_call_index")?,
        totals_json: r
            .try_get::<Option<String>, _>("totals_json")?
            .unwrap_or_default(),
        workspace_slug: r
            .try_get::<Option<String>, _>("workspace_slug")?
            .unwrap_or_default(),
    })
}

async fn migrate_postgres(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    sqlx::query("CREATE SCHEMA IF NOT EXISTS plasm_trace_sink")
        .execute(pool)
        .await?;
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS plasm_trace_sink.ingested_events (
            event_id UUID PRIMARY KEY,
            tenant_partition TEXT NOT NULL,
            ingested_at TIMESTAMPTZ NOT NULL
        )"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS ix_plasm_tsink_ev_tenant
           ON plasm_trace_sink.ingested_events (tenant_partition)"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS plasm_trace_sink.trace_heads (
            trace_id UUID PRIMARY KEY,
            tenant_partition TEXT NOT NULL,
            tenant_id TEXT NOT NULL,
            project_slug TEXT NOT NULL,
            mcp_session_id TEXT,
            status TEXT NOT NULL,
            started_at_ms BIGINT NOT NULL,
            ended_at_ms BIGINT,
            updated_at_ms BIGINT NOT NULL,
            expression_lines BIGINT NOT NULL,
            max_call_index BIGINT,
            totals_json TEXT NOT NULL DEFAULT '',
            workspace_slug TEXT NOT NULL DEFAULT ''
        )"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"ALTER TABLE plasm_trace_sink.trace_heads
           ADD COLUMN IF NOT EXISTS workspace_slug TEXT NOT NULL DEFAULT ''"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS ix_plasm_tsink_heads_tenant_proj
           ON plasm_trace_sink.trace_heads (tenant_partition, project_slug)"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS plasm_trace_sink.trace_segments (
            event_id UUID PRIMARY KEY,
            trace_id UUID NOT NULL,
            tenant_partition TEXT NOT NULL,
            emitted_at TIMESTAMPTZ NOT NULL,
            call_index BIGINT,
            line_index BIGINT,
            sort_key BIGINT NOT NULL,
            record_kind TEXT NOT NULL,
            record_json TEXT NOT NULL
        )"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS ix_plasm_tsink_seg_trace
           ON plasm_trace_sink.trace_segments (trace_id, tenant_partition)"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS ix_plasm_tsink_seg_emitted_at
           ON plasm_trace_sink.trace_segments (emitted_at)"#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

fn segment_ttl_cutoff(ttl_secs: u64) -> Option<DateTime<Utc>> {
    if ttl_secs == 0 {
        return None;
    }
    Some(Utc::now() - chrono::Duration::seconds(ttl_secs as i64))
}

fn trace_head_within_segment_ttl(ttl_secs: u64, head: &TraceHeadRow) -> bool {
    let Some(cutoff) = segment_ttl_cutoff(ttl_secs) else {
        return true;
    };
    head.updated_at_ms >= cutoff.timestamp_millis()
}

fn summary_within_segment_ttl(ttl_secs: u64, summary: &TraceSummary) -> bool {
    let Some(cutoff) = segment_ttl_cutoff(ttl_secs) else {
        return true;
    };
    let anchor_ms = summary.ended_at_ms.unwrap_or(summary.started_at_ms) as i64;
    anchor_ms >= cutoff.timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn head(updated_at_ms: i64) -> TraceHeadRow {
        TraceHeadRow {
            trace_id: Uuid::new_v4(),
            tenant_partition: "t".into(),
            tenant_id: "t".into(),
            project_slug: "main".into(),
            mcp_session_id: None,
            status: "completed".into(),
            started_at_ms: updated_at_ms - 1000,
            ended_at_ms: Some(updated_at_ms),
            updated_at_ms,
            expression_lines: 1,
            max_call_index: Some(0),
            totals_json: String::new(),
            workspace_slug: String::new(),
        }
    }

    #[test]
    fn trace_head_ttl_disabled_always_hot() {
        let h = head(0);
        assert!(trace_head_within_segment_ttl(0, &h));
    }

    #[test]
    fn trace_head_outside_ttl_is_cold() {
        let old_ms = Utc::now().timestamp_millis() - 86400 * 1000;
        assert!(!trace_head_within_segment_ttl(3600, &head(old_ms)));
    }
}
