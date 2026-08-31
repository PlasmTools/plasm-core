//! JSON discovery API (`/v1/*`): catalog, capability search, and operator [`tool-model`](crate::tool_model).
//! `POST /v1/terminal/discover` returns the **same Markdown** as MCP `discover_capabilities` (non-typed): fenced TSV + ambiguity notes — use discovery results to build `POST /execute`
//! (`entry_id` + deduped `entity` values from [`RankedCandidate`](plasm_core::discovery::RankedCandidate)).

use axum::body::Body;
use axum::extract::{Extension, Path, Query};
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_problem::prelude::{StatusCode as ProblemStatus, Uri};
use http_problem::Problem;
use plasm_core::discovery::{
    CapabilityQuery, CatalogEntryMeta, CgsCatalog, CgsDiscovery, DiscoveryError, DiscoveryResult,
};
use plasm_core::schema::CGS;
use plasm_discovery::DiscoveryQuery;
use serde::{Deserialize, Serialize};

use crate::http_problem_util::problem_response;
use crate::http_problem_util::problem_types;
use crate::release_version::RELEASE_VERSION;
use crate::server_state::{PlasmHostState, ToolModelHostError};
use crate::tool_model::ToolModelQuery;
use crate::tool_model_service::ToolModelServiceError;
use crate::typed_discovery_host::run_typed_catalog_discovery;
use tracing::Instrument;

#[derive(Debug, Deserialize)]
pub struct IncludeCgsQuery {
    #[serde(default)]
    pub include_cgs: bool,
}

#[derive(Debug, Serialize)]
pub struct RegistryListResponse {
    pub entries: Vec<CatalogEntryMeta>,
}

#[derive(Debug, Serialize)]
pub struct RegistryEntryResponse {
    pub entry_id: String,
    pub label: String,
    pub tags: Vec<String>,
    /// Present when `include_cgs=true` — digest for client symbol-space pinning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_cgs_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgs: Option<CGS>,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

/// Returned by [`get_auth_status`] when [`AuthFramework`] is initialized.
#[derive(Debug, Serialize)]
pub struct AuthStatusResponse {
    pub status: &'static str,
    pub storage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_source: Option<bool>,
}

/// Public health check (no incoming auth).
pub async fn health_response() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: RELEASE_VERSION,
    })
}

pub async fn get_auth_status(
    Extension(st): Extension<PlasmHostState>,
) -> Result<Json<AuthStatusResponse>, (StatusCode, Json<serde_json::Value>)> {
    if st.auth_framework().is_none() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "auth_framework_disabled",
                "detail": "auth-framework is not initialized in this process"
            })),
        ));
    }
    Ok(Json(AuthStatusResponse {
        status: "ok",
        storage: crate::auth_framework_host::auth_storage_backend_label(),
        open_source: st.saas.is_none().then_some(true),
    }))
}

fn typed_discovery_problem(e: plasm_discovery::DiscoveryError) -> Problem {
    match e {
        plasm_discovery::DiscoveryError::EmptyUtterance
        | plasm_discovery::DiscoveryError::InvalidClarificationAnswer => Problem::custom(
            ProblemStatus::BAD_REQUEST,
            Uri::from_static(problem_types::DISCOVERY_TYPED_BAD_REQUEST),
        )
        .with_title("Bad Request")
        .with_detail(e.to_string()),
        plasm_discovery::DiscoveryError::UnknownEntry(_) => Problem::custom(
            ProblemStatus::NOT_FOUND,
            Uri::from_static(problem_types::DISCOVERY_UNKNOWN_ENTRY),
        )
        .with_title("Not Found")
        .with_detail(e.to_string()),
        _ => Problem::custom(
            ProblemStatus::INTERNAL_SERVER_ERROR,
            Uri::from_static(problem_types::DISCOVERY_TYPED_ERROR),
        )
        .with_title("Discovery Error")
        .with_detail(e.to_string()),
    }
}

fn discovery_problem(e: DiscoveryError) -> Problem {
    match e {
        DiscoveryError::EmptyQuery => Problem::custom(
            ProblemStatus::BAD_REQUEST,
            Uri::from_static(problem_types::DISCOVERY_EMPTY_QUERY),
        )
        .with_title("Bad Request")
        .with_detail(e.to_string()),
        DiscoveryError::UnknownEntry(_) => Problem::custom(
            ProblemStatus::NOT_FOUND,
            Uri::from_static(problem_types::DISCOVERY_UNKNOWN_ENTRY),
        )
        .with_title("Not Found")
        .with_detail(e.to_string()),
    }
}

