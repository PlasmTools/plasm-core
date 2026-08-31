//! Shared OpenTelemetry OTLP bootstrap for Plasm binaries (`OTEL_*` env vars).

mod trace_context;
mod tui_capture;

#[cfg(any(feature = "testing", test))]
pub mod span_capture;

pub use trace_context::{install_w3c_trace_context_propagator, tower_http_trace_parent_span};
pub use tui_capture::{layer as tui_capture_layer, TuiCaptureLayer, TuiLogCallback, TuiLogRecord};

use std::borrow::Cow;

use anyhow::Context;
use opentelemetry::global;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter};
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::metrics::Temporality;
use opentelemetry_sdk::resource::Resource;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;

/// `OTEL_EXPORTER_OTLP_PROTOCOL` selects gRPC vs HTTP when set to `grpc` (case-insensitive).
fn use_grpc_transport() -> bool {
    std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL")
        .map(|v| v.trim().eq_ignore_ascii_case("grpc"))
        .unwrap_or(false)
}

fn build_span_exporter() -> Result<SpanExporter, opentelemetry_otlp::ExporterBuildError> {
    if use_grpc_transport() {
        SpanExporter::builder().with_tonic().build()
    } else {
        SpanExporter::builder().with_http().build()
    }
}

/// OTLP metrics temporality: **delta** only. Hosted SigNoz rejects or drops many **cumulative**
/// exponential-histogram series; delta matches what works in production without per-deploy env tuning.
const OTLP_METRICS_TEMPORALITY: Temporality = Temporality::Delta;

fn build_metric_exporter(
    temporality: Temporality,
) -> Result<MetricExporter, opentelemetry_otlp::ExporterBuildError> {
    if use_grpc_transport() {
        MetricExporter::builder()
            .with_tonic()
            .with_temporality(temporality)
            .build()
    } else {
        MetricExporter::builder()
            .with_http()
            .with_temporality(temporality)
            .build()
    }
}

fn build_log_exporter() -> Result<LogExporter, opentelemetry_otlp::ExporterBuildError> {
    if use_grpc_transport() {
        LogExporter::builder().with_tonic().build()
    } else {
        LogExporter::builder().with_http().build()
    }
}

fn otlp_any_endpoint_configured() -> bool {
    if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .is_some()
    {
        return true;
    }
    for key in [
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
    ] {
        if std::env::var(key)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .is_some()
        {
            return true;
        }
    }
    false
}

fn exporter_explicitly_disabled(var: &str) -> bool {
    std::env::var(var).ok().as_deref() == Some("none")
}

/// When all three standard exporter env vars are explicitly `none`, skip OTLP entirely.
fn all_otlp_signals_explicitly_disabled() -> bool {
    exporter_explicitly_disabled("OTEL_TRACES_EXPORTER")
        && exporter_explicitly_disabled("OTEL_METRICS_EXPORTER")
        && exporter_explicitly_disabled("OTEL_LOGS_EXPORTER")
}

fn otel_disabled() -> bool {
    if std::env::var("OTEL_SDK_DISABLED").ok().as_deref() == Some("true") {
        return true;
    }
    if !otlp_any_endpoint_configured() {
        return true;
    }
    all_otlp_signals_explicitly_disabled()
}

fn traces_enabled() -> bool {
    !exporter_explicitly_disabled("OTEL_TRACES_EXPORTER")
}

fn metrics_enabled() -> bool {
    !exporter_explicitly_disabled("OTEL_METRICS_EXPORTER")
}

fn logs_enabled() -> bool {
    !exporter_explicitly_disabled("OTEL_LOGS_EXPORTER")
}

fn build_resource(default_service_name: &str) -> Resource {
    let name = std::env::var("OTEL_SERVICE_NAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default_service_name.to_string());
    Resource::builder().with_service_name(name).build()
}

fn init_logs(resource: &Resource) -> anyhow::Result<SdkLoggerProvider> {
    let exporter = build_log_exporter().context("LogExporter::build")?;
    Ok(SdkLoggerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource.clone())
        .build())
}

