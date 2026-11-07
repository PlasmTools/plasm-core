//! Human decisions cross an authenticated owner/control-plane boundary.
//! Ordinary MCP/HTTP credentials can request a decision but cannot impersonate it.
use crate::server_state::PlasmHostState;
use crate::workflow_learning::{
    self,
    model::LibraryScope,
    repository::{TrainingCandidateRef, WorkflowRepository},
    training::{
        TrainingCandidateSelection, TrainingCommand, TrainingCommandEnvelope,
        TrainingDecisionRequest, TrainingError, TrainingGrantSpec, TrainingRequestPurpose,
        TrustedTrainingActor,
    },
};
use axum::{
    extract::Extension,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use subtle::ConstantTimeEq;

pub const TRAINING_HTML: &str = include_str!("mcp_app/training.html");

pub(crate) fn routes() -> Router {
    Router::new()
        .route("/training/requests/{request_ref}", get(page))
        .route("/internal/training/v1/inspect", post(inspect))
        .route("/internal/training/v1/decide", post(decide))
        .route("/internal/training/v1/grants", post(grant))
        .route("/internal/training/v1/grants/revoke", post(revoke_grant))
        .route("/internal/training/v1/issuer/revoke", post(revoke_issuer))
        .route(
            "/internal/training/v1/recuration/preview",
            post(preview_recuration),
        )
        .route(
            "/internal/training/v1/recuration/decide",
            post(decide_recuration),
        )
        .route(
            "/internal/training/v1/archive-authorization",
            post(authorize_archive),
        )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecurationPreviewRequest {
    identity: HumanIdentity,
    project: String,
    source_example: uuid::Uuid,
    survivor_example: uuid::Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecurationDecisionRequest {
    identity: HumanIdentity,
    project: String,
    plan: workflow_learning::repository::CrossFamilyRecurationPlan,
    decision_id: uuid::Uuid,
}

fn recuration_failure(
    error: workflow_learning::repository::CrossFamilyRecurationError,
) -> Response {
    use workflow_learning::repository::CrossFamilyRecurationError as E;
    let (status, code) = match &error {
        E::Denied => (StatusCode::FORBIDDEN, "recuration_denied"),
        E::StaleRevision => (StatusCode::CONFLICT, "recuration_stale_revision"),
        E::NotEquivalent | E::IntentTooLarge | E::MissingEvidence => {
            (StatusCode::UNPROCESSABLE_ENTITY, "recuration_not_eligible")
        }
        E::Database(_) | E::Serialization(_) | E::Repository(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "recuration_unavailable")
        }
    };
    (
        status,
        Json(json!({"code": code, "message": error.to_string()})),
    )
        .into_response()
}

async fn recuration_owner(
    host: &PlasmHostState,
    headers: &HeaderMap,
    identity: &HumanIdentity,
    project: String,
) -> Result<(std::sync::Arc<WorkflowRepository>, TrustedTrainingActor), TrainingError> {
    let authority = owner_authority(host, headers)?;
    let scope = LibraryScope {
        tenant: workflow_learning::learning_tenant(&identity.tenant_id),
        project,
    };
    let actor = identity.actor(scope, authority)?;
    let repo = host
        .workflow_learning
        .repository()
        .await
        .map_err(|_| TrainingError::Unavailable)?;
    Ok((repo, actor))
}

async fn preview_recuration(
    Extension(host): Extension<PlasmHostState>,
    headers: HeaderMap,
    Json(body): Json<RecurationPreviewRequest>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || async move {
            let (repo, actor) =
                match recuration_owner(&host, &headers, &body.identity, body.project).await {
                    Ok(value) => value,
                    Err(error) => return failure(error),
                };
            match repo
                .describe_cross_family_recuration(
                    &actor,
                    body.source_example,
                    body.survivor_example,
                )
                .await
            {
                Ok(preview) => Json(json!({"preview": preview})).into_response(),
                Err(error) => recuration_failure(error),
            }
        })
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

async fn decide_recuration(
    Extension(host): Extension<PlasmHostState>,
    headers: HeaderMap,
    Json(body): Json<RecurationDecisionRequest>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || async move {
            let (repo, actor) =
                match recuration_owner(&host, &headers, &body.identity, body.project).await {
                    Ok(value) => value,
                    Err(error) => return failure(error),
                };
            match repo
                .recurate_cross_family(&actor, &body.plan, body.decision_id)
                .await
            {
                Ok(receipt) => Json(json!({"receipt": receipt})).into_response(),
                Err(error) => recuration_failure(error),
            }
        })
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

async fn page() -> Response {
    (
        [
            ("cache-control", "no-store"),
            ("referrer-policy", "no-referrer"),
            ("x-content-type-options", "nosniff"),
            ("content-security-policy", "frame-ancestors 'none'"),
        ],
        Html(TRAINING_HTML),
    )
        .into_response()
}

fn failure(error: TrainingError) -> Response {
    let (status, body) = crate::workflow_training_wire::training_failure(&error);
    (status, Json(body)).into_response()
}

/// Unlike development control-plane routes, training never accepts an implicit default secret.
#[derive(Clone, Copy)]
enum OwnerAuthority {
    ControlPlane,
    ApplianceOwner,
}

fn owner_control(headers: &HeaderMap) -> Option<OwnerAuthority> {
    let configured = crate::control_plane_http::control_plane_secret_from_env_strict();
    let presented = headers
        .get("x-plasm-control-plane-secret")
        .and_then(|v| v.to_str().ok());
    let control = match (configured, presented) {
        (Some(expected), Some(actual)) => {
            expected.len() == actual.len()
                && bool::from(expected.as_bytes().ct_eq(actual.as_bytes()))
        }
        _ => false,
    };
    if control {
        Some(OwnerAuthority::ControlPlane)
    } else if crate::control_plane_http::outbound_setup_headers_authorized(headers) {
        Some(OwnerAuthority::ApplianceOwner)
    } else {
        None
    }
}

fn appliance_scope() -> LibraryScope {
    let tenant = crate::appliance_mcp_defaults::appliance_mcp_tenant_id();
    let workspace = crate::appliance_mcp_defaults::appliance_mcp_workspace_slug();
    let project = crate::appliance_mcp_defaults::appliance_mcp_project_slug();
    LibraryScope {
        tenant: workflow_learning::learning_tenant(&tenant),
        project: serde_json::to_string(&(workspace, project))
            .expect("appliance project serializes"),
    }
}

/// Authenticate a local appliance owner using the configured setup credential.
/// A terminal or knowledge of a session identifier is not authority.
pub fn authenticate_local_training_owner(
    host: &PlasmHostState,
    setup_secret: &str,
) -> Result<TrustedTrainingActor, TrainingError> {
    if host.saas.is_some() {
        return Err(TrainingError::Denied);
    }
    let mut headers = HeaderMap::new();
    let value = setup_secret.parse().map_err(|_| TrainingError::Denied)?;
    headers.insert(
        crate::control_plane_http::X_PLASM_OUTBOUND_SETUP_SECRET,
        value,
    );
    if !crate::control_plane_http::outbound_setup_headers_authorized(&headers) {
        return Err(TrainingError::Denied);
    }
    TrustedTrainingActor::human_owner(
        appliance_scope(),
        "appliance-owner".into(),
        "appliance-owner-control".into(),
    )
}

fn owner_authority(
    host: &PlasmHostState,
    headers: &HeaderMap,
) -> Result<OwnerAuthority, TrainingError> {
    let authority = owner_control(headers).ok_or(TrainingError::Denied)?;
    if matches!(authority, OwnerAuthority::ApplianceOwner) && host.saas.is_some() {
        return Err(TrainingError::Denied);
    }
    Ok(authority)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HumanIdentity {
    tenant_id: String,
    subject: String,
    /// Verified project memberships supplied by the authenticated control plane.
    projects: Vec<String>,
}

impl HumanIdentity {
    fn actor(
        &self,
        scope: LibraryScope,
        authority: OwnerAuthority,
    ) -> Result<TrustedTrainingActor, TrainingError> {
        if matches!(authority, OwnerAuthority::ApplianceOwner) {
            if scope != appliance_scope() {
                return Err(TrainingError::Denied);
            }
            return TrustedTrainingActor::human_owner(
                scope,
                "appliance-owner".into(),
                "appliance-owner-control".into(),
            );
        }
        if self.subject.trim().is_empty()
            || scope.tenant != workflow_learning::learning_tenant(&self.tenant_id)
            || !self.projects.contains(&scope.project)
        {
            return Err(TrainingError::Denied);
        }
        TrustedTrainingActor::human_owner(
            scope,
            self.subject.clone(),
            format!("owner-control:{}", self.subject),
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectRequest {
    identity: HumanIdentity,
    request_ref: uuid::Uuid,
}

async fn human_request(
    host: &PlasmHostState,
    headers: &HeaderMap,
    identity: &HumanIdentity,
    request_ref: uuid::Uuid,
) -> Result<
    (
        std::sync::Arc<WorkflowRepository>,
        TrustedTrainingActor,
        TrainingDecisionRequest,
    ),
    TrainingError,
> {
    let authority = owner_authority(host, headers)?;
    let repository = host
        .workflow_learning
        .repository()
        .await
        .map_err(|_| TrainingError::Unavailable)?;
    let tenant = match authority {
        OwnerAuthority::ControlPlane => workflow_learning::learning_tenant(&identity.tenant_id),
        OwnerAuthority::ApplianceOwner => appliance_scope().tenant,
    };
    let scope = repository
        .training_request_scope(&tenant, request_ref)
        .await?;
    let actor = identity.actor(scope, authority)?;
    let request = repository
        .read_training_request(&actor, request_ref)
        .await?;
    Ok((repository, actor, request))
}

async fn inspect(
    Extension(host): Extension<PlasmHostState>,
    headers: HeaderMap,
    Json(body): Json<InspectRequest>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || inspect_local(host, headers, body))
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

async fn inspect_local(host: PlasmHostState, headers: HeaderMap, body: InspectRequest) -> Response {
    let (repository, actor, request) =
        match human_request(&host, &headers, &body.identity, body.request_ref).await {
            Ok(value) => value,
            Err(error) => return failure(error),
        };
    let status = match repository
        .training_status(actor.scope(), request.logical_session)
        .await
    {
        Ok(status) => status,
        Err(error) => return failure(error),
    };
    let mut candidates = Vec::new();
    if let TrainingRequestPurpose::CandidateDecision { selection } = &request.purpose {
        for item in selection {
            match repository
                .read_training_candidate(
                    actor.scope(),
                    &TrainingCandidateRef {
                        review_ref: item.review_ref,
                        candidate_hash: item.candidate_hash.clone(),
                    },
                )
                .await
            {
                Ok(detail) => candidates.push(detail),
                Err(_) => return failure(TrainingError::Unavailable),
            }
        }
    }
    Json(json!({"request": request,"training":status,"candidates":candidates,
        "disclosure": {
            "destination": "The selected tenant and project teaching library.",
            "material": "Eligible whole-program source, compiled plan, catalogue contracts, run receipts, review claims and supporting evidence.",
            "processing": "Candidate preparation sends selected intent and program evidence to OpenRouter for normalization and embeddings using the configured models.",
            "retention": "The workflow learning ledger has no automatic expiration; retained material persists until an operator removes it under deployment policy.",
            "publication": "Human reviewed unless a separate automatic-publication delegation is explicitly granted."
        }
    })).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HumanDecision {
    identity: HumanIdentity,
    request_ref: uuid::Uuid,
    request_revision: u64,
    expected_revision: u64,
    idempotency_key: uuid::Uuid,
    approve: bool,
    selections: Vec<TrainingCandidateSelection>,
}

async fn decide(
    Extension(host): Extension<PlasmHostState>,
    headers: HeaderMap,
    Json(body): Json<HumanDecision>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || decide_local(host, headers, body))
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

async fn decide_local(host: PlasmHostState, headers: HeaderMap, body: HumanDecision) -> Response {
    let (repository, actor, request) =
        match human_request(&host, &headers, &body.identity, body.request_ref).await {
            Ok(value) => value,
            Err(error) => return failure(error),
        };
    match request.purpose {
        TrainingRequestPurpose::Consent => {
            if !body.selections.is_empty() {
                return failure(TrainingError::InvalidCommand);
            }
            let command = TrainingCommandEnvelope {
                idempotency_key: body.idempotency_key,
                expected_revision: body.expected_revision,
                command: TrainingCommand::ResolveRequest {
                    request_ref: body.request_ref,
                    request_revision: body.request_revision,
                    approve: body.approve,
                },
            };
            match repository
                .training_command(&actor, request.logical_session, &command)
                .await
            {
                Ok(receipt) => {
                    Json(json!({"status":"decision_recorded","receipt":receipt})).into_response()
                }
                Err(error) => failure(error),
            }
        }
        TrainingRequestPurpose::CandidateDecision { .. } => {
            match repository
                .resolve_training_candidate_request(
                    &actor,
                    body.request_ref,
                    body.request_revision,
                    body.idempotency_key,
                    &body.selections,
                    body.approve,
                )
                .await
            {
                Ok(receipt) => {
                    Json(json!({"status":"decision_recorded","receipt":receipt})).into_response()
                }
                Err(error) => (
                    StatusCode::CONFLICT,
                    Json(json!({"code":"decision_conflict","message":error.to_string()})),
                )
                    .into_response(),
            }
        }
        TrainingRequestPurpose::ReleaseHeld { .. }
        | TrainingRequestPurpose::ReleaseExecutions { .. } => {
            if !body.selections.is_empty() {
                return failure(TrainingError::InvalidCommand);
            }
            let command = TrainingCommandEnvelope {
                idempotency_key: body.idempotency_key,
                expected_revision: body.expected_revision,
                command: TrainingCommand::ResolveRelease {
                    request_ref: body.request_ref,
                    request_revision: body.request_revision,
                    approve: body.approve,
                },
            };
            match repository
                .training_command(&actor, request.logical_session, &command)
                .await
            {
                Ok(receipt) => {
                    Json(json!({"status":"decision_recorded","receipt":receipt})).into_response()
                }
                Err(error) => failure(error),
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantRequest {
    identity: HumanIdentity,
    project: String,
    idempotency_key: uuid::Uuid,
    grant: TrainingGrantSpec,
}

async fn grant(
    Extension(host): Extension<PlasmHostState>,
    headers: HeaderMap,
    Json(body): Json<GrantRequest>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || grant_local(host, headers, body))
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

async fn grant_local(host: PlasmHostState, headers: HeaderMap, body: GrantRequest) -> Response {
    let authority = match owner_authority(&host, &headers) {
        Ok(authority) => authority,
        Err(error) => return failure(error),
    };
    let scope = LibraryScope {
        tenant: workflow_learning::learning_tenant(&body.identity.tenant_id),
        project: body.project,
    };
    let actor = match body.identity.actor(scope, authority) {
        Ok(actor) => actor,
        Err(error) => return failure(error),
    };
    let repository = match host.workflow_learning.repository().await {
        Ok(repository) => repository,
        Err(_) => return failure(TrainingError::Unavailable),
    };
    match repository
        .install_training_grant(&actor, body.idempotency_key, body.grant)
        .await
    {
        Ok(grant) => Json(json!({"grant":grant})).into_response(),
        Err(error) => failure(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveAuthorization {
    identity: HumanIdentity,
    project: String,
    archive_digest: String,
    evidence: workflow_learning::archive::EvidenceReference,
    decision_id: uuid::Uuid,
    expires_at: chrono::DateTime<chrono::Utc>,
}

async fn authorize_archive(
    Extension(host): Extension<PlasmHostState>,
    headers: HeaderMap,
    Json(body): Json<ArchiveAuthorization>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || authorize_archive_local(host, headers, body))
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

async fn authorize_archive_local(
    host: PlasmHostState,
    headers: HeaderMap,
    body: ArchiveAuthorization,
) -> Response {
    let authority = match owner_authority(&host, &headers) {
        Ok(authority) => authority,
        Err(error) => return failure(error),
    };
    let scope = LibraryScope {
        tenant: workflow_learning::learning_tenant(&body.identity.tenant_id),
        project: body.project,
    };
    let actor = match body.identity.actor(scope, authority) {
        Ok(actor) => actor,
        Err(error) => return failure(error),
    };
    let digest = match workflow_learning::archive::ArchiveDigest::try_from(body.archive_digest) {
        Ok(value) => value,
        Err(_) => return failure(TrainingError::InvalidCommand),
    };
    let repository = match host.workflow_learning.repository().await {
        Ok(repository) => repository,
        Err(_) => return failure(TrainingError::Unavailable),
    };
    match repository.authorize_imported_review(&actor, &digest, &body.evidence, body.decision_id, body.expires_at).await {
        Ok(()) => Json(json!({"status":"authorized","archive_digest":digest.as_str(),"evidence":body.evidence,"decision_id":body.decision_id,"expires_at":body.expires_at})).into_response(),
        Err(_) => failure(TrainingError::Conflict),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeGrantRequest {
    identity: HumanIdentity,
    project: String,
    idempotency_key: uuid::Uuid,
    grant_ref: uuid::Uuid,
    expected_revision: u64,
}

async fn revoke_grant(
    Extension(host): Extension<PlasmHostState>,
    headers: HeaderMap,
    Json(body): Json<RevokeGrantRequest>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || revoke_grant_local(host, headers, body))
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

async fn revoke_grant_local(
    host: PlasmHostState,
    headers: HeaderMap,
    body: RevokeGrantRequest,
) -> Response {
    let authority = match owner_authority(&host, &headers) {
        Ok(authority) => authority,
        Err(error) => return failure(error),
    };
    let scope = LibraryScope {
        tenant: workflow_learning::learning_tenant(&body.identity.tenant_id),
        project: body.project,
    };
    let actor = match body.identity.actor(scope, authority) {
        Ok(actor) => actor,
        Err(error) => return failure(error),
    };
    let repository = match host.workflow_learning.repository().await {
        Ok(repository) => repository,
        Err(_) => return failure(TrainingError::Unavailable),
    };
    match repository
        .revoke_training_grant(
            &actor,
            body.idempotency_key,
            body.grant_ref,
            body.expected_revision,
        )
        .await
    {
        Ok(()) => Json(json!({"status":"revoked","grant_ref":body.grant_ref})).into_response(),
        Err(error) => failure(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeIssuerRequest {
    identity: HumanIdentity,
    project: String,
    idempotency_key: uuid::Uuid,
    issuer_subject: String,
}

async fn revoke_issuer(
    Extension(host): Extension<PlasmHostState>,
    headers: HeaderMap,
    Json(body): Json<RevokeIssuerRequest>,
) -> Response {
    host.live_plan_pool()
        .run_local(move || revoke_issuer_local(host, headers, body))
        .await
        .unwrap_or_else(|_| failure(TrainingError::Unavailable))
}

async fn revoke_issuer_local(
    host: PlasmHostState,
    headers: HeaderMap,
    body: RevokeIssuerRequest,
) -> Response {
    let authority = match owner_authority(&host, &headers) {
        Ok(authority) => authority,
        Err(error) => return failure(error),
    };
    let scope = LibraryScope {
        tenant: workflow_learning::learning_tenant(&body.identity.tenant_id),
        project: body.project,
    };
    let actor = match body.identity.actor(scope, authority) {
        Ok(actor) => actor,
        Err(error) => return failure(error),
    };
    let repository = match host.workflow_learning.repository().await {
        Ok(repository) => repository,
        Err(_) => return failure(TrainingError::Unavailable),
    };
    match repository
        .revoke_training_issuer(&actor, body.idempotency_key, &body.issuer_subject)
        .await
    {
        Ok(receipt) => Json(json!({"status":"revoked","receipt":receipt})).into_response(),
        Err(error) => failure(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recuration_wire_cannot_supply_authority() {
        let value = json!({
            "identity": {"tenant_id":"t", "subject":"s", "projects":["p"]},
            "project":"p", "source_example":uuid::Uuid::new_v4(),
            "survivor_example":uuid::Uuid::new_v4(), "authority":"human_owner"
        });
        assert!(serde_json::from_value::<RecurationPreviewRequest>(value).is_err());
        assert_eq!(
            recuration_failure(workflow_learning::repository::CrossFamilyRecurationError::Denied)
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    #[test]
    fn human_identity_is_scoped_and_cannot_claim_another_tenant() {
        let identity = HumanIdentity {
            tenant_id: "one".into(),
            subject: "owner".into(),
            projects: vec!["p".into()],
        };
        assert!(identity
            .actor(
                LibraryScope {
                    tenant: "tenant:one".into(),
                    project: "p".into()
                },
                OwnerAuthority::ControlPlane
            )
            .is_ok());
        assert!(identity
            .actor(
                LibraryScope {
                    tenant: "tenant:two".into(),
                    project: "p".into()
                },
                OwnerAuthority::ControlPlane
            )
            .is_err());
        assert!(identity
            .actor(
                LibraryScope {
                    tenant: "tenant:one".into(),
                    project: "other".into()
                },
                OwnerAuthority::ControlPlane
            )
            .is_err());
        assert!(identity
            .actor(
                LibraryScope {
                    tenant: "tenant:one".into(),
                    project: "p".into()
                },
                OwnerAuthority::ApplianceOwner
            )
            .is_err());
        assert!(identity
            .actor(appliance_scope(), OwnerAuthority::ApplianceOwner)
            .is_ok());
    }
    #[test]
    fn browser_bearer_is_not_owner_control() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer model-credential".parse().unwrap());
        assert!(owner_control(&headers).is_none());
    }
}
