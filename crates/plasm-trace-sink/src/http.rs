//! Axum HTTP surface for ingest and read APIs.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use crate::append_port::{TenantId, TimeWindow, TraceListFilter, TraceListStatusFilter};
use crate::metrics;
use crate::model::{
    BillingUsageResponse, IngestBatchRequest, IngestBatchResponse, TraceDetailResponse,
    TraceListResponse,
};
use crate::spans;
use crate::state::AppState;
use crate::storage_error::TraceSinkStorageError;
use chrono::{DateTime, Utc};
use plasm_otel::tower_http_trace_parent_span;
use tracing::Instrument;

#[derive(Debug, Deserialize)]
pub struct BillingQuery {
    pub tenant_id: Option<String>,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Deserialize)]
pub struct TraceListQuery {
    pub tenant_id: String,
    pub project_slug: Option<String>,
    pub status: Option<String>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct TraceDetailQuery {
    pub tenant_id: String,
}

pub fn router(state: Arc<AppState>) -> Router {
    // Health is excluded from TraceLayer so liveness probes do not emit per-request DEBUG spans.
    let health_only = Router::new().route("/v1/health", get(health));

    let traced = Router::new()
        .route("/v1/events", post(post_events))
        .route("/v1/traces", get(list_traces))
        .route("/v1/traces/{trace_id}", get(get_trace))
        .route("/v1/billing/usage", get(get_billing_usage))
        .layer(TraceLayer::new_for_http().make_span_with(tower_http_trace_parent_span));

    Router::new()
        .merge(health_only)
        .merge(traced)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

async fn post_events(
    State(state): State<Arc<AppState>>,
    Json(body): Json<IngestBatchRequest>,
) -> Result<Json<IngestBatchResponse>, StatusCode> {
    let incoming = body.events.len();
    let ingest_span = spans::ingest_events_batch(incoming);
    let (accepted, duplicate_skipped) = state
        .ingest_batch(body.events)
        .instrument(ingest_span)
        .await;
    metrics::record_ingest_batch(incoming, accepted, duplicate_skipped);
    Ok(Json(IngestBatchResponse {
        accepted,
        duplicate_skipped,
    }))
}

async fn get_trace(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TraceDetailQuery>,
    Path(trace_id): Path<Uuid>,
) -> Result<Json<TraceDetailResponse>, StatusCode> {
    let tenant = TenantId::parse(&q.tenant_id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let detail_span = spans::read_trace_detail(tenant.as_str(), &trace_id);
    if let Some(detail) = state
        .trace_detail(&tenant, trace_id)
        .instrument(detail_span)
        .await
        .map_err(iceberg_500("Iceberg load_trace_detail failed"))?
    {
        return Ok(Json(TraceDetailResponse { trace_id, detail }));
    }

    // Fallback: tenant-scoped pruned segment scan (never trace_id-only across partitions).
    let fallback_span = spans::read_trace_detail(tenant.as_str(), &trace_id);
    let mut events = state
        .trace_events_for_tenant(&tenant, trace_id)
        .instrument(fallback_span)
        .await
        .map_err(iceberg_500("Iceberg load_trace_events_for_tenant failed"))?;
    if events.is_empty() {
        return Err(StatusCode::NOT_FOUND);
    }
    crate::iceberg_writer::sort_audit_events(&mut events);
    let detail = crate::iceberg_writer::durable_detail_from_events(
        trace_id,
        events,
        tenant.as_str().to_string(),
    );
    Ok(Json(TraceDetailResponse { trace_id, detail }))
}

async fn list_traces(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TraceListQuery>,
) -> Result<Json<TraceListResponse>, StatusCode> {
    let tenant = TenantId::parse(&q.tenant_id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let status =
        TraceListStatusFilter::parse(q.status.as_deref()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let list_span = spans::read_trace_list(tenant.as_str(), limit, offset);
    let traces = state
        .list_traces(TraceListFilter {
            tenant: &tenant,
            project_slug: q.project_slug.as_deref(),
            status,
            offset,
            limit,
        })
        .instrument(list_span)
        .await
        .map_err(iceberg_500("Iceberg list_traces failed"))?;
    Ok(Json(TraceListResponse { traces }))
}

async fn get_billing_usage(
    State(state): State<Arc<AppState>>,
    Query(q): Query<BillingQuery>,
) -> Result<Json<BillingUsageResponse>, StatusCode> {
    let from = parse_rfc3339_utc(&q.from)?;
    let to = parse_rfc3339_utc(&q.to)?;
    let window = TimeWindow::new(from, to).map_err(|_| StatusCode::BAD_REQUEST)?;
    let tenant = q
        .tenant_id
        .as_deref()
        .map(TenantId::parse)
        .transpose()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let bill_span = spans::billing_usage_query(tenant.is_some());
    let usage = state
        .billing_usage(tenant.as_ref(), window)
        .instrument(bill_span)
        .await
        .map_err(iceberg_500("Iceberg load_billing_usage failed"))?;
    Ok(Json(BillingUsageResponse { usage }))
}

fn iceberg_500(ctx: &'static str) -> impl Fn(TraceSinkStorageError) -> StatusCode {
    move |e| {
        tracing::error!(error = %e, context = ctx, "Iceberg handler error");
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

fn parse_rfc3339_utc(s: &str) -> Result<DateTime<Utc>, StatusCode> {
    DateTime::parse_from_rfc3339(s)
        .map_err(|_| StatusCode::BAD_REQUEST)
        .map(|dt| dt.with_timezone(&Utc))
}
