//! OpenTelemetry metrics for typed discovery (`plasm.discovery.*`).

use std::sync::OnceLock;
use std::time::Duration;

use opentelemetry::global;
use opentelemetry::metrics::{Counter, Gauge, Histogram};
use opentelemetry::KeyValue;

fn meter() -> opentelemetry::metrics::Meter {
    global::meter("plasm-discovery")
}

fn requests_total() -> &'static Counter<u64> {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("plasm.discovery.requests_total")
            .build()
    })
}

fn clarifications_total() -> &'static Counter<u64> {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("plasm.discovery.clarifications_total")
            .build()
    })
}

fn index_builds_total() -> &'static Counter<u64> {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("plasm.discovery.index_builds_total")
            .build()
    })
}

fn index_cache_total() -> &'static Counter<u64> {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("plasm.discovery.index_cache_total")
            .build()
    })
}

fn index_build_duration() -> &'static Histogram<f64> {
    static H: OnceLock<Histogram<f64>> = OnceLock::new();
    H.get_or_init(|| {
        meter()
            .f64_histogram("plasm.discovery.index_build.duration_ms")
            .build()
    })
}

fn intent_decompose_duration() -> &'static Histogram<f64> {
    static H: OnceLock<Histogram<f64>> = OnceLock::new();
    H.get_or_init(|| {
        meter()
            .f64_histogram("plasm.discovery.intent_decompose.duration_ms")
            .build()
    })
}

fn graph_validate_duration() -> &'static Histogram<f64> {
    static H: OnceLock<Histogram<f64>> = OnceLock::new();
    H.get_or_init(|| {
        meter()
            .f64_histogram("plasm.discovery.graph_validate.duration_ms")
            .build()
    })
}

fn decision_duration() -> &'static Histogram<f64> {
    static H: OnceLock<Histogram<f64>> = OnceLock::new();
    H.get_or_init(|| {
        meter()
            .f64_histogram("plasm.discovery.decision.duration_ms")
            .build()
    })
}

fn index_entities_gauge() -> &'static Gauge<i64> {
    static G: OnceLock<Gauge<i64>> = OnceLock::new();
    G.get_or_init(|| meter().i64_gauge("plasm.discovery.index.entities").build())
}

fn index_capabilities_gauge() -> &'static Gauge<i64> {
    static G: OnceLock<Gauge<i64>> = OnceLock::new();
    G.get_or_init(|| {
        meter()
            .i64_gauge("plasm.discovery.index.capabilities")
            .build()
    })
}

fn options_count_hist() -> &'static Histogram<f64> {
    static H: OnceLock<Histogram<f64>> = OnceLock::new();
    H.get_or_init(|| {
        meter()
            .f64_histogram("plasm.discovery.options.count")
            .build()
    })
}

fn hypotheses_count_hist() -> &'static Histogram<f64> {
    static H: OnceLock<Histogram<f64>> = OnceLock::new();
    H.get_or_init(|| {
        meter()
            .f64_histogram("plasm.discovery.hypotheses.count")
            .build()
    })
}

pub fn record_request_outcome(outcome: &'static str) {
    requests_total().add(1, &[KeyValue::new("outcome", outcome.to_string())]);
}

pub fn record_clarification(dimension: &'static str) {
    clarifications_total().add(1, &[KeyValue::new("dimension", dimension.to_string())]);
}

pub fn record_index_build(outcome: &'static str, duration: Duration) {
    index_builds_total().add(1, &[KeyValue::new("outcome", outcome.to_string())]);
    index_build_duration().record(duration.as_secs_f64() * 1000.0, &[]);
}

pub fn record_index_cache(outcome: &'static str) {
    index_cache_total().add(1, &[KeyValue::new("outcome", outcome.to_string())]);
}

pub fn record_index_sizes(entities: i64, capabilities: i64) {
    index_entities_gauge().record(entities, &[]);
    index_capabilities_gauge().record(capabilities, &[]);
}

pub fn record_intent_decompose_duration(duration: Duration) {
    intent_decompose_duration().record(duration.as_secs_f64() * 1000.0, &[]);
}

pub fn record_graph_validate_duration(duration: Duration) {
    graph_validate_duration().record(duration.as_secs_f64() * 1000.0, &[]);
}

pub fn record_decision_duration(duration: Duration) {
    decision_duration().record(duration.as_secs_f64() * 1000.0, &[]);
}

pub fn record_option_count(n: u64) {
    options_count_hist().record(n as f64, &[]);
}

pub fn record_hypothesis_count(n: u64) {
    hypotheses_count_hist().record(n as f64, &[]);
}

#[cfg(test)]
mod tests {
    #[test]
    fn plasm_discovery_metric_names_documented() {
        assert_eq!(
            "plasm.discovery.requests_total",
            "plasm.discovery.requests_total"
        );
        assert_eq!(
            "plasm.discovery.index_build.duration_ms",
            "plasm.discovery.index_build.duration_ms"
        );
    }
}