fn tool_model_problem(e: ToolModelHostError) -> Problem {
    match e {
        ToolModelHostError::UnknownEntry(id) => Problem::custom(
            ProblemStatus::NOT_FOUND,
            Uri::from_static(problem_types::DISCOVERY_UNKNOWN_ENTRY),
        )
        .with_title("Not Found")
        .with_detail(id),
        ToolModelHostError::Discovery(err) => discovery_problem(err),
        ToolModelHostError::Service(ToolModelServiceError::Build(build)) => Problem::custom(
            ProblemStatus::BAD_REQUEST,
            Uri::from_static(problem_types::TOOL_MODEL_BAD_REQUEST),
        )
        .with_title("Bad Request")
        .with_detail(build.to_string()),
        ToolModelHostError::Service(ToolModelServiceError::Compute(_)) => Problem::custom(
            ProblemStatus::SERVICE_UNAVAILABLE,
            Uri::from_static(problem_types::TOOL_MODEL_BAD_REQUEST),
        )
        .with_title("Service Unavailable")
        .with_detail("tool-model compute pool unavailable"),
    }
}

async fn get_registry(Extension(st): Extension<PlasmHostState>) -> Json<RegistryListResponse> {
    let reg = st.catalog.snapshot();
    Json(RegistryListResponse {
        entries: reg.list_entries(),
    })
}

async fn get_registry_entry(
    Extension(st): Extension<PlasmHostState>,
    Path(id): Path<String>,
    Query(q): Query<IncludeCgsQuery>,
) -> Response {
    let reg = st.catalog.snapshot();
    let meta = match reg.lookup_entry_meta(&id) {
        Some(m) => m,
        None => {
            return problem_response(discovery_problem(DiscoveryError::UnknownEntry(id.clone())));
        }
    };
    let (cgs, catalog_cgs_hash) = if q.include_cgs {
        match reg.load_context(&id) {
            Ok(ctx) => {
                let digest = ctx.cgs.catalog_cgs_hash_hex();
                (Some((*ctx.cgs).clone()), Some(digest))
            }
            Err(e) => return problem_response(discovery_problem(e)),
        }
    } else {
        (None, None)
    };
    Json(RegistryEntryResponse {
        entry_id: meta.entry_id,
        label: meta.label,
        tags: meta.tags,
        catalog_cgs_hash,
        cgs,
    })
    .into_response()
}

async fn get_tool_model(
    Extension(st): Extension<PlasmHostState>,
    Path(entry_id): Path<String>,
    Query(q): Query<ToolModelQuery>,
) -> Response {
    match st.build_tool_model_for_entry(&entry_id, q).await {
        Ok(body) => Json(body.as_ref()).into_response(),
        Err(e) => problem_response(tool_model_problem(e)),
    }
}

fn log_discovery_response(out: &DiscoveryResult) {
    tracing::debug!(
        candidates = out.candidates.len(),
        contexts = out.contexts.len(),
        ambiguities = out.ambiguities.len(),
        schema_neighborhoods = out.schema_neighborhoods.len(),
        entity_summaries = out.entity_summaries.len(),
        top_scores = ?out
            .candidates
            .iter()
            .take(5)
            .map(|c| (c.capability_name.as_str(), c.score))
            .collect::<Vec<_>>(),
        "plasm discovery response"
    );
}

/// Legacy typed browse (`plasm-discovery` / `AgentDiscovery`). Prefer intent-only
/// `plasm_context` or lexicon `POST /v1/discover`. Not exposed on MCP.
async fn post_discover_typed(
    Extension(st): Extension<PlasmHostState>,
    Json(query): Json<DiscoveryQuery>,
) -> Response {
    post_discover_typed_inner(st, query)
        .instrument(crate::spans::discover_query())
        .await
}

