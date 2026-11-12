//! Authenticated HTTP adapters for prospective session training.
//! Human decisions are handled by the trusted control-plane bridge, never by
//! treating an arbitrary bearer credential as a human action.

use axum::{
    extract::{Extension, Path, Query},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::workflow_learning::{
    self as learning,
    repository::{RepositoryError, WorkflowRepository},
    training::{TrainingCandidateSelection, TrainingError, TrainingState, TrustedTrainingActor},
};
use crate::workflow_training_wire::{
    approval_url, candidate_page, decode_candidate_cursor, default_candidate_limit,
    parse_candidate_ref, training_failure, SessionCommand, TrainingStatusResponse,
};
use crate::{
    incoming_auth::{IncomingAuthMethod, IncomingPrincipal, TenantPrincipal},
    server_state::PlasmHostState,
    session_identity::LogicalSessionId,
};

pub(crate) fn training_routes() -> Router {
    Router::new()
        .merge(crate::mcp_app::mount_bundle(&crate::mcp_app::TRAINING))
        .route("/v1/training/sessions/{logical_session_ref}", get(status))
        .route(
            "/v1/training/sessions/{logical_session_ref}/executions",
            get(executions),
        )
        .route(
            "/v1/training/sessions/{logical_session_ref}/commands",
            post(command),
        )
        .route(
            "/v1/training/sessions/{logical_session_ref}/reviews",
            post(review),
        )
        .route(
            "/v1/training/sessions/{logical_session_ref}/candidates",
            get(candidates),
        )
        .route("/v1/training/candidates/{candidate_ref}", get(candidate))
        .route("/v1/training/requests", post(request_decision))
        .route("/v1/training/releases", post(request_release))
        .route(
            "/v1/training/execution-releases",
            post(request_execution_release),
        )
        .route("/v1/training/requests/{request_ref}", get(request_status))
}

fn failure(error: TrainingError) -> Response {
    let (status, failure) = training_failure(&error);
    (status, Json(failure)).into_response()
}

fn unavailable() -> Response {
    failure(TrainingError::Unavailable)
}

fn repository_failure(error: RepositoryError) -> Response {
    match error {
        RepositoryError::Training(error) => failure(error),
        RepositoryError::Unavailable | RepositoryError::ApprovalUnavailable => unavailable(),
        RepositoryError::ImmutableConflict
        | RepositoryError::LeaseConflict
        | RepositoryError::PortfolioConflict => failure(TrainingError::Conflict),
        RepositoryError::InvalidEvidence(_) | RepositoryError::IncompatibleTeachingFormat => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"code":"evidence_ineligible","message":error.to_string()})),
        )
            .into_response(),
        RepositoryError::Database(_)
        | RepositoryError::Serialization(_)
        | RepositoryError::InvalidTenantCap => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                json!({"code":"learning_unavailable","message":"Training storage is unavailable."}),
            ),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateQuery {
    #[serde(default = "default_candidate_limit")]
    limit: u32,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewBody {
    expected_revision: u64,
    idempotency_key: uuid::Uuid,
    subtasks: Vec<learning::model::AchievedSubtask>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionRequestBody {
    logical_session_ref: String,
    expected_revision: u64,
    idempotency_key: uuid::Uuid,
    selection: Vec<TrainingCandidateSelection>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRequestBody {
    logical_session_ref: String,
    expected_revision: u64,
    idempotency_key: uuid::Uuid,
    review_refs: Vec<uuid::Uuid>,
}

async fn request_release(
    Extension(host): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    headers: HeaderMap,
    Json(body): Json<ReleaseRequestBody>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || async move {
            let (repo, actor, logical) = match session_context(
                &host,
                principal.as_ref(),
                &headers,
                &body.logical_session_ref,
            )
            .await
            {
                Ok(v) => v,
                Err(r) => return *r,
            };
            match repo
                .request_training_release(
                    &actor,
                    logical,
                    body.expected_revision,
                    body.idempotency_key,
                    body.review_refs,
                )
                .await
            {
                Ok(request) => (
                    StatusCode::ACCEPTED,
                    Json(
                        json!({"approval_url":approval_url(request.request_ref),"request":request}),
                    ),
                )
                    .into_response(),
                Err(e) => failure(e),
            }
        })
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionReleaseRequestBody {
    logical_session_ref: String,
    expected_revision: u64,
    idempotency_key: uuid::Uuid,
    execution_ids: Vec<crate::workflow_learning::model::ExecutionId>,
}

async fn request_execution_release(
    Extension(host): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    headers: HeaderMap,
    Json(body): Json<ExecutionReleaseRequestBody>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || async move {
            let (repo, actor, logical) = match session_context(
                &host,
                principal.as_ref(),
                &headers,
                &body.logical_session_ref,
            )
            .await
            {
                Ok(v) => v,
                Err(r) => return *r,
            };
            match repo
                .request_training_execution_release(
                    &actor,
                    logical,
                    body.expected_revision,
                    body.idempotency_key,
                    body.execution_ids,
                )
                .await
            {
                Ok(request) => (
                    StatusCode::ACCEPTED,
                    Json(
                        json!({"approval_url":approval_url(request.request_ref),"request":request}),
                    ),
                )
                    .into_response(),
                Err(e) => failure(e),
            }
        })
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

/// Derives a project from verified identity/configuration, never caller-supplied
/// project strings. Individual API credentials retain distinct grant identities.
fn jwt_credential_fingerprint(verified_token: &str) -> String {
    format!(
        "incoming-jwt:{}",
        hex::encode(Sha256::digest(verified_token.as_bytes()))
    )
}

pub(crate) async fn resolve_actor(
    host: &PlasmHostState,
    principal: Option<&TenantPrincipal>,
    headers: &HeaderMap,
) -> Result<TrustedTrainingActor, Box<Response>> {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token);
    if let (Some(token), Some(keys), Some(configs)) = (
        bearer,
        host.mcp_transport_auth(),
        host.mcp_config_repository(),
    ) {
        if let Some(credential) = keys.verify_api_key(token).await {
            let config = configs
                .get_runtime_config(&credential.config_id)
                .await
                .map_err(|_| failure(TrainingError::Denied))?
                .ok_or_else(unavailable)?;
            let scope = learning::scope(&config.tenant_id, Some(&config));
            let subject = config
                .owner_subject
                .clone()
                .unwrap_or_else(|| config.id.to_string());
            return TrustedTrainingActor::authenticated(
                scope,
                subject,
                format!("mcp-key:{}", credential.key_id),
            )
            .map_err(|error| Box::new(failure(error)));
        }
    }
    let principal = principal.ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(json!({"code":"authentication_required","message":"Training controls require an authenticated caller."}))).into_response())?;
    let config = match host.mcp_config_repository() {
        Some(configs) => Some(
            configs
                .find_personal_runtime(&principal.tenant_id, &principal.subject)
                .await
                .map_err(|_| failure(TrainingError::Denied))?
                .ok_or_else(unavailable)?,
        ),
        None => None,
    };
    let scope = learning::scope(&principal.tenant_id, config.as_ref());
    let credential = match principal.method {
        IncomingAuthMethod::Jwt => {
            let token = bearer.ok_or_else(|| failure(TrainingError::Denied))?;
            jwt_credential_fingerprint(token)
        }
        IncomingAuthMethod::ApiKey => {
            let raw = headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .or(bearer)
                .ok_or_else(|| failure(TrainingError::Denied))?;
            format!(
                "incoming-key:{}",
                hex::encode(Sha256::digest(raw.as_bytes()))
            )
        }
    };
    TrustedTrainingActor::authenticated(scope, principal.subject.clone(), credential)
        .map_err(|error| Box::new(failure(error)))
}

