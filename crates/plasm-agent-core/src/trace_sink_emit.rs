//! Optional POST to `PLASM_TRACE_SINK_URL/v1/events` for audit + billing (non-blocking).
//!
//! When `PLASM_TRACE_SINK_URL` is unset, [`EnvTraceIngestClient`] skips the network call. A **one-time**
//! `tracing::warn!` is emitted per process (see [`warn_missing_trace_sink_url_once`]). If
//! **`PLASM_TRACE_SINK_STRICT=1`**, a **one-time** `tracing::error!` is emitted instead — operators should
//! treat that as misconfiguration in environments where durable ingest is required.

use std::sync::{Once, OnceLock};
use std::time::{Duration, Instant};

use plasm_observability_contracts::{
    AuditEvent, IngestBatchRequest, AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT, SCHEMA_VERSION,
};
use plasm_trace::{TraceEvent, TraceSegment};
use tokio::sync::Semaphore;
use tracing::Instrument;
use uuid::Uuid;

/// Correlates audit rows with an in-process trace root.
///
/// - **MCP (logical session):** `trace_id` matches [`crate::trace_hub::trace_id_for_mcp_logical_session`];
///   `logical_session_id` is the canonical UUID string; `mcp_session_id` is transport correlation only.
/// - **MCP (legacy / tests):** [`crate::trace_hub::trace_id_for_mcp_transport_session`] when using
///   [`crate::trace_hub::TraceHub::ensure_session`].
/// - **Direct HTTP execute:** `trace_id` is [`crate::trace_hub::trace_id_for_http_execute_session`]
///   (`tenant_scope`, `prompt_hash`, `execute_session_id`); MCP fields are absent.
#[derive(Clone, Debug)]
pub struct PlasmTraceContext {
    pub trace_id: Uuid,
    pub call_index: Option<i64>,
    /// MCP `MCP-Session-Id` (transport correlation).
    pub mcp_session_id: Option<String>,
    /// Agent logical session from `plasm_context` (canonical prompt/trace scope).
    pub logical_session_id: Option<String>,
    /// Stateless wire ref (`l_<token>`) for short `plasm://session/l_<token>/r/n` URIs; HTTP execute leaves unset.
    pub logical_session_ref: Option<String>,
}

/// Envelope fields shared by MCP hub and HTTP execute durable emits (`mcp_trace_segment` rows).
#[derive(Clone, Debug)]
pub struct McpTraceAuditFields {
    pub trace_id: Uuid,
    pub mcp_session_id: Option<String>,
    pub logical_session_id: Option<String>,
    pub plasm_prompt_hash: Option<String>,
    pub plasm_execute_session: Option<String>,
    /// Deterministic wire `run_id` (`pr` + 64 hex), when present.
    pub run_id: Option<String>,
    pub tenant_id: Option<String>,
    pub principal_sub: Option<String>,
}

/// Fire-and-forget ingest using the shared trace-sink envelope (compile-time schema alignment).
pub trait TraceIngestClient: Send + Sync {
    fn spawn_ingest_batch(&self, batch: IngestBatchRequest);
}

fn trace_sink_strict_from_env() -> bool {
    matches!(
        std::env::var("PLASM_TRACE_SINK_STRICT")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

static MISSING_TRACE_SINK_URL_WARN: Once = Once::new();
static MISSING_TRACE_SINK_URL_STRICT_ERR: Once = Once::new();

fn log_missing_plasm_trace_sink_url_once() {
    if trace_sink_strict_from_env() {
        MISSING_TRACE_SINK_URL_STRICT_ERR.call_once(|| {
            tracing::error!(
                target: "plasm_agent::trace_sink",
                "PLASM_TRACE_SINK_STRICT is set but PLASM_TRACE_SINK_URL is unset — durable MCP trace ingest is disabled (SSE still streams in-memory only). Unset STRICT or configure PLASM_TRACE_SINK_URL."
            );
        });
    } else {
        MISSING_TRACE_SINK_URL_WARN.call_once(|| {
            tracing::warn!(
                target: "plasm_agent::trace_sink",
                "PLASM_TRACE_SINK_URL is unset; MCP trace segments are not persisted to plasm-trace-sink (SSE still works). Set PLASM_TRACE_SINK_URL for durable audit/billing."
            );
        });
    }
}

/// Reads `PLASM_TRACE_SINK_URL` per emit (same semantics as the previous free function).
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvTraceIngestClient;

impl TraceIngestClient for EnvTraceIngestClient {
    fn spawn_ingest_batch(&self, batch: IngestBatchRequest) {
        let Some(base) = std::env::var("PLASM_TRACE_SINK_URL")
            .ok()
            .filter(|s| !s.is_empty())
        else {
            if !batch.events.is_empty() {
                log_missing_plasm_trace_sink_url_once();
            }
            return;
        };
        let event_count = batch.events.len();
        crate::metrics::record_trace_sink_batch_spawned(event_count);
        let kind_hint = batch
            .events
            .first()
            .map(|e| e.event_kind.as_str())
            .unwrap_or("audit_batch");
        let emit_span = crate::spans::billing_audit_batch_emit(event_count, kind_hint);
        let body = match serde_json::to_value(&batch) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    target: "plasm_agent::trace_sink",
                    error = %e,
                    "trace sink batch serialize failed"
                );
                crate::metrics::record_trace_sink_batch_serialize_failed();
                return;
            }
        };
        tokio::spawn(async move { post_events_json(base, body).await }.instrument(emit_span));
    }
}

