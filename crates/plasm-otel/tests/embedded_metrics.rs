use opentelemetry::global;
use std::time::Duration;

// Run separately with an OTLP collector and OTEL_METRIC_EXPORT_INTERVAL=100.
#[test]
#[ignore = "requires a configured OTLP metrics collector"]
fn embedded_meter_emits_a_probe_without_a_tracing_subscriber() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = runtime.enter();
    plasm_otel::init_embedded_metrics("embedded-test").unwrap();
    global::meter("embedded-test")
        .f64_histogram("plasm.embedded.test.duration_ms")
        .build()
        .record(1.0, &[]);
    std::thread::sleep(Duration::from_secs(2));
}
