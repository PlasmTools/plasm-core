//! OpenTelemetry metrics for `plasm` (MCP tools, HTTP execute, trace sink, plugin catalog reload).
//!
//! Follows [`docs/otel-metrics-instrumentation-guide.md`](../../docs/otel-metrics-instrumentation-guide.md):
//! static metric names, structured attributes, `OnceLock` instrument cache, meter `plasm`.

use std::sync::OnceLock;
use std::time::Duration;

use opentelemetry::global;
use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::KeyValue;

const METER_NAME: &str = "plasm";

struct AgentMetrics {
    mcp_tool_calls: Counter<u64>,
    mcp_tool_duration_ms: Histogram<f64>,
    mcp_resource_calls: Counter<u64>,
    mcp_resource_duration_ms: Histogram<f64>,
    execute_session_outcomes: Counter<u64>,
    execute_rehydrate_outcomes: Counter<u64>,
    execute_expression_calls: Counter<u64>,
    execute_expression_duration_ms: Histogram<f64>,
    execute_expression_cache_hits: Counter<u64>,
    execute_expression_cache_misses: Counter<u64>,
    execute_artifact_calls: Counter<u64>,
    execute_artifact_duration_ms: Histogram<f64>,
    execute_artifact_resolve_layer: Counter<u64>,
    trace_sink_batches_spawned: Counter<u64>,
    trace_sink_events_spawned: Counter<u64>,
    trace_sink_http_calls: Counter<u64>,
    trace_sink_http_duration_ms: Histogram<f64>,
    trace_sink_batch_serialize_errors: Counter<u64>,
    plugin_catalog_reload_calls: Counter<u64>,
    plugin_catalog_reload_duration_ms: Histogram<f64>,
    tenant_outbound_hosted_kv_lookups: Counter<u64>,
    mcp_transport_auth: Counter<u64>,
    audit_control_plane: Counter<u64>,
    run_artifact_hot_cache_evictions: Counter<u64>,
    run_artifact_archive_puts: Counter<u64>,
    trace_timeline_events_dropped: Counter<u64>,
    mcp_plasm_run_phase_duration_ms: Histogram<f64>,
    plan_execute_write_conflict_retry: Counter<u64>,
}

static AGENT_METRICS: OnceLock<AgentMetrics> = OnceLock::new();

