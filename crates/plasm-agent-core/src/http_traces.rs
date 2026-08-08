//! `GET /v1/traces` (list), `GET /v1/traces/:trace_id` (detail), `GET /v1/traces/:trace_id/stream` (SSE patches).
//!
//! **Durable reads** (tenant-scoped list/detail) go to the Plasm trace sink service via `PLASM_TRACE_SINK_READ_URL` /
//! `PLASM_TRACE_SINK_URL`. There is **no** fallback to the in-memory [`crate::trace_hub`] when that HTTP
//! call fails — the handler returns **503** (`application/problem+json`) so clients do not misread outages
//! as an empty history.
//!
//! **SSE** (`/stream`) remains hub-backed for real-time patches on active sessions only.

use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::stream::{self, Stream};
use futures_util::StreamExt;
use http_problem::prelude::{StatusCode as ProblemStatus, Uri};
use http_problem::Problem;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::convert::Infallible;
use uuid::Uuid;

use crate::http_problem_util::{problem_response, problem_types};
use crate::incoming_auth::IncomingPrincipal;
use crate::server_state::PlasmHostState;
use crate::trace_hub::{TraceDetailDto, TraceListStatus, TraceSummaryDto};
use plasm_trace::merge_trace_totals;
use plasm_trace_wire::{
    TraceDetailResponse as SinkTraceDetailResponse, TraceListResponse as SinkTraceListResponse,
};
use tracing::Instrument;

#[derive(Debug, Deserialize)]
pub struct TraceListQuery {
    #[serde(default)]
    pub project_slug: Option<String>,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub status: Option<String>,
}

fn default_limit() -> usize {
    50
}

fn trace_list_limit(limit: usize) -> usize {
    limit.clamp(1, 200)
}

fn merge_live_trace_summary(d: &mut TraceSummaryDto, live: &TraceSummaryDto) {
    d.totals = merge_trace_totals(&d.totals, &live.totals);
    if live.status != "live" {
        return;
    }
    d.status = live.status;
    d.started_at_ms = live.started_at_ms;
    d.ended_at_ms = live.ended_at_ms;
    if let Some(id) = live.logical_session_id.clone() {
        d.logical_session_id = Some(id);
    }
    if !live.mcp_session_id.is_empty() {
        d.mcp_session_id = live.mcp_session_id.clone();
    }
}

fn merge_trace_summaries(
    durable: Vec<TraceSummaryDto>,
    live: Vec<TraceSummaryDto>,
    offset: usize,
    limit: usize,
) -> Vec<TraceSummaryDto> {
    let mut by_trace_id: HashMap<String, TraceSummaryDto> = HashMap::new();
    for trace in durable {
        by_trace_id.insert(trace.trace_id.clone(), trace);
    }
    for live_trace in live {
        by_trace_id
            .entry(live_trace.trace_id.clone())
            .and_modify(|d| merge_live_trace_summary(d, &live_trace))
            .or_insert(live_trace);
    }
    let mut traces = by_trace_id.into_values().collect::<Vec<_>>();
    traces.sort_by(|a, b| {
        b.started_at_ms
            .cmp(&a.started_at_ms)
            .then_with(|| b.trace_id.cmp(&a.trace_id))
    });
    traces
        .into_iter()
        .skip(offset)
        .take(trace_list_limit(limit))
        .collect()
}

#[derive(Serialize)]
struct TraceListResponse {
    traces: Vec<TraceSummaryDto>,
}

fn viewer_tenant(principal: &IncomingPrincipal) -> Option<&str> {
    principal.0.as_ref().map(|p| p.tenant_id.as_str())
}

fn problem_trace_sink_not_configured() -> Problem {
    Problem::custom(
        ProblemStatus::SERVICE_UNAVAILABLE,
        Uri::from_static(problem_types::TRACE_SINK_NOT_CONFIGURED),
    )
    .with_title("Trace sink not configured")
    .with_detail(
        "Set PLASM_TRACE_SINK_READ_URL, PLASM_TRACE_SINK_URL (remote), or PLASM_TRACE_ARCHIVE_DIR (local OSS) for durable trace reads; hub-only history is not exposed as tenant history without one of these.",
    )
}

fn problem_trace_sink_unavailable(detail: impl Into<String>) -> Problem {
    Problem::custom(
        ProblemStatus::SERVICE_UNAVAILABLE,
        Uri::from_static(problem_types::TRACE_SINK_UNAVAILABLE),
    )
    .with_title("Trace sink unavailable")
    .with_detail(detail.into())
}

