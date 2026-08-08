//! Sink-specific projection row types; wire DTOs live in [`plasm_trace_wire`].

pub use plasm_trace_wire::{
    AuditEvent, DurableTraceDetail, IngestBatchRequest, IngestBatchResponse, TraceDetailRecord,
    TraceDetailResponse, TraceGetResponse, TraceListResponse, TraceSummary, TraceTotals,
    AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT, SCHEMA_VERSION,
};

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `YYYYMM` UTC (e.g. `202604`) for Iceberg identity partition / pruning.
pub fn year_month_bucket_utc(dt: DateTime<Utc>) -> i32 {
    dt.year() * 100 + dt.month() as i32
}

/// Distinct `year_month_bucket` values spanning a trace's wall-time window (UTC).
///
/// Used to add `year_month_bucket IN (...)` when reading `audit_events` from Iceberg so
/// detail scans touch only the monthly partitions the trace actually used (typically one).
pub fn year_month_buckets_for_trace_ms(started_at_ms: i64, ended_at_ms: Option<i64>) -> Vec<i32> {
    let start_ms = started_at_ms.max(0);
    let end_ms = ended_at_ms.unwrap_or(started_at_ms).max(start_ms);
    let start = ms_to_utc_datetime(start_ms);
    let end = ms_to_utc_datetime(end_ms);
    let mut buckets = Vec::new();
    let (mut y, mut m) = (start.year(), start.month());
    let (end_y, end_m) = (end.year(), end.month());
    loop {
        buckets.push(y * 100 + m as i32);
        if y == end_y && m == end_m {
            break;
        }
        if m == 12 {
            y += 1;
            m = 1;
        } else {
            m += 1;
        }
    }
    buckets
}

fn ms_to_utc_datetime(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod year_month_tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn single_month_trace_one_bucket() {
        let t = Utc.with_ymd_and_hms(2026, 4, 7, 10, 0, 0).unwrap();
        let ms = t.timestamp_millis();
        assert_eq!(year_month_buckets_for_trace_ms(ms, Some(ms)), vec![202604]);
    }

    #[test]
    fn cross_month_trace_two_buckets() {
        let start = Utc.with_ymd_and_hms(2026, 3, 31, 23, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 4, 1, 1, 0, 0).unwrap();
        assert_eq!(
            year_month_buckets_for_trace_ms(start.timestamp_millis(), Some(end.timestamp_millis())),
            vec![202603, 202604]
        );
    }
}

/// Denormalized span row for fast trace reads + billing projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSpanRow {
    pub span_id: Uuid,
    pub event_id: Uuid,
    pub trace_id: Uuid,
    pub emitted_at: chrono::DateTime<chrono::Utc>,
    /// Coalesced for partitioning (`__none__` when tenant unknown).
    pub tenant_partition: String,
    pub mcp_session_id: Option<String>,
    pub plasm_prompt_hash: Option<String>,
    pub plasm_execute_session: Option<String>,
    pub run_id: Option<String>,
    pub call_index: Option<i64>,
    pub line_index: Option<i64>,
    pub span_name: String,
    pub is_billing_event: bool,
    pub billing_event_type: Option<String>,
    pub request_units: i64,
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub api_entry_id: Option<String>,
    #[serde(default)]
    pub capability: Option<String>,
    #[serde(default)]
    pub attributes_json: serde_json::Value,
}

/// Append-only durable projection row for latest trace summary snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceHeadRow {
    pub trace_id: Uuid,
    pub tenant_partition: String,
    pub tenant_id: String,
    pub project_slug: String,
    pub mcp_session_id: Option<String>,
    pub status: String,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub updated_at_ms: i64,
    pub expression_lines: i64,
    pub max_call_index: Option<i64>,
    /// JSON [`plasm_trace::SessionTraceCountersSnapshot`] for incremental list totals.
    #[serde(default)]
    pub totals_json: String,
    #[serde(default)]
    pub workspace_slug: String,
}

#[derive(Debug, Serialize)]
pub struct BillingUsageResponse {
    pub usage: Vec<TraceSpanRow>,
}