fn agent_metrics() -> &'static AgentMetrics {
    AGENT_METRICS.get_or_init(|| {
        let m = global::meter(METER_NAME);
        AgentMetrics {
            mcp_tool_calls: m
                .u64_counter("plasm.mcp.tool.calls_total")
                .with_description("MCP call_tool invocations by tool and result.")
                .build(),
            mcp_tool_duration_ms: m
                .f64_histogram("plasm.mcp.tool.duration_ms")
                .with_description("Wall time for MCP call_tool handler per tool.")
                .build(),
            mcp_resource_calls: m
                .u64_counter("plasm.mcp.resource.read.calls_total")
                .with_description("MCP resources/read outcomes.")
                .build(),
            mcp_resource_duration_ms: m
                .f64_histogram("plasm.mcp.resource.read.duration_ms")
                .with_description("Wall time for MCP resources/read.")
                .build(),
            execute_session_outcomes: m
                .u64_counter("plasm.execute.session.outcomes_total")
                .with_description("HTTP/MCP session create outcomes (reuse, create, error).")
                .build(),
            execute_rehydrate_outcomes: m
                .u64_counter("plasm.execute.rehydrate.outcomes_total")
                .with_description("Cross-pod execute session rehydrate outcomes.")
                .build(),
            execute_expression_calls: m
                .u64_counter("plasm.execute.expression.calls_total")
                .with_description("Plasm expression line executions (HTTP/MCP).")
                .build(),
            execute_expression_duration_ms: m
                .f64_histogram("plasm.execute.expression.duration_ms")
                .with_description(
                    "Wall time for one parsed expression execution (incl. projection).",
                )
                .build(),
            execute_expression_cache_hits: m
                .u64_counter("plasm.execute.expression.cache_hits_total")
                .with_description("Cache hits summed per executed expression line.")
                .build(),
            execute_expression_cache_misses: m
                .u64_counter("plasm.execute.expression.cache_misses_total")
                .with_description("Cache misses summed per executed expression line.")
                .build(),
            execute_artifact_calls: m
                .u64_counter("plasm.execute.artifact.serve_total")
                .with_description("GET execute run artifact responses.")
                .build(),
            execute_artifact_duration_ms: m
                .f64_histogram("plasm.execute.artifact.serve_duration_ms")
                .with_description("Wall time to serve a run artifact (session lookup + payload).")
                .build(),
            execute_artifact_resolve_layer: m
                .u64_counter("plasm.execute.artifact.resolve_layer_total")
                .with_description(
                    "Successful artifact payload resolution: hot session cache vs durable archive.",
                )
                .build(),
            trace_sink_batches_spawned: m
                .u64_counter("plasm.trace_sink.ingest_batches_spawned_total")
                .with_description("Trace sink ingest batches spawned (async POST scheduled).")
                .build(),
            trace_sink_events_spawned: m
                .u64_counter("plasm.trace_sink.ingest_events_spawned_total")
                .with_description("Audit events included in spawned trace sink batches.")
                .build(),
            trace_sink_http_calls: m
                .u64_counter("plasm.trace_sink.http_post.calls_total")
                .with_description("Trace sink HTTP POST /v1/events outcomes.")
                .build(),
            trace_sink_http_duration_ms: m
                .f64_histogram("plasm.trace_sink.http_post.duration_ms")
                .with_description("Trace sink HTTP POST latency.")
                .build(),
            trace_sink_batch_serialize_errors: m
                .u64_counter("plasm.trace_sink.batch_serialize_errors_total")
                .with_description("Failed to JSON-serialize a trace sink ingest batch before POST.")
                .build(),
            plugin_catalog_reload_calls: m
                .u64_counter("plasm.plugin_catalog.reload.calls_total")
                .with_description(
                    "POST /internal/plugin-registry/v1/reload outcomes (plugin-dir catalog hot reload).",
                )
                .build(),
            plugin_catalog_reload_duration_ms: m
                .f64_histogram("plasm.plugin_catalog.reload.duration_ms")
                .with_description(
                    "Wall time for plugin catalog reload (load dir + validate + publish snapshot).",
                )
                .build(),
            tenant_outbound_hosted_kv_lookups: m
                .u64_counter("plasm.tenant_outbound.hosted_kv.lookup_total")
                .with_description(
                    "Hosted KV resolution per graph binding (outcome: hit | miss | error).",
                )
                .build(),
            mcp_transport_auth: m
                .u64_counter("plasm.mcp.transport.auth_total")
                .with_description(
                    "MCP Streamable HTTP Bearer verification (result + method: api_key | oauth | anonymous).",
                )
                .build(),
            audit_control_plane: m
                .u64_counter("plasm.audit.control_plane.requests_total")
                .with_description(
                    "Internal control-plane handlers (action + outcome) for audit dashboards.",
                )
                .build(),
            run_artifact_hot_cache_evictions: m
                .u64_counter("plasm.execute.run_artifact.hot_cache_evictions_total")
                .with_description(
                    "Run snapshots evicted from the per-session hot cache (FIFO); durable copy remains in the archive.",
                )
                .build(),
            run_artifact_archive_puts: m
                .u64_counter("plasm.execute.run_artifact.archive_puts_total")
                .with_description(
                    "Successful durable writes of execute run snapshot payloads (object store or memory backend).",
                )
                .build(),
            trace_timeline_events_dropped: m
                .u64_counter("plasm.trace_hub.timeline_events_dropped_total")
                .with_description(
                    "Trace timeline events dropped from the in-memory window (cap); durable ingest may still hold full history.",
                )
                .build(),
            mcp_plasm_run_phase_duration_ms: m
                .f64_histogram("plasm.mcp.plasm_run.phase_duration_ms")
                .with_description("Wall time between MCP plasm_run phase checkpoints.")
                .build(),
            plan_execute_write_conflict_retry: m
                .u64_counter("plasm.plan_execute.write_conflict_retry_total")
                .with_description(
                    "Per-store materialization write-conflict branch-cycle retries during concurrent execute (re-fork before any commit is visible).",
                )
                .build(),
        }
    })
}

/// Record one write-conflict branch-cycle retry (single expression fork/commit re-run).
pub fn record_graph_branch_write_conflict_retry() {
    agent_metrics()
        .plan_execute_write_conflict_retry
        .add(1, &[]);
}

pub fn record_mcp_plasm_run_phase(phase: &'static str, duration: Duration) {
    let ms = duration.as_secs_f64() * 1000.0;
    let attrs = &[KeyValue::new("phase", phase)];
    agent_metrics()
        .mcp_plasm_run_phase_duration_ms
        .record(ms, attrs);
}

