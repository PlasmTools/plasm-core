//! Storage ports: append vs query are separate traits; [`AuditSpanStore`] is their intersection.

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use crate::model::{AuditEvent, DurableTraceDetail, TraceHeadRow, TraceSpanRow, TraceSummary};
pub use crate::storage_error::TraceSinkStorageError;

#[derive(Clone, Debug)]
pub struct TenantId(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TenantIdError {
    #[error("tenant identifier must be non-empty")]
    Empty,
}

impl TenantId {
    pub fn parse(raw: &str) -> Result<Self, TenantIdError> {
        let v = raw.trim();
        if v.is_empty() {
            return Err(TenantIdError::Empty);
        }
        Ok(Self(v.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TimeWindow {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TimeWindowError {
    #[error("time window start must not be after its end")]
    Reversed,
}

impl TimeWindow {
    pub fn new(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Self, TimeWindowError> {
        if from > to {
            return Err(TimeWindowError::Reversed);
        }
        Ok(Self { from, to })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TraceListStatusFilter {
    All,
    Live,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TraceListStatusFilterError {
    #[error("unsupported trace status filter `{value}`")]
    Unsupported { value: String },
}

impl TraceListStatusFilter {
    pub fn parse(raw: Option<&str>) -> Result<Self, TraceListStatusFilterError> {
        match raw.unwrap_or("all").to_ascii_lowercase().as_str() {
            "all" | "" => Ok(Self::All),
            "live" => Ok(Self::Live),
            "completed" => Ok(Self::Completed),
            other => Err(TraceListStatusFilterError::Unsupported {
                value: other.to_owned(),
            }),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TraceListFilter<'a> {
    pub tenant: &'a TenantId,
    pub project_slug: Option<&'a str>,
    pub status: TraceListStatusFilter,
    pub offset: usize,
    pub limit: usize,
}

/// Append-only: Parquet/Iceberg writes for `audit_events` and `trace_spans`.
#[async_trait]
pub trait AuditSpanWriter: Send + Sync {
    async fn append_audit_events(&self, events: &[AuditEvent])
        -> Result<(), TraceSinkStorageError>;
    async fn append_trace_spans(&self, rows: &[TraceSpanRow]) -> Result<(), TraceSinkStorageError>;
    /// Append `audit_events` then `trace_spans` under one storage lock so readers never see audit-only gaps.
    async fn append_audit_events_with_trace_spans(
        &self,
        events: &[AuditEvent],
        spans: &[TraceSpanRow],
    ) -> Result<(), TraceSinkStorageError>;
    async fn append_trace_heads(&self, rows: &[TraceHeadRow]) -> Result<(), TraceSinkStorageError>;
}

/// Read path: idempotency checks and HTTP GET backends.
#[async_trait]
pub trait AuditSpanReader: Send + Sync {
    /// Subset of `ids` already present in `audit_events`.
    ///
    /// When `tenant_partitions` is `Some` with one or more values (within the implementation cap),
    /// the query adds `AND tenant_partition IN (...)` so Iceberg can prune to those partitions.
    /// Pass `None` to scan all partitions (e.g. tests or callers without partition context).
    async fn existing_event_ids(
        &self,
        ids: &[Uuid],
        tenant_partitions: Option<&[String]>,
    ) -> Result<HashSet<Uuid>, TraceSinkStorageError>;

    /// Audit rows for `trace_id`, ordered by `emitted_at`, `call_index`, `line_index`.
    async fn load_trace_events(
        &self,
        trace_id: Uuid,
    ) -> Result<Vec<AuditEvent>, TraceSinkStorageError>;

    /// Tenant-scoped segment events for `trace_id` (head-guided month pruning when supported).
    async fn load_trace_events_for_tenant(
        &self,
        tenant: &TenantId,
        trace_id: Uuid,
    ) -> Result<Vec<AuditEvent>, TraceSinkStorageError>;
    async fn load_latest_trace_heads(
        &self,
        trace_ids: &[Uuid],
    ) -> Result<Vec<TraceHeadRow>, TraceSinkStorageError>;

    /// Billing-eligible spans in `[from, to]` scoped to one tenant.
    async fn load_billing_usage_scoped(
        &self,
        tenant: &TenantId,
        window: TimeWindow,
    ) -> Result<Vec<TraceSpanRow>, TraceSinkStorageError>;

    /// Privileged global billing usage in `[from, to]` across all tenants.
    async fn load_billing_usage_global(
        &self,
        window: TimeWindow,
    ) -> Result<Vec<TraceSpanRow>, TraceSinkStorageError>;

    /// Durable trace summaries by tenant and optional project filter.
    async fn list_trace_summaries(
        &self,
        filter: TraceListFilter<'_>,
    ) -> Result<Vec<TraceSummary>, TraceSinkStorageError>;

    /// Durable trace detail for one trace in tenant scope.
    async fn load_trace_detail(
        &self,
        tenant: &TenantId,
        trace_id: Uuid,
    ) -> Result<Option<DurableTraceDetail>, TraceSinkStorageError>;
}

/// Full sink capability for [`crate::state::AppState`] (`Arc<dyn AuditSpanStore>`).
pub trait AuditSpanStore: AuditSpanWriter + AuditSpanReader {}

impl<T> AuditSpanStore for T where T: AuditSpanWriter + AuditSpanReader + ?Sized {}