async fn session_context(
    host: &PlasmHostState,
    principal: Option<&TenantPrincipal>,
    headers: &HeaderMap,
    wire: &str,
) -> Result<
    (
        Arc<WorkflowRepository>,
        TrustedTrainingActor,
        LogicalSessionId,
    ),
    Box<Response>,
> {
    let actor = resolve_actor(host, principal, headers).await?;
    let logical = crate::mcp_logical_ref::parse_logical_session_wire_ref(wire)
        .map_err(|_| failure(TrainingError::InvalidCommand))?;
    let tenant = actor.scope().tenant.strip_prefix("tenant:").unwrap_or("");
    if !host.logical_sessions.verify_tenant(logical, tenant).await {
        return Err(Box::new(unavailable()));
    }
    let repo = host.workflow_learning.repository().await.map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"code":"learning_unavailable","message":"Training is unavailable; ordinary execution remains available."}))).into_response())?;
    let bound = repo
        .session_scope(&actor.scope().tenant, logical)
        .await
        .map_err(|_| unavailable())?;
    if bound.as_ref() != Some(actor.scope()) {
        return Err(Box::new(unavailable()));
    }
    repo.check_training_controller(&actor, logical)
        .await
        .map_err(failure)?;
    Ok((repo, actor, logical))
}

