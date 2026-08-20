//! W3C Trace Context propagation for incoming HTTP (tower-http [`tower_http::trace::TraceLayer`]).
//!
//! Call [`crate::install_w3c_trace_context_propagator`] during OTLP init when traces are enabled
//! so [`tower_http_trace_parent_span`] can extract `traceparent` / `tracestate` from request headers.

use axum::extract::MatchedPath;
use http::Request;
use opentelemetry::global;
use opentelemetry_http::HeaderExtractor;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Install the standard W3C `traceparent` propagator on the global OpenTelemetry stack.
///
/// Must run **after** [`opentelemetry::global::set_tracer_provider`] when exporting traces, and
/// **before** serving HTTP so incoming distributed traces (e.g. Phoenix `Req` + `traceparent`)
/// become parents of the tower-http request span.
pub fn install_w3c_trace_context_propagator() {
    global::set_text_map_propagator(opentelemetry_sdk::propagation::TraceContextPropagator::new());
}

/// [`tower_http::trace::MakeSpan`] for Axum: static name `plasm_agent.http.request`, semantic
/// `http.method` / `http.route` (matched template when available; path without query otherwise),
/// and OpenTelemetry parent from W3C headers when present.
///
/// Apply with [`.route_layer`](axum::Router::route_layer) so [`MatchedPath`] is populated.
pub fn tower_http_trace_parent_span<B>(request: &Request<B>) -> tracing::Span {
    let parent_cx = global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor(request.headers()))
    });
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str())
        .unwrap_or_else(|| request.uri().path());
    let span = tracing::debug_span!(
        "plasm_agent.http.request",
        http.method = %request.method(),
        http.route = %route,
    );
    let _ = span.set_parent(parent_cx);
    span
}
