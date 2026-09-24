//! JSON discovery API (`/v1/*`): catalog, capability search, and operator [`tool-model`](crate::tool_model).
//! Intent routing and context creation share the PostgreSQL generation and prerequisite graph.

use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_problem::prelude::{StatusCode as ProblemStatus, Uri};
use http_problem::Problem;
use plasm_core::discovery::{CatalogEntryMeta, CgsCatalog, DiscoveryError};
use plasm_core::schema::CGS;
use serde::{Deserialize, Serialize};

use crate::http_problem_util::problem_response;
use crate::http_problem_util::problem_types;
use crate::release_version::RELEASE_VERSION;
use crate::server_state::{PlasmHostState, ToolModelHostError};
use crate::tool_model::ToolModelQuery;
use crate::tool_model_service::ToolModelServiceError;

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

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IntentDiscoveryRequest {
    #[serde(default)]
    pub principal: Option<String>,
    pub intent: String,
    #[serde(default)]
    pub allowed_entry_ids: Option<Vec<String>>,
}

async fn route_http_intent(
    st: &PlasmHostState,
    body: &IntentDiscoveryRequest,
    _scope: &str,
    session: Option<&crate::execute_session::ExecuteSession>,
) -> anyhow::Result<crate::discovery_service::RoutingReceipt> {
    use crate::discovery_service::{DiscoveryService, RouteTurn};
    let generation = match session.and_then(|session| session.discovery_pin.as_ref()) {
        Some(pin) => pin.generation.clone(),
        None => st.catalog.discovery_generation()?.as_ref().clone(),
    };
    let registry = st.catalog.pinned_view(&generation).await?.snapshot();
    let mut allowed = registry
        .list_entries()
        .into_iter()
        .map(|entry| entry.entry_id)
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(restriction) = &body.allowed_entry_ids {
        allowed.retain(|id| restriction.contains(id));
    }
    let allowed = crate::discovery_store::DiscoveryAuthorization::catalogs(allowed);
    let allowed = session
        .and_then(|session| session.discovery_pin.as_ref())
        .map(|pin| pin.authorization.intersection(&allowed))
        .unwrap_or(allowed);
    let logical_session = session
        .and_then(|session| session.discovery_pin.as_ref())
        .map(|pin| pin.pin_id.as_str());
    let exposed = session
        .and_then(|session| session.teaching_exposure.as_ref())
        .map(|exposure| {
            exposure
                .surface
                .capabilities
                .iter()
                .map(|cap| plasm_core::prerequisites::CapabilityRef {
                    catalog: cap.entry_id.clone(),
                    capability: cap.capability.to_string(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let store = st.catalog.discovery_store().await?;
    let provenance = if let Some(id) = logical_session {
        store
            .intent_provenance(id)
            .await?
            .derived(body.intent.clone())?
    } else {
        crate::intent_provenance::IntentProvenance::from_turns([body.intent.clone()])?
    };
    let service = DiscoveryService::from_env(st.catalog.discovery_store().await?.clone())?;
    service
        .route_turn(RouteTurn {
            new_generation: &generation,
            intent_provenance: &provenance,
            logical_session,
            allowed: &allowed,
            exposed: &exposed,
            expires_at: std::time::SystemTime::now() + std::time::Duration::from_secs(24 * 3600),
        })
        .await
}

async fn post_discover(
    Extension(st): Extension<PlasmHostState>,
    Extension(crate::incoming_auth::IncomingPrincipal(principal)): Extension<
        crate::incoming_auth::IncomingPrincipal,
    >,
    Json(body): Json<IntentDiscoveryRequest>,
) -> Response {
    let scope = crate::incoming_auth::tenant_scope(principal.as_ref());
    match route_http_intent(&st, &body, &scope, None).await {
        Ok(receipt) => Json(receipt).into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"routing_error","detail":error.to_string()})),
        )
            .into_response(),
    }
}

async fn post_terminal_discover(
    Extension(st): Extension<PlasmHostState>,
    Extension(crate::incoming_auth::IncomingPrincipal(principal)): Extension<
        crate::incoming_auth::IncomingPrincipal,
    >,
    Json(body): Json<IntentDiscoveryRequest>,
) -> Response {
    let scope = crate::incoming_auth::tenant_scope(principal.as_ref());
    let receipt = match route_http_intent(&st, &body, &scope, None).await {
        Ok(receipt) => receipt,
        Err(error) => return (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response(),
    };
    let mut text = if let Some(recovery) = &receipt.recovery {
        let mut body = recovery.render_unmatched_markdown();
        body.push_str("\n\n");
        body
    } else if receipt.closure.is_some() {
        "Discovery: matched affirmative effect slots.\n\n".to_owned()
    } else {
        "Discovery: no additional capability matches.\n\n".to_owned()
    };
    if let Some(closure) = &receipt.closure {
        let registry = match st.catalog.pinned_view(&receipt.retrieval.generation).await {
            Ok(view) => view.snapshot(),
            Err(error) => {
                return (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response()
            }
        };
        text.push_str("```tsv\napi\tentity\tdescription\trole\n");
        for (references, role) in [
            (&closure.business, "business"),
            (&closure.input_sources, "input_source"),
            (&closure.prerequisites, "prerequisite"),
        ] {
            for reference in references {
                let ctx = match registry.load_context(&reference.catalog) {
                    Ok(ctx) => ctx,
                    Err(error) => {
                        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
                            .into_response()
                    }
                };
                let Some(cap) = ctx.cgs.capabilities.get(reference.capability.as_str()) else {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "selected capability missing",
                    )
                        .into_response();
                };
                text.push_str(&format!(
                    "{}\t{}\t{}\t{}\n",
                    reference.catalog, cap.domain, reference.capability, role
                ));
            }
        }
        text.push_str("```\n\n");
    }
    // Keep the full binding graph and continuation contract in the terminal artifact.
    match serde_json::to_string_pretty(&receipt) {
        Ok(json) => {
            text.push_str("```json\n");
            text.push_str(&json);
            text.push_str("\n```\n");
            text.into_response()
        }
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

/// Create an intent-routed execute context. Clarification returns a receipt without opening execution.
async fn post_context(
    Extension(st): Extension<PlasmHostState>,
    Extension(crate::incoming_auth::IncomingPrincipal(principal)): Extension<
        crate::incoming_auth::IncomingPrincipal,
    >,
    Json(body): Json<IntentDiscoveryRequest>,
) -> Response {
    routed_http_context(&st, principal.as_ref(), &body, None, None).await
}

pub(crate) async fn routed_http_context(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    body: &IntentDiscoveryRequest,
    session: Option<&crate::execute_session::ExecuteSession>,
    binding: Option<(&str, &str)>,
) -> Response {
    match apply_routed_http_context(st, principal, body, session, binding).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "routing_error", "detail": error.to_string()
            })),
        )
            .into_response(),
    }
}