fn init_traces(resource: &Resource) -> anyhow::Result<SdkTracerProvider> {
    let exporter = build_span_exporter().context("SpanExporter::build")?;
    Ok(SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource.clone())
        .build())
}

fn init_metrics(resource: &Resource) -> anyhow::Result<SdkMeterProvider> {
    let exporter =
        build_metric_exporter(OTLP_METRICS_TEMPORALITY).context("MetricExporter::build")?;
    Ok(SdkMeterProvider::builder()
        .with_periodic_exporter(exporter)
        .with_resource(resource.clone())
        .build())
}

fn init_console_only_with_writer<W>(
    make_writer: W,
    tui_capture: Option<TuiLogCallback>,
) -> anyhow::Result<()>
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    let fmt_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(make_writer)
        .with_filter(fmt_filter.clone());
    let tui = tui_capture_layer(tui_capture).with_filter(fmt_filter);
    tracing_subscriber::registry()
        .with(fmt)
        .with(tui)
        .try_init()
        .map_err(|e| anyhow::anyhow!("tracing subscriber init: {e}"))?;
    Ok(())
}

/// Default for the **stderr** `fmt` layer when `RUST_LOG` is unset.
///
/// `tower_http::trace::TraceLayer` (used by Plasm HTTP servers) builds its request span at
/// **DEBUG** by default (`DefaultMakeSpan`, target `tower_http::trace::make_span`). With a global
/// max level of **INFO**, those callsites stay disabled, `tracing-opentelemetry` never records
/// them, and backends show no HTTP traces.
///
/// Enabling **`tower_http=debug`** also turns on **`DefaultOnRequest`** / **`DefaultOnResponse`**
/// DEBUG events (`started processing request` / `finished processing request`), which flood
/// stderr. We keep **`tower_http=debug`** for span export but silence only those two modules.
fn stderr_fmt_env_filter(traces_otlp_enabled: bool) -> (EnvFilter, bool) {
    match EnvFilter::try_from_default_env() {
        Ok(filter) => (filter, false),
        Err(_) => {
            let directives = if traces_otlp_enabled {
                "info,tower_http=debug,tower_http::trace::on_request=off,tower_http::trace::on_response=off,hyper=warn,h2=warn"
            } else {
                "info"
            };
            let filter = EnvFilter::try_new(directives).unwrap_or_else(|_| EnvFilter::new("info"));
            (filter, true)
        }
    }
}