pub(crate) async fn post_discover_typed_inner(
    st: PlasmHostState,
    query: DiscoveryQuery,
) -> Response {
    tracing::debug!(
        utterance_len = query.utterance.len(),
        allowed = query.allowed_entry_ids.len(),
        max_options = query.max_options,
        "plasm typed discovery request"
    );
    let reg = st.catalog.snapshot();
    match run_typed_catalog_discovery(&reg, query, Some(st.discovery_index_cache())).await {
        Ok(out) => Json(out).into_response(),
        Err(e) => {
            tracing::debug!(error = %e, "typed discovery failed");
            problem_response(typed_discovery_problem(e))
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TerminalDiscoverBody {
    pub intent: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub allowed_entry_ids: Vec<String>,
}

async fn post_terminal_discover(
    Extension(st): Extension<PlasmHostState>,
    Json(body): Json<TerminalDiscoverBody>,
) -> Response {
    let intent = body.intent.trim();
    if intent.is_empty() {
        return problem_response(discovery_problem(DiscoveryError::EmptyQuery));
    }
    let cq = CapabilityQuery {
        tokens: intent.split_whitespace().map(|s| s.to_string()).collect(),
        phrases: vec![intent.to_string()],
        entry_ids: if body.allowed_entry_ids.is_empty() {
            None
        } else {
            Some(body.allowed_entry_ids.clone())
        },
        ..Default::default()
    };
    let reg = st.catalog.snapshot();
    let structured = match reg.discover(&cq) {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!(error = %e, "terminal structured discovery failed");
            return problem_response(discovery_problem(e));
        }
    };
    let limit = body.limit.unwrap_or(32).clamp(1, 128);

    let mut r_out = structured.clone();
    r_out.candidates = structured.candidates.iter().take(limit).cloned().collect();
    let text = crate::discovery_human_format::format_discovery_markdown(&r_out);
    match Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(text))
    {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(error = %e, "terminal discover response build failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn post_discover(
    Extension(st): Extension<PlasmHostState>,
    Json(query): Json<CapabilityQuery>,
) -> Response {
    post_discover_inner(st, query)
        .instrument(crate::spans::discover_query())
        .await
}

pub(crate) async fn post_discover_inner(st: PlasmHostState, query: CapabilityQuery) -> Response {
    tracing::debug!(
        tokens = query.tokens.len(),
        phrases = query.phrases.len(),
        entity_hints = query.entity_hints.len(),
        kinds = query.kinds.len(),
        capability_names_len = query.capability_names.as_ref().map(Vec::len),
        entry_ids_len = query.entry_ids.as_ref().map(Vec::len),
        pick_entry = query.pick_entry.as_deref(),
        pick_capabilities_len = query.pick_capabilities.as_ref().map(Vec::len),
        exclude_capabilities_len = query.exclude_capabilities.as_ref().map(Vec::len),
        expand_entities_len = query.expand_entities.as_ref().map(Vec::len),
        "plasm discovery request"
    );
    let reg = st.catalog.snapshot();
    match reg.discover(&query) {
        Ok(out) => {
            log_discovery_response(&out);
            Json(out).into_response()
        }
        Err(e) => {
            tracing::debug!(error = %e, "plasm discovery failed");
            problem_response(discovery_problem(e))
        }
    }
}

async fn get_connect_requirements(
    Extension(st): Extension<PlasmHostState>,
    Path(entry_id): Path<String>,
) -> Response {
    let reg = st.catalog.snapshot();
    if reg.lookup_entry_meta(&entry_id).is_none() {
        return problem_response(discovery_problem(DiscoveryError::UnknownEntry(
            entry_id.clone(),
        )));
    }
    match crate::binding_slots::connect_requirements_json(entry_id.as_str()) {
        Some(spec) => Json(spec).into_response(),
        None => Json(serde_json::json!({
            "entry_id": entry_id,
            "secret": { "kind": "none" },
            "bindings": [],
        }))
        .into_response(),
    }
}

/// Registry + discover (protected by incoming-auth middleware when enabled).
pub fn discovery_routes_protected() -> Router {
    Router::new()
        .route("/v1/registry", get(get_registry))
        .route("/v1/registry/{entry_id}", get(get_registry_entry))
        .route(
            "/v1/registry/{entry_id}/connect-requirements",
            get(get_connect_requirements),
        )
        .route("/v1/registry/{entry_id}/tool-model", get(get_tool_model))
        .route("/v1/discover", post(post_discover))
        .route("/v1/discover-typed", post(post_discover_typed))
        .route("/v1/terminal/discover", post(post_terminal_discover))
}