async fn apply_routed_http_context(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    body: &IntentDiscoveryRequest,
    session: Option<&crate::execute_session::ExecuteSession>,
    binding: Option<(&str, &str)>,
) -> anyhow::Result<serde_json::Value> {
    if session.is_some_and(|session| session.discovery_pin.is_none()) {
        anyhow::bail!("intent extension requires a routed context; open one with POST /v1/context");
    }
    let scope = crate::incoming_auth::tenant_scope(principal);
    let receipt = route_http_intent(st, body, &scope, session).await?;
    if receipt.closure.is_none() {
        return Ok(serde_json::json!({"routing": receipt}));
    }
    if let Some(pin) = session.and_then(|session| session.discovery_pin.as_ref()) {
        anyhow::ensure!(
            pin.pin_id == receipt.pin_id && pin.generation == receipt.retrieval.generation,
            "routing attempted to change the pinned session generation"
        );
    }
    let routed = st.with_discovery_route(receipt).await?;
    let receipt = routed
        .discovery_route
        .as_ref()
        .expect("validated capability route");
    let closure = receipt
        .closure
        .as_ref()
        .expect("validated capability closure");
    let registry = routed.catalog.snapshot();
    let mut seeds = Vec::new();
    for reference in closure
        .business
        .iter()
        .chain(&closure.input_sources)
        .chain(&closure.prerequisites)
    {
        let context = registry.load_context(&reference.catalog)?;
        let capability = context
            .cgs
            .capabilities
            .get(reference.capability.as_str())
            .ok_or_else(|| anyhow::anyhow!("selected capability absent from pinned catalog"))?;
        seeds.push(crate::http_execute::CapabilitySeed {
            entry_id: reference.catalog.clone(),
            entity: capability.domain.to_string(),
        });
    }
    let intent = session
        .and_then(|session| session.context_intent.as_deref())
        .map(|previous| format!("{previous}\n{}", receipt.intent))
        .unwrap_or_else(|| receipt.intent.clone());
    let out = crate::http_execute::apply_capability_seeds(
        &routed,
        principal,
        binding,
        seeds,
        session
            .and_then(|session| session.principal.clone())
            .or_else(|| body.principal.clone()),
        None,
        Some(uuid::Uuid::parse_str(&receipt.pin_id)?),
        &intent,
    )
    .await
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let execute = routed
        .try_get_execute_session(&out.prompt_hash, &out.session_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("routed execute session missing after exposure"))?;
    let exposure = execute
        .teaching_exposure
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("routed execute session missing teaching exposure"))?;
    let catalogs = execute
        .contexts_by_entry
        .iter()
        .map(|(id, context)| (id.clone(), context.cgs.as_ref()))
        .collect();
    let guidance = plasm_core::prompt_render::render_prerequisite_bindings(
        closure,
        &catalogs,
        exposure.to_symbol_map().as_ref(),
    )
    .map_err(anyhow::Error::msg)?;
    Ok(serde_json::json!({"routing": receipt, "context": out, "prerequisite_guidance": guidance}))
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
        .route("/v1/context", post(post_context))
        .route("/v1/terminal/discover", post(post_terminal_discover))
}

#[cfg(test)]
mod requirement_outcome_protocol_tests {
    use super::IntentDiscoveryRequest;
    use serde_json::json;

    #[test]
    fn intent_round_trips_and_conversational_inputs_are_rejected() {
        let request: IntentDiscoveryRequest = serde_json::from_value(json!({
            "intent":"inspect records"
        }))
        .unwrap();
        assert_eq!(request.intent, "inspect records");
        for invalid in [
            json!({"intent":"inspect records","routing_ref":"receipt","clarify_choice":1}),
            json!({"intent":"inspect records","routing_ref":"receipt","clarify_choices":1}),
            json!({"intent":"inspect records","routing_ref":"receipt","clarify_choices":[1.5]}),
            json!({"intent":"inspect records","routing_ref":"receipt","clarify_choices":[-1]}),
        ] {
            assert!(serde_json::from_value::<IntentDiscoveryRequest>(invalid).is_err());
        }
    }
}