async fn executions(
    Extension(host): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    headers: HeaderMap,
    Path(wire): Path<String>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || async move {
            let (repo, actor, logical) =
                match session_context(&host, principal.as_ref(), &headers, &wire).await {
                    Ok(context) => context,
                    Err(response) => return *response,
                };
            match repo.execution_index(actor.scope(), logical).await {
                Ok(executions) => Json(json!({"executions": executions})).into_response(),
                Err(error) => repository_failure(error),
            }
        })
        .await
        .unwrap_or_else(|_| unavailable())
}

async fn status(
    Extension(host): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    headers: HeaderMap,
    Path(wire): Path<String>,
) -> Response {
    let pool = host.live_plan_pool();
    pool.run_local(move || status_local(host, principal, headers, wire))
        .await
        .unwrap_or_else(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "training worker unavailable",
            )
                .into_response()
        })
}

async fn status_local(
    host: PlasmHostState,
    principal: Option<TenantPrincipal>,
    headers: HeaderMap,
    wire: String,
) -> Response {
    let (repo, actor, logical) =
        match session_context(&host, principal.as_ref(), &headers, &wire).await {
            Ok(context) => context,
            Err(response) => return *response,
        };
    match repo.training_status(actor.scope(), logical).await {
        Ok(status) => Json(TrainingStatusResponse::from(status)).into_response(),
        Err(error) => failure(error),
    }
}

async fn command(
    Extension(host): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    headers: HeaderMap,
    Path(wire): Path<String>,
    Json(command): Json<SessionCommand>,
) -> Response {
    let pool = host.live_plan_pool();
    pool.run_local(move || command_local(host, principal, headers, wire, command))
        .await
        .unwrap_or_else(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "training worker unavailable",
            )
                .into_response()
        })
}

async fn command_local(
    host: PlasmHostState,
    principal: Option<TenantPrincipal>,
    headers: HeaderMap,
    wire: String,
    command: SessionCommand,
) -> Response {
    let (repo, actor, logical) =
        match session_context(&host, principal.as_ref(), &headers, &wire).await {
            Ok(context) => context,
            Err(response) => return *response,
        };
    match repo
        .training_command(&actor, logical, &command.envelope())
        .await
    {
        Ok(status) => {
            let code = if matches!(
                status.training_state,
                TrainingState::AwaitingAuthorization { .. }
            ) {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            };
            (code, Json(TrainingStatusResponse::from(status))).into_response()
        }
        Err(error) => failure(error),
    }
}

async fn candidates(
    Extension(host): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    headers: HeaderMap,
    Path(wire): Path<String>,
    Query(query): Query<CandidateQuery>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || async move {
            let (repo, actor, logical) =
                match session_context(&host, principal.as_ref(), &headers, &wire).await {
                    Ok(v) => v,
                    Err(r) => return *r,
                };
            let cursor =
                match decode_candidate_cursor(query.cursor.as_deref(), actor.scope(), logical) {
                    Ok(v) => v,
                    Err(e) => return failure(e),
                };
            match repo
                .list_training_candidates(actor.scope(), logical, cursor.as_ref(), query.limit)
                .await
            {
                Ok(page) => match candidate_page(page, actor.scope(), logical) {
                    Ok(page) => Json(page).into_response(),
                    Err(e) => failure(e),
                },
                Err(e) => repository_failure(e),
            }
        })
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

