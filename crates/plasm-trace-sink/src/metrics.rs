//! OpenTelemetry metrics for `plasm-trace-sink` (ingest + SQL projection vs Iceberg fallbacks).
//!
//! Meter name: `plasm-trace-sink`. Metric names follow `plasm.trace_sink.*` (see
//! `docs/otel-signoz-metrics-inventory.md`).

use std::sync::OnceLock;

use opentelemetry::global;
use opentelemetry::metrics::Counter;
use opentelemetry::KeyValue;

const METER_NAME: &str = "plasm-trace-sink";

struct SinkMetrics {
    ingest_batches: Counter<u64>,
    ingest_events_in_batch: Counter<u64>,
    ingest_events_accepted: Counter<u64>,
    ingest_events_duplicate_skipped: Counter<u64>,
    projection_dedupe_sql_hits: Counter<u64>,
    projection_dedupe_lake_hits: Counter<u64>,
    projection_heads_sql_rows: Counter<u64>,
    projection_heads_lake_rows: Counter<u64>,
    projection_list_summaries: Counter<u64>,
    projection_load_trace_detail: Counter<u64>,
    iceberg_detail_prune_fallback: Counter<u64>,
    projection_heads_backfill_rows: Counter<u64>,
    segment_projection_gc_rows: Counter<u64>,
    trace_event_deserialize_failed: Counter<u64>,
    trace_event_detail_serialize_failed: Counter<u64>,
}

static SINK_METRICS: OnceLock<SinkMetrics> = OnceLock::new();

fn sink_metrics() -> &'static SinkMetrics {
    SINK_METRICS.get_or_init(|| {
        let m = global::meter(METER_NAME);
        SinkMetrics {
            ingest_batches: m
                .u64_counter("plasm.trace_sink.ingest.batches_total")
                .with_description("POST /v1/events requests processed (one batch per request).")
                .build(),
            ingest_events_in_batch: m
                .u64_counter("plasm.trace_sink.ingest.events_in_batch_total")
                .with_description("Audit events in ingest request bodies.")
                .build(),
            ingest_events_accepted: m
                .u64_counter("plasm.trace_sink.ingest.events_accepted_total")
                .with_description("Events appended to the lake after idempotency.")
                .build(),
            ingest_events_duplicate_skipped: m
                .u64_counter("plasm.trace_sink.ingest.events_duplicate_skipped_total")
                .with_description("Events skipped as duplicates (in-batch or historical).")
                .build(),
            projection_dedupe_sql_hits: m
                .u64_counter("plasm.trace_sink.projection.dedupe_ids.sql_hits_total")
                .with_description(
                    "Event IDs resolved as duplicates from SQL projection (per idempotency query).",
                )
                .build(),
            projection_dedupe_lake_hits: m
                .u64_counter("plasm.trace_sink.projection.dedupe_ids.lake_hits_total")
                .with_description(
                    "Event IDs resolved as duplicates from Iceberg after SQL miss (cold path).",
                )
                .build(),
            projection_heads_sql_rows: m
                .u64_counter("plasm.trace_sink.projection.trace_heads.rows_from_sql_total")
                .with_description("Trace head rows returned from SQL projection.")
                .build(),
            projection_heads_lake_rows: m
                .u64_counter("plasm.trace_sink.projection.trace_heads.rows_from_lake_total")
                .with_description("Trace head rows filled from Iceberg after SQL partial miss.")
                .build(),
            projection_list_summaries: m
                .u64_counter("plasm.trace_sink.projection.list_summaries.calls_total")
                .with_description("GET /v1/traces list: projection query vs Iceberg fallback.")
                .build(),
            projection_load_trace_detail: m
                .u64_counter("plasm.trace_sink.projection.load_trace_detail.calls_total")
                .with_description("GET /v1/traces/:id detail: projection query vs Iceberg fallback.")
                .build(),
            iceberg_detail_prune_fallback: m
                .u64_counter("plasm.trace_sink.iceberg.detail_prune_fallback_total")
                .with_description(
                    "Iceberg detail reads that retried without year_month_bucket after pruned scan returned empty.",
                )
                .build(),
            projection_heads_backfill_rows: m
                .u64_counter("plasm.trace_sink.projection.heads_backfill.rows_total")
                .with_description("Trace heads copied from Iceberg into empty SQL on startup.")
                .build(),
            segment_projection_gc_rows: m
                .u64_counter("plasm.trace_sink.projection.segment_gc.rows_purged_total")
                .with_description("Expired trace_segments rows deleted by TTL background GC.")
                .build(),
            trace_event_deserialize_failed: m
                .u64_counter("plasm.trace_sink.projection.trace_event_deserialize_failed_total")
                .with_description(
                    "mcp_trace_segment audit payloads that failed TraceEvent deserialize (silent drop avoided via metric).",
                )
                .build(),
            trace_event_detail_serialize_failed: m
                .u64_counter("plasm.trace_sink.projection.trace_event_detail_serialize_failed_total")
                .with_description(
                    "Decoded TraceEvent rows that failed to_detail_record_value during detail projection.",
                )
                .build(),
        }
    })
}