/// `multi_line`: `None` except for `plasm` (`Some(true)` / `Some(false)`).
pub fn record_mcp_tool(
    tool: &'static str,
    multi_line: Option<bool>,
    result: &'static str,
    error_class: &'static str,
    duration: Duration,
) {
    let ms = duration.as_secs_f64() * 1000.0;
    let mut attrs: Vec<KeyValue> = vec![
        KeyValue::new("tool", tool),
        KeyValue::new("result", result),
        KeyValue::new("error_class", error_class),
    ];
    if let Some(m) = multi_line {
        attrs.push(KeyValue::new(
            "multi_line",
            if m {
                "true".to_string()
            } else {
                "false".to_string()
            },
        ));
    }
    let a: &[KeyValue] = attrs.as_slice();
    let m = agent_metrics();
    m.mcp_tool_calls.add(1, a);
    m.mcp_tool_duration_ms.record(ms, a);
}

/// `uri_kind`: `logical_short` (`plasm://session/{logical_session_ref}/r/{n}` or legacy UUID segment), `canonical`, `unsupported`, etc.
pub fn record_mcp_resource_read(
    uri_kind: &'static str,
    result: &'static str,
    error_class: &'static str,
    duration: Duration,
) {
    let ms = duration.as_secs_f64() * 1000.0;
    let attrs = &[
        KeyValue::new("uri_kind", uri_kind),
        KeyValue::new("result", result),
        KeyValue::new("error_class", error_class),
    ];
    let m = agent_metrics();
    m.mcp_resource_calls.add(1, attrs);
    m.mcp_resource_duration_ms.record(ms, attrs);
}

/// `outcome`: `reuse` | `create` | `error`. On success `error_kind` is `""` (empty).
pub fn record_execute_session_outcome(outcome: &'static str, error_kind: &'static str) {
    let attrs = &[
        KeyValue::new("outcome", outcome),
        KeyValue::new("error_kind", error_kind),
    ];
    agent_metrics().execute_session_outcomes.add(1, attrs);
}

pub fn record_execute_rehydrate(outcome: &'static str, error_kind: &'static str) {
    let attrs = &[
        KeyValue::new("outcome", outcome),
        KeyValue::new("error_kind", error_kind),
    ];
    agent_metrics().execute_rehydrate_outcomes.add(1, attrs);
}

pub fn record_execute_expression_line(
    entry_id: &str,
    operation: &str,
    result: &'static str,
    error_class: &'static str,
    wall_ms: f64,
    cache_hits: u64,
    cache_misses: u64,
) {
    let attrs = &[
        KeyValue::new("entry_id", entry_id.to_string()),
        KeyValue::new("operation", operation.to_string()),
        KeyValue::new("result", result),
        KeyValue::new("error_class", error_class),
    ];
    let m = agent_metrics();
    m.execute_expression_calls.add(1, attrs);
    m.execute_expression_duration_ms.record(wall_ms, attrs);
    if cache_hits > 0 {
        m.execute_expression_cache_hits.add(
            cache_hits,
            &[
                KeyValue::new("entry_id", entry_id.to_string()),
                KeyValue::new("operation", operation.to_string()),
            ],
        );
    }
    if cache_misses > 0 {
        m.execute_expression_cache_misses.add(
            cache_misses,
            &[
                KeyValue::new("entry_id", entry_id.to_string()),
                KeyValue::new("operation", operation.to_string()),
            ],
        );
    }
}

pub fn record_execute_artifact_serve(
    result: &'static str,
    error_class: &'static str,
    duration: Duration,
) {
    let ms = duration.as_secs_f64() * 1000.0;
    let attrs = &[
        KeyValue::new("result", result),
        KeyValue::new("error_class", error_class),
    ];
    let m = agent_metrics();
    m.execute_artifact_calls.add(1, attrs);
    m.execute_artifact_duration_ms.record(ms, attrs);
}

/// `layer`: `hot` (session working set) | `archive` (RunArtifactStore) — successful GET artifact resolution only.
pub fn record_execute_artifact_resolve_layer(layer: &'static str) {
    let attrs = &[KeyValue::new("layer", layer)];
    agent_metrics().execute_artifact_resolve_layer.add(1, attrs);
}

pub fn record_run_artifact_hot_cache_evictions(n: u64) {
    if n > 0 {
        agent_metrics().run_artifact_hot_cache_evictions.add(n, &[]);
    }
}

pub fn record_run_artifact_archive_put_ok() {
    agent_metrics().run_artifact_archive_puts.add(1, &[]);
}

pub fn record_trace_timeline_events_dropped(n: u64) {
    if n > 0 {
        agent_metrics().trace_timeline_events_dropped.add(n, &[]);
    }
}

pub fn record_trace_sink_batch_spawned(event_count: usize) {
    let bucket = event_count_bucket(event_count);
    let attrs = &[KeyValue::new("event_count_bucket", bucket)];
    let m = agent_metrics();
    m.trace_sink_batches_spawned.add(1, attrs);
    m.trace_sink_events_spawned.add(event_count as u64, attrs);
}

