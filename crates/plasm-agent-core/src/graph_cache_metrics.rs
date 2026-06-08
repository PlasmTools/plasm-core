//! OpenTelemetry metrics for bounded session graph cache (durable spill deltas, hot trim, rehydrate).

use std::sync::OnceLock;
use std::time::Duration;

use opentelemetry::global;
use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::KeyValue;

struct GraphCacheInstruments {
    delta_pages_appended: Counter<u64>,
    delta_append_duration_ms: Histogram<f64>,
    delta_append_errors: Counter<u64>,
    surface_graph_backed: Counter<u64>,
    rehydrate_calls: Counter<u64>,
    rehydrate_rows: Counter<u64>,
    rehydrate_pages_read: Counter<u64>,
    rehydrate_duration_ms: Histogram<f64>,
    snapshot_calls: Counter<u64>,
    snapshot_entities: Counter<u64>,
    snapshot_duration_ms: Histogram<f64>,
    snapshot_errors: Counter<u64>,
}

static GRAPH_CACHE_INSTRUMENTS: OnceLock<GraphCacheInstruments> = OnceLock::new();

fn instruments() -> &'static GraphCacheInstruments {
    GRAPH_CACHE_INSTRUMENTS.get_or_init(|| {
        let meter = global::meter("plasm");
        GraphCacheInstruments {
            delta_pages_appended: meter
                .u64_counter("plasm.execute.graph.delta_pages_appended_total")
                .with_description(
                    "Durable graph page deltas appended for a session (v2 spill body).",
                )
                .build(),
            delta_append_duration_ms: meter
                .f64_histogram("plasm.execute.graph.delta_append.duration_ms")
                .with_description("Wall time to frame and write one graph page delta.")
                .build(),
            delta_append_errors: meter
                .u64_counter("plasm.execute.graph.delta_append.errors_total")
                .with_description("Failed graph page delta writes to session object store.")
                .build(),
            surface_graph_backed: meter
                .u64_counter("plasm.execute.graph.surface_graph_backed_total")
                .with_description(
                    "Surface query materializations that defer row expansion to graph rehydrate.",
                )
                .build(),
            rehydrate_calls: meter
                .u64_counter("plasm.execute.graph.rehydrate.calls_total")
                .with_description("Graph row rehydrate invocations (full merge vs streaming scan).")
                .build(),
            rehydrate_rows: meter
                .u64_counter("plasm.execute.graph.rehydrate.rows_total")
                .with_description("Rows returned from hot cache + spilled pages during rehydrate.")
                .build(),
            rehydrate_pages_read: meter
                .u64_counter("plasm.execute.graph.rehydrate.pages_read_total")
                .with_description("Spilled graph page deltas scanned during rehydrate.")
                .build(),
            rehydrate_duration_ms: meter
                .f64_histogram("plasm.execute.graph.rehydrate.duration_ms")
                .with_description("Wall time to rehydrate rows from hot cache and durable pages.")
                .build(),
            snapshot_calls: meter
                .u64_counter("plasm.execute.graph.snapshot.calls_total")
                .with_description("Session graph snapshot writes on session finalize.")
                .build(),
            snapshot_entities: meter
                .u64_counter("plasm.execute.graph.snapshot.entities_total")
                .with_description("Entity rows merged into a session graph snapshot payload.")
                .build(),
            snapshot_duration_ms: meter
                .f64_histogram("plasm.execute.graph.snapshot.duration_ms")
                .with_description("Wall time to merge hot + spilled pages and upload snapshot.")
                .build(),
            snapshot_errors: meter
                .u64_counter("plasm.execute.graph.snapshot.errors_total")
                .with_description("Failed session graph snapshot writes on finalize.")
                .build(),
        }
    })
}

/// Record a successful durable graph page delta append.
pub(crate) fn record_graph_delta_page_append(entity_count: usize, duration: Duration) {
    let bucket = entity_count_bucket(entity_count);
    let attrs = &[KeyValue::new("entity_count_bucket", bucket)];
    let m = instruments();
    m.delta_pages_appended.add(1, attrs);
    m.delta_append_duration_ms
        .record(duration.as_secs_f64() * 1000.0, attrs);
}

pub(crate) fn record_graph_delta_page_append_error() {
    instruments().delta_append_errors.add(1, &[]);
}

/// Surface query chose graph-backed row source (hot RAM incomplete vs logical count).
pub(crate) fn record_graph_surface_graph_backed(logical_count: usize) {
    let attrs = &[KeyValue::new(
        "logical_count_bucket",
        logical_count_bucket(logical_count),
    )];
    instruments().surface_graph_backed.add(1, attrs);
}

/// `mode`: `full` (merge hot + pages) | `stream` (row callback scan).
pub(crate) fn record_graph_rehydrate(
    mode: &'static str,
    rows: usize,
    pages_read: usize,
    duration: Duration,
) {
    let attrs = &[
        KeyValue::new("mode", mode),
        KeyValue::new("row_count_bucket", logical_count_bucket(rows)),
    ];
    let m = instruments();
    m.rehydrate_calls.add(1, attrs);
    if rows > 0 {
        m.rehydrate_rows.add(rows as u64, attrs);
    }
    if pages_read > 0 {
        m.rehydrate_pages_read.add(pages_read as u64, attrs);
    }
    m.rehydrate_duration_ms
        .record(duration.as_secs_f64() * 1000.0, attrs);
}

pub(crate) fn record_graph_snapshot(result: &'static str, entities: usize, duration: Duration) {
    let attrs = &[
        KeyValue::new("result", result),
        KeyValue::new("entity_count_bucket", entity_count_bucket(entities)),
    ];
    let m = instruments();
    if result == "success" {
        m.snapshot_calls.add(1, attrs);
        if entities > 0 {
            m.snapshot_entities.add(entities as u64, attrs);
        }
    } else {
        m.snapshot_errors.add(1, attrs);
    }
    m.snapshot_duration_ms
        .record(duration.as_secs_f64() * 1000.0, attrs);
}

fn entity_count_bucket(n: usize) -> &'static str {
    logical_count_bucket(n)
}

fn logical_count_bucket(n: usize) -> &'static str {
    match n {
        0 => "0",
        1..=10 => "1_10",
        11..=100 => "11_100",
        101..=1000 => "101_1000",
        _ => "gt_1000",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_count_bucket_labels() {
        assert_eq!(logical_count_bucket(0), "0");
        assert_eq!(logical_count_bucket(5), "1_10");
        assert_eq!(logical_count_bucket(50), "11_100");
        assert_eq!(logical_count_bucket(500), "101_1000");
        assert_eq!(logical_count_bucket(5000), "gt_1000");
    }

    #[test]
    fn graph_cache_metrics_smoke() {
        record_graph_delta_page_append(20, Duration::from_millis(2));
        record_graph_delta_page_append_error();
        record_graph_surface_graph_backed(128);
        record_graph_rehydrate("full", 128, 3, Duration::from_millis(8));
        record_graph_rehydrate("stream", 1, 2, Duration::from_millis(1));
        record_graph_snapshot("success", 256, Duration::from_millis(40));
        record_graph_snapshot("error", 0, Duration::from_millis(5));
    }
}
