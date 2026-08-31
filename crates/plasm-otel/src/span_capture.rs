//! In-memory OpenTelemetry span capture for force_flush parent/child assertions.
//!
//! Enabled with the `testing` feature (or crate unit tests).

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{
    InMemorySpanExporter, InMemorySpanExporterBuilder, SdkTracerProvider, SimpleSpanProcessor,
    SpanData,
};
use tracing::Subscriber;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;

/// Captured finished spans plus the provider used to flush them.
pub struct SpanCapture {
    exporter: InMemorySpanExporter,
    provider: SdkTracerProvider,
}

impl SpanCapture {
    /// Build a tracer provider with a simple in-memory exporter (sync on span end).
    ///
    /// Uses a provider-local tracer for the subscriber layer (no global tracer provider
    /// mutation — safe under parallel `cargo test`).
    pub fn install() -> Self {
        let exporter = InMemorySpanExporterBuilder::new().build();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
            .build();
        Self { exporter, provider }
    }

    /// Registry + `tracing-opentelemetry` layer bound to this capture's tracer.
    pub fn subscriber(&self) -> impl Subscriber + Send + Sync + 'static {
        let tracer = self.provider.tracer("plasm-otel-span-capture");
        let otel = tracing_opentelemetry::layer().with_tracer(tracer);
        Registry::default().with(otel)
    }

    /// Force-flush then return finished [`SpanData`] rows.
    pub fn force_flush_spans(&self) -> Vec<SpanData> {
        let _ = self.provider.force_flush();
        self.exporter
            .get_finished_spans()
            .expect("in-memory span exporter lock")
    }
}

/// Run `f` under a thread-local subscriber that exports OTel spans; return result + finished spans.
pub fn with_captured_spans<F, R>(f: F) -> (R, Vec<SpanData>)
where
    F: FnOnce() -> R,
{
    let capture = SpanCapture::install();
    let subscriber = capture.subscriber();
    let result = tracing::subscriber::with_default(subscriber, f);
    let spans = capture.force_flush_spans();
    (result, spans)
}

/// True when `child` lists `parent` as its parent span id.
pub fn is_child_of(child: &SpanData, parent: &SpanData) -> bool {
    child.parent_span_id == parent.span_context.span_id()
}

/// True when `child` is a descendant of `ancestor` by walking `parent_span_id`.
pub fn is_descendant(child: &SpanData, ancestor: &SpanData, spans: &[SpanData]) -> bool {
    let mut current = child;
    for _ in 0..16 {
        if is_child_of(current, ancestor) {
            return true;
        }
        let parent_id = current.parent_span_id;
        match spans.iter().find(|s| s.span_context.span_id() == parent_id) {
            Some(p) => current = p,
            None => return false,
        }
    }
    false
}

/// Find the first finished span whose name equals `name`.
pub fn find_span<'a>(spans: &'a [SpanData], name: &str) -> Option<&'a SpanData> {
    spans.iter().find(|s| s.name.as_ref() == name)
}