/// No network; for tests or hosts that disable outbound telemetry.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopTraceIngestClient;

impl TraceIngestClient for NoopTraceIngestClient {
    fn spawn_ingest_batch(&self, _batch: IngestBatchRequest) {}
}

fn trace_sink_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            // Trace-sink serializes Iceberg writes on one DataFusion mutex; bursts of MCP emits
            // can queue longer than a typical HTTP timeout — allow headroom so rows reach Iceberg.
            .timeout(Duration::from_secs(120))
            .pool_max_idle_per_host(16)
            .build()
            .expect("reqwest client for trace sink ingest")
    })
}

/// Caps concurrent POSTs so we do not stampede the sink's single-writer Iceberg path (reduces client timeouts).
fn trace_sink_inflight() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(16))
}

async fn post_events_json(base: String, body: serde_json::Value) {
    let _inflight = trace_sink_inflight()
        .acquire()
        .await
        .expect("trace sink inflight semaphore");
    let post_span = crate::spans::billing_trace_sink_http_post();
    let url = format!("{}/v1/events", base.trim_end_matches('/'));
    let started = Instant::now();
    match trace_sink_http_client()
        .post(url.clone())
        .json(&body)
        .send()
        .instrument(post_span)
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            crate::metrics::record_trace_sink_http_post("success", "2xx", started.elapsed());
        }
        Ok(resp) => {
            let status_class = match resp.status().as_u16() {
                400..=499 => "4xx",
                500..=599 => "5xx",
                _ => "other",
            };
            crate::metrics::record_trace_sink_http_post("error", status_class, started.elapsed());
            tracing::warn!(
                target: "plasm_agent::trace_sink",
                status = %resp.status(),
                url = %url,
                "trace sink ingest non-success"
            );
        }
        Err(e) => {
            crate::metrics::record_trace_sink_http_post("error", "transport", started.elapsed());
            tracing::warn!(
                target: "plasm_agent::trace_sink",
                error = %e,
                error_dbg = ?e,
                url = %url,
                "trace sink ingest failed"
            );
        }
    }
}

fn indices_for_segment(seg: &TraceSegment) -> (Option<i64>, Option<i64>) {
    match seg {
        TraceSegment::PlasmLine {
            call_index,
            line_index,
            ..
        } => (Some(*call_index as i64), Some(*line_index as i64)),
        TraceSegment::PlasmInvocation { call_index, .. } => (Some(*call_index as i64), None),
        TraceSegment::PlasmError {
            call_index,
            line_index,
            ..
        } => (Some(*call_index as i64), line_index.map(|i| i as i64)),
        _ => (None, None),
    }
}

/// Fire-and-forget ingest of one canonical [`TraceEvent`] (`event_kind` = `mcp_trace_segment`).
pub fn spawn_emit_mcp_trace_segment(
    ingest: &dyn TraceIngestClient,
    fields: &McpTraceAuditFields,
    trace_event: &TraceEvent,
    precomputed_payload: Option<serde_json::Value>,
) {
    let event_id = Uuid::new_v4();
    let emitted_at = chrono::Utc::now();
    let (call_index, line_index) = indices_for_segment(&trace_event.segment);
    let request_units = i64::from(matches!(
        &trace_event.segment,
        TraceSegment::PlasmLine { .. }
    ));
    let mut payload = precomputed_payload.unwrap_or_else(|| {
        serde_json::to_value(trace_event).unwrap_or_else(|_| serde_json::json!({}))
    });
    if fields.logical_session_id.is_some() || fields.mcp_session_id.is_some() {
        if let serde_json::Value::Object(ref mut map) = payload {
            let mut audit = serde_json::Map::new();
            if let Some(ls) = &fields.logical_session_id {
                audit.insert(
                    "logical_session_id".into(),
                    serde_json::Value::String(ls.clone()),
                );
            }
            if let Some(ms) = &fields.mcp_session_id {
                audit.insert(
                    "mcp_transport_session_id".into(),
                    serde_json::Value::String(ms.clone()),
                );
            }
            if !audit.is_empty() {
                map.insert("_plasm_audit".into(), serde_json::Value::Object(audit));
            }
        }
    }

    let event = AuditEvent {
        event_id,
        schema_version: SCHEMA_VERSION,
        emitted_at,
        ingested_at: chrono::Utc::now(),
        trace_id: fields.trace_id,
        mcp_session_id: fields.mcp_session_id.clone(),
        plasm_prompt_hash: fields.plasm_prompt_hash.clone(),
        plasm_execute_session: fields.plasm_execute_session.clone(),
        run_id: fields.run_id.clone(),
        call_index,
        line_index,
        tenant_id: fields.tenant_id.clone(),
        principal_sub: fields.principal_sub.clone(),
        workspace_slug: None,
        project_slug: None,
        event_kind: AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT.to_string(),
        request_units,
        payload,
    };

    ingest.spawn_ingest_batch(IngestBatchRequest {
        events: vec![event],
    });
}
