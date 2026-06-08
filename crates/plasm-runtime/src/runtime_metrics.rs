//! OTLP metrics for outbound HTTP from compiled operations (low-cardinality `host_class`).

use std::sync::OnceLock;
use std::time::Duration;

use opentelemetry::global;
use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::KeyValue;

struct RuntimeHttpMetrics {
    request_total: Counter<u64>,
    duration_ms: Histogram<f64>,
    retry_total: Counter<u64>,
    rate_limited_total: Counter<u64>,
    throttle_wait_ms: Histogram<f64>,
    graph_page_spill_pages: Counter<u64>,
    graph_page_spill_entities: Counter<u64>,
    graph_hot_cache_evictions: Counter<u64>,
    graph_page_spill_duration_ms: Histogram<f64>,
}

static RUNTIME_HTTP: OnceLock<RuntimeHttpMetrics> = OnceLock::new();

fn runtime_http() -> &'static RuntimeHttpMetrics {
    RUNTIME_HTTP.get_or_init(|| {
        let m = global::meter("plasm-runtime");
        RuntimeHttpMetrics {
            request_total: m
                .u64_counter("plasm.runtime.http.client.request_total")
                .with_description("Outbound HTTP requests from compiled HTTP/GraphQL operations.")
                .build(),
            duration_ms: m
                .f64_histogram("plasm.runtime.http.client.request_duration_ms")
                .with_description(
                    "Wall time for outbound HTTP round-trip (reqwest send + response read).",
                )
                .build(),
            retry_total: m
                .u64_counter("plasm.runtime.http.client.retry_total")
                .with_description("Outbound HTTP retries after rate limit or transient failure.")
                .build(),
            rate_limited_total: m
                .u64_counter("plasm.runtime.http.client.rate_limited_total")
                .with_description("Outbound HTTP failures after exhausting retries on HTTP 429.")
                .build(),
            throttle_wait_ms: m
                .f64_histogram("plasm.runtime.http.client.throttle_wait_ms")
                .with_description("Sleep time between outbound HTTP retry attempts.")
                .build(),
            graph_page_spill_pages: m
                .u64_counter("plasm.runtime.graph.page_spill.pages_total")
                .with_description(
                    "Paginated query pages durably spilled after graph-backed materialization.",
                )
                .build(),
            graph_page_spill_entities: m
                .u64_counter("plasm.runtime.graph.page_spill.entities_total")
                .with_description("Entity rows written to durable graph page deltas per spill.")
                .build(),
            graph_hot_cache_evictions: m
                .u64_counter("plasm.runtime.graph.hot_cache.evictions_total")
                .with_description(
                    "Graph cache entities evicted from RAM after spill trim (FIFO insertion order).",
                )
                .build(),
            graph_page_spill_duration_ms: m
                .f64_histogram("plasm.runtime.graph.page_spill.duration_ms")
                .with_description("Wall time for one spill append + hot-cache trim cycle.")
                .build(),
        }
    })
}

pub(crate) fn record_http_retry(status: u16, delay: Duration) {
    let m = runtime_http();
    m.retry_total.add(
        1,
        &[KeyValue::new(
            "status",
            if status == 0 {
                "transport".to_string()
            } else {
                status.to_string()
            },
        )],
    );
    m.throttle_wait_ms.record(delay.as_secs_f64() * 1000.0, &[]);
}

pub(crate) fn record_http_rate_limited() {
    runtime_http().rate_limited_total.add(1, &[]);
}

/// Coarse host bucketing to avoid high-cardinality labels on full URLs.
fn host_class(url: &str) -> &'static str {
    let u = url.to_ascii_lowercase();
    if u.contains("localhost")
        || u.contains("127.0.0.1")
        || u.contains("0.0.0.0")
        || u.contains("[::1]")
    {
        return "loopback";
    }
    "public"
}

pub(crate) fn record_outbound_http_request(
    http_method: &str,
    url: &str,
    success: bool,
    duration: Duration,
) {
    let ms = duration.as_secs_f64() * 1000.0;
    let attrs = &[
        KeyValue::new("http_method", http_method.to_string()),
        KeyValue::new("host_class", host_class(url)),
        KeyValue::new("result", if success { "success" } else { "error" }),
    ];
    let m = runtime_http();
    m.request_total.add(1, attrs);
    m.duration_ms.record(ms, attrs);
}

/// `result`: `success` | `error`. `page_index_bucket`: coarse page ordinal for paginated reads.
pub(crate) fn record_graph_page_spill(
    result: &'static str,
    page_index: usize,
    entities_spilled: usize,
    evicted: usize,
    duration: Duration,
) {
    let page_bucket = page_index_bucket(page_index);
    let attrs = &[
        KeyValue::new("result", result),
        KeyValue::new("page_index_bucket", page_bucket),
    ];
    let m = runtime_http();
    if result == "success" {
        m.graph_page_spill_pages.add(1, attrs);
        if entities_spilled > 0 {
            m.graph_page_spill_entities.add(entities_spilled as u64, attrs);
        }
        if evicted > 0 {
            m.graph_hot_cache_evictions.add(evicted as u64, &[]);
        }
    }
    m.graph_page_spill_duration_ms
        .record(duration.as_secs_f64() * 1000.0, attrs);
}

fn page_index_bucket(page_index: usize) -> &'static str {
    match page_index {
        0 => "0",
        1..=10 => "1_10",
        _ => "gt_10",
    }
}

#[cfg(test)]
mod graph_metrics_tests {
    use super::*;

    #[test]
    fn page_index_bucket_labels() {
        assert_eq!(page_index_bucket(0), "0");
        assert_eq!(page_index_bucket(5), "1_10");
        assert_eq!(page_index_bucket(11), "gt_10");
    }

    #[test]
    fn graph_page_spill_metrics_smoke() {
        record_graph_page_spill("success", 2, 20, 5, Duration::from_millis(3));
        record_graph_page_spill("error", 0, 0, 0, Duration::from_millis(1));
    }
}

#[cfg(test)]
mod host_class_tests {
    use super::host_class;

    #[test]
    fn loopback_detection() {
        assert_eq!(host_class("http://localhost:3000/foo"), "loopback");
        assert_eq!(host_class("https://127.0.0.1/api"), "loopback");
    }

    #[test]
    fn public_default() {
        assert_eq!(host_class("https://api.example.com/v1"), "public");
    }
}