async fn list_traces(
    Extension(st): Extension<PlasmHostState>,
    Extension(principal): Extension<IncomingPrincipal>,
    Query(q): Query<TraceListQuery>,
) -> Result<Json<TraceListResponse>, Response> {
    let status = TraceListStatus::parse(q.status.as_deref());
    let limit = trace_list_limit(q.limit);
    let merged_limit = q.offset.saturating_add(limit);
    if let Some(tenant) = viewer_tenant(&principal) {
        if let Some(base) = st.trace_sink_read_base_url.as_deref() {
            let list_span = crate::spans::billing_trace_list(tenant, q.limit, q.offset);
            match list_traces_from_sink(
                &st.trace_sink_http,
                base,
                tenant,
                q.project_slug.as_deref(),
                q.status.as_deref(),
                0,
                merged_limit,
            )
            .instrument(list_span)
            .await
            {
                Ok(durable) => {
                    let live = st
                        .trace_hub
                        .list_for_tenant(
                            Some(tenant),
                            q.project_slug.as_deref(),
                            0,
                            merged_limit,
                            status,
                        )
                        .await;
                    return Ok(Json(TraceListResponse {
                        traces: merge_trace_summaries(durable, live, q.offset, limit),
                    }));
                }
                Err(detail) => {
                    tracing::warn!(
                        target: "plasm_agent::http_traces",
                        tenant,
                        error = %detail,
                        "GET /v1/traces: trace sink read failed",
                    );
                    return Err(problem_response(problem_trace_sink_unavailable(detail)));
                }
            }
        } else if let Some(arch) = st.local_trace_archive.as_ref() {
            match arch
                .list_for_tenant(tenant, q.project_slug.as_deref(), 0, merged_limit, status)
                .await
            {
                Ok(durable) => {
                    let live = st
                        .trace_hub
                        .list_for_tenant(
                            Some(tenant),
                            q.project_slug.as_deref(),
                            0,
                            merged_limit,
                            status,
                        )
                        .await;
                    return Ok(Json(TraceListResponse {
                        traces: merge_trace_summaries(durable, live, q.offset, limit),
                    }));
                }
                Err(e) => {
                    tracing::warn!(
                        target: "plasm_agent::http_traces",
                        tenant,
                        error = %e,
                        "GET /v1/traces: local trace archive list failed",
                    );
                    return Err(problem_response(problem_trace_sink_unavailable(
                        e.to_string(),
                    )));
                }
            }
        } else {
            tracing::warn!(
                target: "plasm_agent::http_traces",
                tenant,
                "GET /v1/traces: no durable trace backend (set PLASM_TRACE_SINK_READ_URL, PLASM_TRACE_SINK_URL, or PLASM_TRACE_ARCHIVE_DIR)",
            );
            return Err(problem_response(problem_trace_sink_not_configured()));
        }
    }

    let traces = st
        .trace_hub
        .list_for_tenant(
            viewer_tenant(&principal),
            q.project_slug.as_deref(),
            q.offset,
            limit,
            status,
        )
        .await;
    Ok(Json(TraceListResponse { traces }))
}

async fn get_trace_detail(
    Extension(st): Extension<PlasmHostState>,
    Extension(principal): Extension<IncomingPrincipal>,
    Path(trace_id): Path<Uuid>,
) -> Result<Json<TraceDetailDto>, Response> {
    let viewer = viewer_tenant(&principal);
    if let Some(detail) = st.trace_hub.get_detail(trace_id, viewer).await {
        return Ok(Json(detail));
    }

    let Some(tenant) = viewer else {
        return match st.trace_hub.get_detail(trace_id, None).await {
            Some(d) => Ok(Json(d)),
            None => Err(StatusCode::NOT_FOUND.into_response()),
        };
    };

    if let Some(base) = st.trace_sink_read_base_url.as_deref() {
        let detail_span = crate::spans::billing_trace_detail(tenant, &trace_id);
        match fetch_trace_detail_from_sink(&st.trace_sink_http, base, tenant, trace_id)
            .instrument(detail_span)
            .await
        {
            Ok(Some(detail)) => return Ok(Json(detail)),
            Ok(None) => {}
            Err(detail) => {
                tracing::warn!(
                    target: "plasm_agent::http_traces",
                    tenant,
                    %trace_id,
                    error = %detail,
                    "GET /v1/traces/:id: trace sink read failed",
                );
                return Err(problem_response(problem_trace_sink_unavailable(detail)));
            }
        }
    }
    if let Some(arch) = st.local_trace_archive.as_ref() {
        match arch.get_detail(tenant, trace_id).await {
            Ok(Some(d)) => return Ok(Json(d)),
            Ok(None) => return Err(StatusCode::NOT_FOUND.into_response()),
            Err(e) => {
                tracing::warn!(
                    target: "plasm_agent::http_traces",
                    tenant,
                    %trace_id,
                    error = %e,
                    "GET /v1/traces/:id: local trace archive read failed",
                );
                return Err(problem_response(problem_trace_sink_unavailable(
                    e.to_string(),
                )));
            }
        }
    }
    tracing::warn!(
        target: "plasm_agent::http_traces",
        tenant,
        %trace_id,
        "GET /v1/traces/:id: no durable trace read backend configured",
    );
    Err(problem_response(problem_trace_sink_not_configured()))
}