async fn candidate(
    Extension(host): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    headers: HeaderMap,
    Path(reference): Path<String>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || async move {
            let actor = match resolve_actor(&host, principal.as_ref(), &headers).await {
                Ok(v) => v,
                Err(r) => return *r,
            };
            let reference = match parse_candidate_ref(&reference) {
                Ok(v) => v,
                Err(e) => return failure(e),
            };
            let repo = match host.workflow_learning.repository().await {
                Ok(repo) => repo,
                Err(_) => {
                    return (StatusCode::SERVICE_UNAVAILABLE, "training unavailable")
                        .into_response()
                }
            };
            let logical = match repo
                .training_candidate_session(actor.scope(), &reference)
                .await
            {
                Ok(logical) => logical,
                Err(error) => return repository_failure(error),
            };
            if let Err(error) = repo.check_training_controller(&actor, logical).await {
                return failure(error);
            }
            match repo
                .read_training_candidate(actor.scope(), &reference)
                .await
            {
                Ok(candidate) => Json(candidate).into_response(),
                Err(e) => repository_failure(e),
            }
        })
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

async fn request_decision(
    Extension(host): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    headers: HeaderMap,
    Json(body): Json<DecisionRequestBody>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || async move {
            let (repo, actor, logical) = match session_context(
                &host,
                principal.as_ref(),
                &headers,
                &body.logical_session_ref,
            )
            .await
            {
                Ok(v) => v,
                Err(r) => return *r,
            };
            match repo
                .request_candidate_decision(
                    &actor,
                    logical,
                    body.expected_revision,
                    body.idempotency_key,
                    body.selection,
                )
                .await
            {
                Ok(request) => (
                    StatusCode::ACCEPTED,
                    Json(
                        json!({"approval_url":approval_url(request.request_ref),"request":request}),
                    ),
                )
                    .into_response(),
                Err(e) => failure(e),
            }
        })
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

async fn request_status(
    Extension(host): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    headers: HeaderMap,
    Path(request_ref): Path<uuid::Uuid>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || async move {
            let actor = match resolve_actor(&host, principal.as_ref(), &headers).await {
                Ok(v) => v,
                Err(r) => return *r,
            };
            let repo = match host.workflow_learning.repository().await {
                Ok(repo) => repo,
                Err(_) => {
                    return (StatusCode::SERVICE_UNAVAILABLE, "training unavailable")
                        .into_response()
                }
            };
            match repo.read_training_request(&actor, request_ref).await {
                Ok(request) => Json(
                    json!({"approval_url":approval_url(request.request_ref),"request":request}),
                )
                .into_response(),
                Err(e) => failure(e),
            }
        })
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

async fn review(
    Extension(host): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    headers: HeaderMap,
    Path(wire): Path<String>,
    Json(body): Json<ReviewBody>,
) -> Response {
    host.live_plan_pool().run_local(move||async move {
        let (_,actor,logical)=match session_context(&host,principal.as_ref(),&headers,&wire).await {Ok(v)=>v,Err(r)=>return *r};
        if body.subtasks.is_empty() || body.subtasks.len()>16 || body.subtasks.iter().any(|s|s.execution_ids.is_empty()||s.execution_ids.len()>32) {return failure(TrainingError::InvalidCommand);}
        let Some((prompt_hash,execute_session))=host.logical_execute_bindings.get(&logical.as_uuid()).await else {return unavailable();};
        let request=learning::ReviewSession{scope:actor.scope().clone(),logical_session:logical,prompt_hash,execute_session,subtasks:body.subtasks,actor,expected_revision:body.expected_revision,idempotency_key:body.idempotency_key};
        match host.workflow_learning.seal_review(&host,request).await {
            Ok(sealed)=>{host.workflow_learning.start_worker();(StatusCode::ACCEPTED,Json(json!({"review_ref":sealed.id,"evidence_digest":sealed.digest,"status":"queued","assurance":"agent_attested"}))).into_response()},
            Err(learning::ReviewError::Repository(e))=>repository_failure(e),
            Err(e)=>(StatusCode::UNPROCESSABLE_ENTITY,Json(json!({"code":"evidence_incomplete","message":e.to_string()}))).into_response(),
        }
    }).await.unwrap_or_else(|_|failure(TrainingError::Unavailable))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_resolution_is_not_an_http_session_command() {
        assert!(serde_json::from_value::<SessionCommand>(json!({"action":"resolve_request","request_ref":uuid::Uuid::nil(),"request_revision":1,"approve":true,"expected_revision":1,"idempotency_key":uuid::Uuid::nil()})).is_err());
    }
}

