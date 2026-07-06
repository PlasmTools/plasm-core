//! Tenant project flow policy HTTP API (`/internal/flow-policy/v1/*`).

use axum::extract::{Extension, Query};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::control_plane_http::control_plane_headers_authorized;
use crate::flow_policy_repository::{FlowPolicyRepository, FlowPolicyRepositoryError};
use crate::flow_policy_simulate::{simulate_flow_policy, SimulatePolicyArm};
use crate::flow_policy_validate::validate_flow_policy;
use crate::flow_policy_vocabulary::project_vocabulary;
use crate::http_execute::CapabilitySeed;
use crate::plan_flow_policy::FlowPolicy;
use crate::server_state::PlasmHostState;

#[derive(Debug, Deserialize)]
struct ScopeQuery {
    tenant_id: String,
    workspace_slug: String,
    project_slug: String,
}

#[derive(Debug, Deserialize)]
struct UpsertDraftBody {
    tenant_id: String,
    workspace_slug: String,
    project_slug: String,
    policy: FlowPolicy,
}

#[derive(Debug, Deserialize)]
struct ValidateBody {
    tenant_id: String,
    workspace_slug: String,
    project_slug: String,
    policy: FlowPolicy,
}

#[derive(Debug, Deserialize)]
struct PublishBody {
    tenant_id: String,
    workspace_slug: String,
    project_slug: String,
    #[serde(default)]
    published_by_subject: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SimulateBody {
    tenant_id: String,
    workspace_slug: String,
    project_slug: String,
    #[serde(default = "default_policy_arm")]
    policy_arm: SimulatePolicyArm,
    seeds: Vec<CapabilitySeed>,
    program: String,
    #[serde(default = "default_intent")]
    intent: String,
}

fn default_policy_arm() -> SimulatePolicyArm {
    SimulatePolicyArm::Draft
}

fn default_intent() -> String {
    "flow policy simulation".into()
}

pub fn flow_policy_routes() -> Router {
    Router::new()
        .route("/internal/flow-policy/v1/get", get(get_handler))
        .route(
            "/internal/flow-policy/v1/vocabulary",
            get(vocabulary_handler),
        )
        .route("/internal/flow-policy/v1/validate", post(validate_handler))
        .route(
            "/internal/flow-policy/v1/upsert-draft",
            post(upsert_draft_handler),
        )
        .route("/internal/flow-policy/v1/publish", post(publish_handler))
        .route(
            "/internal/flow-policy/v1/discard-draft",
            post(discard_draft_handler),
        )
        .route("/internal/flow-policy/v1/simulate", post(simulate_handler))
}

fn repo(st: &PlasmHostState) -> Result<&FlowPolicyRepository, StatusCode> {
    st.flow_policy_repository()
        .map(|arc| arc.as_ref())
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

async fn get_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ScopeQuery>,
) -> Result<Json<Value>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "flow policy get") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let repo = repo(&st)?;
    let row = repo
        .get_or_default(&q.tenant_id, &q.workspace_slug, &q.project_slug)
        .await
        .map_err(map_repo_err)?;
    Ok(Json(row_to_wire(&row)))
}

async fn vocabulary_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ScopeQuery>,
) -> Result<Json<Value>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "flow policy vocabulary") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let repo = repo(&st)?;
    let vocab = project_vocabulary(
        repo,
        &st.catalog,
        &q.tenant_id,
        &q.workspace_slug,
        &q.project_slug,
    )
    .await
    .map_err(map_repo_err)?;
    Ok(Json(serde_json::to_value(vocab).unwrap_or(json!({}))))
}

async fn validate_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ValidateBody>,
) -> Result<Json<Value>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "flow policy validate") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let repo = repo(&st)?;
    let vocab = project_vocabulary(
        repo,
        &st.catalog,
        &body.tenant_id,
        &body.workspace_slug,
        &body.project_slug,
    )
    .await
    .map_err(map_repo_err)?;
    let result = validate_flow_policy(&body.policy, &vocab);
    Ok(Json(serde_json::to_value(result).unwrap_or(json!({}))))
}