fn sse_event_name_for_trace_payload(json: &str) -> &'static str {
    let Ok(v) = serde_json::from_str::<JsonValue>(json) else {
        return "patch";
    };
    match v.get("kind").and_then(|k| k.as_str()) {
        Some("terminal") => "terminal",
        Some("snapshot") => "snapshot",
        Some("durable_ingest") => "durable_ingest",
        _ => "patch",
    }
}

/// Hub-backed SSE for live session patches (not a substitute for durable `GET` detail).
async fn stream_trace_events(
    Extension(st): Extension<PlasmHostState>,
    Extension(principal): Extension<IncomingPrincipal>,
    Path(trace_id): Path<Uuid>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>> + Send>, StatusCode> {
    let viewer = viewer_tenant(&principal);
    let snap = st
        .trace_hub
        .sse_snapshot_payload(trace_id, viewer)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    let hub = st.trace_hub.clone();
    let first = stream::once(async move {
        Ok::<Event, Infallible>(Event::default().event("snapshot").data(snap))
    });

    let rx = hub.subscribe_trace_async(trace_id).await;
    let body: std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> =
        if let Some(rx) = rx {
            let rest = stream::unfold(rx, |mut rx| async move {
                match rx.recv().await {
                    Ok(payload) => {
                        let name = sse_event_name_for_trace_payload(&payload);
                        Some((Ok(Event::default().event(name).data(payload)), rx))
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        Some((Ok(Event::default().comment("lagged")), rx))
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
                }
            });
            Box::pin(first.chain(rest))
        } else {
            Box::pin(first)
        };

    Ok(Sse::new(body).keep_alive(KeepAlive::default()))
}

pub fn trace_routes() -> Router {
    Router::new()
        .route("/v1/traces", get(list_traces))
        .route("/v1/traces/{trace_id}", get(get_trace_detail))
        .route("/v1/traces/{trace_id}/stream", get(stream_trace_events))
}

async fn list_traces_from_sink(
    client: &reqwest::Client,
    base: &str,
    tenant_id: &str,
    project_slug: Option<&str>,
    status: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<Vec<TraceSummaryDto>, String> {
    let mut qp = vec![
        ("tenant_id", tenant_id.to_string()),
        ("offset", offset.to_string()),
        ("limit", limit.to_string()),
    ];
    if let Some(ps) = project_slug.filter(|s| !s.is_empty()) {
        qp.push(("project_slug", ps.to_string()));
    }
    if let Some(status) = status.filter(|s| !s.is_empty()) {
        qp.push(("status", status.to_string()));
    }
    let qs = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(qp.iter().map(|(k, v)| (*k, v.as_str())))
        .finish();
    let url = format!("{}/v1/traces?{qs}", base.trim_end_matches('/'));
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("HTTP transport to trace sink: {e}"))?;
    let status_code = resp.status();
    if !status_code.is_success() {
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(512)
            .collect::<String>();
        return Err(format!(
            "trace sink GET /v1/traces returned {status_code}: {body}"
        ));
    }
    let body: SinkTraceListResponse = resp
        .json()
        .await
        .map_err(|e| format!("trace sink list response JSON: {e}"))?;
    let traces = body
        .traces
        .into_iter()
        .map(|t| TraceSummaryDto {
            trace_id: t.trace_id.to_string(),
            mcp_session_id: t.mcp_session_id,
            logical_session_id: None,
            status: hub_status_from_sink(&t.status),
            started_at_ms: t.started_at_ms,
            ended_at_ms: t.ended_at_ms,
            project_slug: t.project_slug,
            tenant_id: t.tenant_id,
            mcp_config: None,
            totals: hub_totals_from_sink(&t.totals),
        })
        .collect::<Vec<_>>();
    Ok(traces)
}

fn logical_session_id_from_sink_records(records: &[JsonValue]) -> Option<String> {
    records.iter().find_map(|r| {
        r.get("_plasm_audit")
            .and_then(|v| v.get("logical_session_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

async fn fetch_trace_detail_from_sink(
    client: &reqwest::Client,
    base: &str,
    tenant_id: &str,
    trace_id: Uuid,
) -> Result<Option<crate::trace_hub::TraceDetailDto>, String> {
    let url = format!(
        "{}/v1/traces/{}?tenant_id={}",
        base.trim_end_matches('/'),
        trace_id,
        url::form_urlencoded::byte_serialize(tenant_id.as_bytes()).collect::<String>()
    );
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("HTTP transport to trace sink: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let status = resp.status();
    if !status.is_success() {
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(512)
            .collect::<String>();
        return Err(format!(
            "trace sink GET /v1/traces/{{id}} returned {status}: {body}"
        ));
    }
    let body: SinkTraceDetailResponse = resp
        .json()
        .await
        .map_err(|e| format!("trace sink detail response JSON: {e}"))?;
    let summary = body.detail.summary;
    let records = body
        .detail
        .records
        .into_iter()
        .map(|r| r.record)
        .collect::<Vec<_>>();
    let logical_session_id = logical_session_id_from_sink_records(&records);
    let detail = crate::trace_hub::TraceDetailDto {
        summary: TraceSummaryDto {
            trace_id: summary.trace_id.to_string(),
            mcp_session_id: summary.mcp_session_id,
            logical_session_id,
            status: hub_status_from_sink(&summary.status),
            started_at_ms: summary.started_at_ms,
            ended_at_ms: summary.ended_at_ms,
            project_slug: summary.project_slug,
            tenant_id: summary.tenant_id,
            mcp_config: None,
            totals: hub_totals_from_sink(&summary.totals),
        },
        records,
    };
    Ok(Some(detail))
}

fn hub_status_from_sink(status: &str) -> &'static str {
    if status == "live" {
        "live"
    } else {
        "completed"
    }
}

fn hub_totals_from_sink(t: &plasm_trace_wire::TraceTotals) -> crate::trace_hub::TraceTotals {
    // Same wire type as `plasm_trace::TraceTotals` (re-exported).
    t.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace_hub::TraceTotals;

    fn summary(trace_id: &str, status: &'static str, started_at_ms: u64) -> TraceSummaryDto {
        TraceSummaryDto {
            trace_id: trace_id.to_string(),
            mcp_session_id: format!("mcp-{trace_id}"),
            logical_session_id: None,
            status,
            started_at_ms,
            ended_at_ms: if status == "completed" {
                Some(started_at_ms + 10)
            } else {
                None
            },
            project_slug: "main".to_string(),
            tenant_id: "tenant-a".to_string(),
            mcp_config: None,
            totals: TraceTotals::default(),
        }
    }

    #[test]
    fn merge_trace_summaries_merges_totals_fieldwise_for_duplicate_trace_id() {
        let duplicate_id = Uuid::new_v4().to_string();
        let mut durable = summary(&duplicate_id, "completed", 100);
        durable.totals.network_requests = 5;
        durable.totals.total_duration_ms = 50;
        let mut live = summary(&duplicate_id, "live", 300);
        live.totals.network_requests = 0;
        live.totals.total_duration_ms = 0;
        live.totals.mcp_resource_read_chars = 1722;

        let merged = merge_trace_summaries(vec![durable], vec![live], 0, 10);
        let row = merged.iter().find(|t| t.trace_id == duplicate_id).unwrap();
        assert_eq!(row.status, "live");
        assert_eq!(row.totals.network_requests, 5);
        assert_eq!(row.totals.total_duration_ms, 50);
        assert_eq!(row.totals.mcp_resource_read_chars, 1722);
    }

    #[test]
    fn merge_trace_summaries_prefers_live_duplicate_and_sorts_newest_first() {
        let duplicate_id = Uuid::new_v4().to_string();
        let old_id = Uuid::new_v4().to_string();
        let newest_id = Uuid::new_v4().to_string();

        let durable = vec![
            summary(&duplicate_id, "completed", 100),
            summary(&old_id, "completed", 50),
        ];
        let live = vec![
            summary(&duplicate_id, "live", 300),
            summary(&newest_id, "live", 400),
        ];

        let merged = merge_trace_summaries(durable, live, 0, 10);

        assert_eq!(
            merged
                .iter()
                .map(|t| t.trace_id.as_str())
                .collect::<Vec<_>>(),
            vec![newest_id.as_str(), duplicate_id.as_str(), old_id.as_str()]
        );
        let duplicate = merged.iter().find(|t| t.trace_id == duplicate_id).unwrap();
        assert_eq!(duplicate.status, "live");
        assert_eq!(duplicate.started_at_ms, 300);
    }

    #[test]
    fn merge_trace_summaries_applies_offset_and_limit_after_merge() {
        let a = Uuid::new_v4().to_string();
        let b = Uuid::new_v4().to_string();
        let c = Uuid::new_v4().to_string();

        let merged = merge_trace_summaries(
            vec![summary(&a, "completed", 100), summary(&c, "completed", 300)],
            vec![summary(&b, "live", 200)],
            1,
            1,
        );

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].trace_id, b);
    }
}