pub fn record_trace_sink_batch_serialize_failed() {
    agent_metrics()
        .trace_sink_batch_serialize_errors
        .add(1, &[]);
}

/// `outcome`: `unauthorized` | `conflict` | `load_error` | `validate_error` | `success`.
/// `error_kind`: `""` | `catalog_load` | `template_validate` (only for `load_error` / `validate_error`).
/// On `success`, pass `entry_count` and whether the catalog diff was non-empty (`catalog_changed`).
pub fn record_plugin_catalog_reload(
    outcome: &'static str,
    error_kind: &'static str,
    duration: Duration,
    entry_count: Option<usize>,
    catalog_changed: Option<bool>,
) {
    let ms = duration.as_secs_f64() * 1000.0;
    let mut attrs: Vec<KeyValue> = vec![
        KeyValue::new("outcome", outcome),
        KeyValue::new("error_kind", error_kind),
    ];
    if let Some(n) = entry_count {
        attrs.push(KeyValue::new("entry_count_bucket", entry_count_bucket(n)));
    }
    if let Some(changed) = catalog_changed {
        attrs.push(KeyValue::new(
            "catalog_changed",
            if changed { "true" } else { "false" },
        ));
    }
    let a: &[KeyValue] = attrs.as_slice();
    let m = agent_metrics();
    m.plugin_catalog_reload_calls.add(1, a);
    m.plugin_catalog_reload_duration_ms.record(ms, a);
}

/// `outcome`: `hit` | `miss` | `error` (per entry_id lookup in tenant outbound resolution).
pub fn record_tenant_outbound_hosted_kv_lookup(outcome: &'static str) {
    let attrs = &[KeyValue::new("outcome", outcome)];
    agent_metrics()
        .tenant_outbound_hosted_kv_lookups
        .add(1, attrs);
}

/// `result`: `success` | `invalid_token`
/// `method`: `api_key` | `oauth` | `anonymous` (which verifier succeeded or last attempted for failures).
pub fn record_mcp_transport_auth(result: &'static str, method: &'static str) {
    let attrs = &[
        KeyValue::new("result", result),
        KeyValue::new("method", method),
    ];
    agent_metrics().mcp_transport_auth.add(1, attrs);
}

/// `action`: e.g. `tenant.resolve`, `workspace.org_create`, `mcp.config.upsert`, `mcp.api_key.provision`
/// `outcome`: `success` | `denied` | `conflict` | `validation_error` | `dependency_error`
pub fn record_audit_control_plane(action: &'static str, outcome: &'static str) {
    let attrs = &[
        KeyValue::new("action", action),
        KeyValue::new("outcome", outcome),
    ];
    agent_metrics().audit_control_plane.add(1, attrs);
}

pub fn record_trace_sink_http_post(
    result: &'static str,
    status_class: &'static str,
    duration: Duration,
) {
    let ms = duration.as_secs_f64() * 1000.0;
    let attrs = &[
        KeyValue::new("result", result),
        KeyValue::new("status_class", status_class),
    ];
    let m = agent_metrics();
    m.trace_sink_http_calls.add(1, attrs);
    m.trace_sink_http_duration_ms.record(ms, attrs);
}

fn event_count_bucket(n: usize) -> &'static str {
    entry_count_bucket(n)
}

fn entry_count_bucket(n: usize) -> &'static str {
    match n {
        0 => "0",
        1 => "1",
        2..=10 => "2_10",
        _ => "gt_10",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_count_bucket_labels() {
        assert_eq!(event_count_bucket(0), "0");
        assert_eq!(event_count_bucket(1), "1");
        assert_eq!(event_count_bucket(5), "2_10");
        assert_eq!(event_count_bucket(11), "gt_10");
    }

    #[test]
    fn plugin_catalog_reload_metrics_smoke() {
        record_plugin_catalog_reload(
            "success",
            "",
            Duration::from_millis(12),
            Some(3),
            Some(true),
        );
        record_plugin_catalog_reload("unauthorized", "", Duration::from_millis(1), None, None);
        record_plugin_catalog_reload(
            "load_error",
            "catalog_load",
            Duration::from_millis(4),
            None,
            None,
        );
    }

    #[test]
    fn tenant_outbound_and_mcp_auth_metrics_smoke() {
        record_tenant_outbound_hosted_kv_lookup("hit");
        record_tenant_outbound_hosted_kv_lookup("miss");
        record_mcp_transport_auth("success", "api_key");
        record_mcp_transport_auth("invalid_token", "oauth");
        record_audit_control_plane("tenant.resolve", "success");
        record_audit_control_plane("mcp.config.upsert", "validation_error");
    }
}