async fn upsert_draft_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<UpsertDraftBody>,
) -> Result<StatusCode, StatusCode> {
    if !control_plane_headers_authorized(&headers, "flow policy upsert draft") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let repo = repo(&st)?;
    let vocab = project_vocabulary(
        repo,
        &st.catalog,
        &body.tenant_id,
        &body.workspace_slug,
        &body.project_slug,
    )
    .await
    .map_err(map_repo_err)?;
    let validation = validate_flow_policy(&body.policy, &vocab);
    if !validation.ok {
        return Err(StatusCode::BAD_REQUEST);
    }
    repo.upsert_draft(
        &body.tenant_id,
        &body.workspace_slug,
        &body.project_slug,
        &body.policy,
    )
    .await
    .map_err(map_repo_err)?;
    let _ = repo
        .record_validation(
            &body.tenant_id,
            &body.workspace_slug,
            &body.project_slug,
            true,
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn publish_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<PublishBody>,
) -> Result<Json<Value>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "flow policy publish") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let repo = repo(&st)?;
    let row = repo
        .get_or_default(&body.tenant_id, &body.workspace_slug, &body.project_slug)
        .await
        .map_err(map_repo_err)?;
    if let Some(draft) = row.draft_policy.as_ref() {
        let vocab = project_vocabulary(
            repo,
            &st.catalog,
            &body.tenant_id,
            &body.workspace_slug,
            &body.project_slug,
        )
        .await
        .map_err(map_repo_err)?;
        let validation = validate_flow_policy(draft, &vocab);
        if !validation.ok {
            return Err(StatusCode::BAD_REQUEST);
        }
        let _ = repo
            .record_validation(
                &body.tenant_id,
                &body.workspace_slug,
                &body.project_slug,
                true,
            )
            .await;
    }
    let rev = repo
        .publish(
            &body.tenant_id,
            &body.workspace_slug,
            &body.project_slug,
            body.published_by_subject.as_deref(),
        )
        .await
        .map_err(map_repo_err)?;
    Ok(Json(json!({ "published_revision": rev })))
}

async fn discard_draft_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<PublishBody>,
) -> Result<StatusCode, StatusCode> {
    if !control_plane_headers_authorized(&headers, "flow policy discard draft") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let repo = repo(&st)?;
    repo.discard_draft(&body.tenant_id, &body.workspace_slug, &body.project_slug)
        .await
        .map_err(map_repo_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn simulate_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<SimulateBody>,
) -> Result<Json<Value>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "flow policy simulate") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let repo = repo(&st)?;
    let row = repo
        .get_or_default(&body.tenant_id, &body.workspace_slug, &body.project_slug)
        .await
        .map_err(map_repo_err)?;
    let result = simulate_flow_policy(
        &st,
        &row,
        body.policy_arm,
        body.seeds,
        body.program.as_str(),
        body.intent.as_str(),
    )
    .await
    .map_err(|e| {
        tracing::warn!(message = %e, "flow policy simulate failed");
        StatusCode::BAD_REQUEST
    })?;
    Ok(Json(serde_json::to_value(result).unwrap_or(json!({}))))
}

fn row_to_wire(row: &crate::flow_policy_repository::FlowPolicyRow) -> Value {
    json!({
        "tenant_id": row.tenant_id,
        "workspace_slug": row.workspace_slug,
        "project_slug": row.project_slug,
        "published_revision": row.published_revision,
        "published_policy": row.published_policy,
        "published_at": row.published_at,
        "published_by_subject": row.published_by_subject,
        "draft_policy": row.draft_policy,
        "draft_updated_at": row.draft_updated_at,
        "draft_validated_at": row.draft_validated_at,
        "draft_validation_ok": row.draft_validation_ok,
        "enforcement_active": row.published_revision > 0 && row.published_policy.is_some(),
    })
}

fn map_repo_err(e: FlowPolicyRepositoryError) -> StatusCode {
    match e {
        FlowPolicyRepositoryError::InvalidInput(_) => StatusCode::BAD_REQUEST,
        FlowPolicyRepositoryError::NoDraft | FlowPolicyRepositoryError::ValidateRequired => {
            StatusCode::CONFLICT
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
