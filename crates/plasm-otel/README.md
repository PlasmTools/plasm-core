# plasm-otel

Shared bootstrap for OpenTelemetry **traces**, **metrics**, and **logs** over **OTLP**, driven by standard `OTEL_*` environment variables.

Used by `plasm-mcp` / `plasm` and `plasm-trace-sink`.

This crate is the **OTLP export lane** only. MCP/execute session timelines live in `plasm-trace` /
`plasm-trace-wire`; durable Iceberg ingest is the optional `plasm-trace-sink` binary.

## When OTLP is enabled

OTLP export initializes when **not** disabled and at least one collector endpoint is configured:

- `OTEL_EXPORTER_OTLP_ENDPOINT`, and/or
- `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`, `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`

Disabled when:

- `OTEL_SDK_DISABLED=true`, or
- `OTEL_TRACES_EXPORTER`, `OTEL_METRICS_EXPORTER`, and `OTEL_LOGS_EXPORTER` are all explicitly `none` (unset means **on** for that signal).

## Resource

Uses `opentelemetry_sdk::Resource::builder()` so `**OTEL_SERVICE_NAME`**, `**OTEL_RESOURCE_ATTRIBUTES**`, and SDK defaults apply. If `OTEL_SERVICE_NAME` is unset, the passed-in **default service name** (e.g. `plasm`) is merged as `service.name`.

## Protocol

`OTEL_EXPORTER_OTLP_PROTOCOL`:

- unset or `http/protobuf` (and `http/json` if enabled upstream): HTTP/protobuf exporters
- `grpc`: gRPC (Tonic) exporters

Per-signal timeouts, headers, compression, and endpoints follow the **opentelemetry-otlp** crate (see upstream constants `OTEL_EXPORTER_OTLP_TRACES_*`, etc.).

### Compression

If `**OTEL_EXPORTER_OTLP_COMPRESSION=gzip`** (recommended for SigNoz Cloud), the Rust OTLP HTTP client must be built with the `**gzip-http**` feature on `**opentelemetry-otlp**`. Without it, exporter construction fails at startup and `**plasm_otel::init**` falls back to console-only logging (no traces/metrics/logs to the collector). Omit the env var or use an image built from a workspace that enables `**gzip-http**`.

## Metrics temporality

The OTLP **metrics** exporter always uses **delta** temporality (fixed in code). That matches what **SigNoz Cloud** accepts for exponential histograms and avoids per-environment `OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE` tuning.

Rust batches metric exports every `**OTEL_METRIC_EXPORT_INTERVAL**` ms (default **60000**); new series can take until the first successful export after traffic.

## Traces (`tower-http` request spans)

HTTP servers use `**tower_http::trace::TraceLayer`**, whose default request span is `**DEBUG**`. When `**RUST_LOG` is unset**, `plasm_otel` uses a stderr filter default of `**info,tower_http=debug,tower_http::trace::on_request=off,tower_http::trace::on_response=off,hyper=warn,h2=warn`** whenever **OTLP traces** are enabled, so request spans still reach `**tracing-opentelemetry`** without per-request `**started processing request**` / `**finished processing request**` DEBUG lines on stderr.

If you set `**RUST_LOG**` yourself (for example to `info` only), add `**tower_http=debug**` (or `trace`) for request spans to reach OTLP, and the same `**on_request`/`on_response**` overrides if you want to avoid that log noise.

## Fallback

If OTLP wiring fails at startup, falls back to **stderr** `tracing` formatting only (same as a pure console setup).