fn try_init_otlp_with_writer<W>(
    default_service_name: &str,
    make_writer: W,
    tui_capture: Option<TuiLogCallback>,
) -> anyhow::Result<()>
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    let resource = build_resource(default_service_name);
    let traces = traces_enabled();
    let metrics = metrics_enabled();
    let logs = logs_enabled();

    let tracer_provider = if traces {
        Some(init_traces(&resource).context("init traces")?)
    } else {
        None
    };
    let meter_provider = if metrics {
        Some(init_metrics(&resource).context("init metrics")?)
    } else {
        None
    };
    let logger_provider = if logs {
        Some(init_logs(&resource).context("init logs")?)
    } else {
        None
    };

    if let Some(ref tp) = tracer_provider {
        global::set_tracer_provider(tp.clone());
    }
    if traces {
        install_w3c_trace_context_propagator();
    }
    if let Some(ref mp) = meter_provider {
        global::set_meter_provider(mp.clone());
    }

    let (fmt_filter, fmt_filter_is_default) = stderr_fmt_env_filter(traces);
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(make_writer)
        .with_filter(fmt_filter.clone());
    let tui = tui_capture_layer(tui_capture).with_filter(fmt_filter);

    let tracer_name: Cow<'static, str> = std::env::var("OTEL_SERVICE_NAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(Into::into)
        .unwrap_or_else(|| default_service_name.to_string().into());

    // `fmt` attaches to `Registry` first; OTel layers wrap it so `Filtered<fmt::Layer<…>>` types line up.
    match (traces, logs) {
        (true, true) => {
            let tracer = global::tracer(tracer_name.clone());
            let otel_traces = tracing_opentelemetry::layer().with_tracer(tracer);
            let lp = logger_provider.as_ref().expect("logs enabled");
            let otel_logs = OpenTelemetryTracingBridge::new(lp);
            let filter_otel = EnvFilter::new("info")
                .add_directive("hyper=off".parse().unwrap())
                .add_directive("reqwest=off".parse().unwrap());
            tracing_subscriber::registry()
                .with(fmt)
                .with(tui)
                .with(otel_traces)
                .with(otel_logs.with_filter(filter_otel))
                .init();
        }
        (true, false) => {
            let tracer = global::tracer(tracer_name.clone());
            let otel_traces = tracing_opentelemetry::layer().with_tracer(tracer);
            tracing_subscriber::registry()
                .with(fmt)
                .with(tui)
                .with(otel_traces)
                .init();
        }
        (false, true) => {
            let lp = logger_provider.as_ref().expect("logs enabled");
            let otel_logs = OpenTelemetryTracingBridge::new(lp);
            let filter_otel = EnvFilter::new("info")
                .add_directive("hyper=off".parse().unwrap())
                .add_directive("reqwest=off".parse().unwrap());
            tracing_subscriber::registry()
                .with(fmt)
                .with(tui)
                .with(otel_logs.with_filter(filter_otel))
                .init();
        }
        (false, false) => {
            tracing_subscriber::registry().with(fmt).with(tui).init();
        }
    }

    if traces && fmt_filter_is_default {
        tracing::info!(
            target: "plasm_otel",
            "stderr tracing filter: RUST_LOG unset; tower_http=debug for TraceLayer request spans (tower_http::trace::on_request/on_response events suppressed)"
        );
    }

    if metrics {
        let interval_ms: u64 = std::env::var("OTEL_METRIC_EXPORT_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60_000);
        tracing::info!(
            target: "plasm_otel",
            temporality = ?OTLP_METRICS_TEMPORALITY,
            metric_export_interval_ms = interval_ms,
            "OTLP metrics exporter configured (delta temporality; SigNoz-compatible)"
        );
    }

    if let Some(tp) = tracer_provider {
        std::mem::forget(tp);
    }
    if let Some(mp) = meter_provider {
        std::mem::forget(mp);
    }
    if let Some(lp) = logger_provider {
        std::mem::forget(lp);
    }

    Ok(())
}

/// Install `tracing` + OTLP when collector endpoints are configured (see crate README).
///
/// On failure, falls back to stderr `tracing` only so servers can still start.
pub fn init(default_service_name: &str) -> anyhow::Result<()> {
    init_with_fmt_make_writer(default_service_name, std::io::stderr, None)
}

/// Like [`init`], but directs the human-readable `tracing-subscriber` **fmt** layer to `make_writer`
/// (OTLP bridges unchanged). `make_writer` must be [`Clone`] so OTLP init can retry on fallback.
///
/// When `tui_capture` is set, a companion layer emits [`TuiLogRecord`] for the same filtered events.
pub fn init_with_fmt_make_writer<W>(
    default_service_name: &str,
    make_writer: W,
    tui_capture: Option<TuiLogCallback>,
) -> anyhow::Result<()>
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + Clone + 'static,
{
    if otel_disabled() {
        return init_console_only_with_writer(make_writer, tui_capture);
    }

    match try_init_otlp_with_writer(
        default_service_name,
        make_writer.clone(),
        tui_capture.clone(),
    ) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("OpenTelemetry init failed ({e}); falling back to console logging only");
            init_console_only_with_writer(make_writer, tui_capture)
        }
    }
}

#[cfg(test)]
mod span_graph_tests;