#[cfg(test)]
mod router_authority_tests {
    use super::*;
    #[test]
    fn jwt_rotation_changes_individual_credential_without_recording_the_token() {
        let first = jwt_credential_fingerprint("verified-first-token-for-same-subject");
        let second = jwt_credential_fingerprint("verified-second-token-for-same-subject");
        assert_ne!(first, second);
        assert!(!first.contains("verified-first"));
        assert_eq!(
            first,
            jwt_credential_fingerprint("verified-first-token-for-same-subject")
        );
    }

    use tower::ServiceExt;

    #[tokio::test]
    async fn training_owner_bridge_bypasses_incoming_auth_but_requires_own_secret() {
        let host = crate::http::build_plasm_host_state_with_learning(
            crate::http::PlasmHostBootstrap {
                engine: plasm_runtime::ExecutionEngine::new(
                    plasm_runtime::ExecutionConfig::default(),
                )
                .unwrap(),
                mode: plasm_runtime::ExecutionMode::Live,
                registry: Arc::new(plasm_core::CgsRegistry::from_pairs(Vec::new())),
                catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
                incoming_auth: Some(Arc::new(
                    crate::incoming_auth::IncomingAuthVerifier::new(
                        crate::incoming_auth::IncomingAuthConfig {
                            mode: crate::incoming_auth::IncomingAuthMode::Required,
                            jwt_secret: Some(
                                "fixture-incoming-secret-at-least-thirty-two-bytes".into(),
                            ),
                            jwt_issuer: None,
                            jwt_audience: None,
                            api_keys_file: None,
                        },
                    )
                    .unwrap(),
                )),
                run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
                session_graph_persistence: None,
                oss_local_filesystem_defaults: false,
            },
            Arc::new(learning::WorkflowLearning::configured(
                learning::LearningMode::Disabled,
                None,
            )),
        )
        .unwrap();
        let router = crate::http::discovery_execute_router(host);
        let body = json!({"identity":{"tenant_id":"fixture","subject":"owner","projects":["p"]},"request_ref":uuid::Uuid::nil()}).to_string();
        let request = || {
            axum::http::Request::builder()
                .method("POST")
                .uri("/internal/training/v1/inspect")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.clone()))
                .unwrap()
        };
        // A 403 is the bridge's own guard; inbound bearer middleware would return 401.
        assert_eq!(
            router.clone().oneshot(request()).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let previous = std::env::var_os("PLASM_MCP_CONTROL_PLANE_SECRET");
        std::env::set_var(
            "PLASM_MCP_CONTROL_PLANE_SECRET",
            "training-route-fixture-control-secret",
        );
        let mut authorized = request();
        authorized.headers_mut().insert(
            "x-plasm-control-plane-secret",
            "training-route-fixture-control-secret".parse().unwrap(),
        );
        let response = router.clone().oneshot(authorized).await.unwrap();
        match previous {
            Some(value) => std::env::set_var("PLASM_MCP_CONTROL_PLANE_SECRET", value),
            None => std::env::remove_var("PLASM_MCP_CONTROL_PLANE_SECRET"),
        }
        assert_eq!(response.status(), StatusCode::NOT_FOUND); // Passed authority; disabled repository unavailable.
        let page = axum::http::Request::builder()
            .uri(format!("/training/requests/{}", uuid::Uuid::nil()))
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(router.oneshot(page).await.unwrap().status(), StatusCode::OK);
    }
}