/// Idempotency query: how many IDs were found in SQL vs required an Iceberg lookup.
#[derive(Clone, Copy, Default)]
pub(crate) struct ProjectionDedupeHits {
    pub sql: u64,
    pub lake: u64,
}

/// `load_latest_trace_heads`: rows from SQL vs filled from the lake.
#[derive(Clone, Copy, Default)]
pub(crate) struct ProjectionHeadRowCounts {
    pub sql: u64,
    pub lake: u64,
}

#[derive(Clone, Copy)]
pub(crate) enum ListSummariesSource {
    Projection,
    IcebergFallback,
}

impl ListSummariesSource {
    fn as_kv(self) -> KeyValue {
        match self {
            ListSummariesSource::Projection => KeyValue::new("source", "projection"),
            ListSummariesSource::IcebergFallback => KeyValue::new("source", "iceberg_fallback"),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum LoadTraceDetailSource {
    Projection,
    IcebergFallback,
}

impl LoadTraceDetailSource {
    fn as_kv(self) -> KeyValue {
        match self {
            LoadTraceDetailSource::Projection => KeyValue::new("source", "projection"),
            LoadTraceDetailSource::IcebergFallback => KeyValue::new("source", "iceberg_fallback"),
        }
    }
}

/// One POST `/v1/events` completed (any outcome).
pub(crate) fn record_ingest_batch(
    events_in_body: usize,
    accepted: usize,
    duplicate_skipped: usize,
) {
    let m = sink_metrics();
    m.ingest_batches.add(1, &[]);
    m.ingest_events_in_batch.add(events_in_body as u64, &[]);
    m.ingest_events_accepted.add(accepted as u64, &[]);
    m.ingest_events_duplicate_skipped
        .add(duplicate_skipped as u64, &[]);
}

pub(crate) fn record_projection_dedupe(hits: ProjectionDedupeHits) {
    let m = sink_metrics();
    if hits.sql > 0 {
        m.projection_dedupe_sql_hits.add(hits.sql, &[]);
    }
    if hits.lake > 0 {
        m.projection_dedupe_lake_hits.add(hits.lake, &[]);
    }
}

pub(crate) fn record_projection_trace_heads(rows: ProjectionHeadRowCounts) {
    let m = sink_metrics();
    if rows.sql > 0 {
        m.projection_heads_sql_rows.add(rows.sql, &[]);
    }
    if rows.lake > 0 {
        m.projection_heads_lake_rows.add(rows.lake, &[]);
    }
}

pub(crate) fn record_list_summaries(source: ListSummariesSource) {
    sink_metrics()
        .projection_list_summaries
        .add(1, &[source.as_kv()]);
}

pub(crate) fn record_load_trace_detail(source: LoadTraceDetailSource) {
    sink_metrics()
        .projection_load_trace_detail
        .add(1, &[source.as_kv()]);
}

pub(crate) fn record_iceberg_detail_prune_fallback() {
    sink_metrics().iceberg_detail_prune_fallback.add(1, &[]);
}

pub(crate) fn record_heads_backfill(rows: u64) {
    if rows > 0 {
        sink_metrics().projection_heads_backfill_rows.add(rows, &[]);
    }
}

pub(crate) fn record_segment_projection_gc(rows: u64) {
    if rows > 0 {
        sink_metrics().segment_projection_gc_rows.add(rows, &[]);
    }
}

/// Failed typed decode of an `mcp_trace_segment` audit payload (`kind` label when peekable).
pub(crate) fn record_trace_event_deserialize_failed(segment_kind: &str) {
    sink_metrics().trace_event_deserialize_failed.add(
        1,
        &[KeyValue::new("segment_kind", segment_kind.to_string())],
    );
}

/// Failed detail-row serialize after a successful TraceEvent decode.
pub(crate) fn record_trace_event_detail_serialize_failed() {
    sink_metrics()
        .trace_event_detail_serialize_failed
        .add(1, &[]);
}
