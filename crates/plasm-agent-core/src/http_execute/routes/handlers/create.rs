//! Create execute session.

use super::super::super::*;

#[derive(serde::Deserialize)]
pub(crate) struct CreateSessionRequest {
    #[serde(flatten)]
    session: CreateExecuteSessionBody,
    #[serde(default)]
    training: Option<crate::workflow_training_wire::CreationTrainingIntent>,
}

pub(crate) async fn post_create_execute_session(
    Extension(st): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateSessionRequest>,
) -> Response {
    let pool = st.live_plan_pool();
    pool.run_local(move || create_local(st, principal, headers, body))
        .await
        .unwrap_or_else(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "session worker unavailable",
            )
                .into_response()
        })
}

async fn create_local(
    st: PlasmHostState,
    principal: Option<crate::incoming_auth::TenantPrincipal>,
    headers: axum::http::HeaderMap,
    request: CreateSessionRequest,
) -> Response {
    let mut body = request.session;
    let fresh = body.logical_session_id.is_none();
    if !fresh && request.training.is_some() {
        return (StatusCode::BAD_REQUEST, "creation training intent requires a fresh logical session; use training commands for existing sessions").into_response();
    }
    let logical = if let Some(id) = body.logical_session_id {
        let logical = crate::session_identity::LogicalSessionId(id);
        if !st
            .logical_sessions
            .verify_tenant(
                logical,
                &crate::incoming_auth::tenant_scope(principal.as_ref()),
            )
            .await
        {
            return (StatusCode::NOT_FOUND, "logical session unavailable").into_response();
        }
        if st.workflow_learning.enabled() {
            let Ok(actor) =
                crate::http_training::resolve_actor(&st, principal.as_ref(), &headers).await
            else {
                return (StatusCode::NOT_FOUND, "logical session unavailable").into_response();
            };
            let Ok(repo) = st.workflow_learning.repository().await else {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "session authority unavailable",
                )
                    .into_response();
            };
            if repo
                .check_training_controller(&actor, logical)
                .await
                .is_err()
            {
                return (StatusCode::NOT_FOUND, "logical session unavailable").into_response();
            }
        }
        logical
    } else {
        let intent = body
            .context_intent
            .as_deref()
            .filter(|intent| !intent.trim().is_empty());
        if request.training.is_some() && intent.is_none() {
            return (
                StatusCode::BAD_REQUEST,
                "training creation requires context_intent",
            )
                .into_response();
        }
        let record = match st
            .logical_sessions
            .mint_session(
                &crate::incoming_auth::tenant_scope(principal.as_ref()),
                intent.unwrap_or("Explicit catalogue session"),
            )
            .await
        {
            Ok(record) => record,
            Err(error) => {
                return (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response()
            }
        };
        body.logical_session_id = Some(record.logical_session_id.as_uuid());
        record.logical_session_id
    };
    match execute_session_create_response(&st, principal.as_ref(), body).await {
        Ok(created) => {
            let location = format!("/execute/{}/{}", created.prompt_hash, created.session);
            // `prompt_hash` and `session` are in the URL; full session JSON (including Plasm instructions in `prompt`) is
            // served by GET on that same path — safe for clients that follow 303 with GET.
            st.logical_execute_bindings
                .insert(logical.as_uuid(), created.prompt_hash, created.session)
                .await;
            let wire = crate::mcp_logical_ref::format_logical_session_wire_ref(logical);
            if fresh && st.workflow_learning.enabled() {
                if let Ok(actor) =
                    crate::http_training::resolve_actor(&st, principal.as_ref(), &headers).await
                {
                    if let Ok(repo) = st.workflow_learning.repository().await {
                        if repo.bind_session(actor.scope(), logical).await.is_ok() {
                            if let Err(error) = repo.bind_training_controller(&actor, logical).await
                            {
                                tracing::warn!(%error, "training controller unavailable; session remains usable");
                            }
                        }
                    }
                }
            }
            if let Some(training) = request.training {
                let status = match crate::http_training::resolve_actor(
                    &st,
                    principal.as_ref(),
                    &headers,
                )
                .await
                {
                    Ok(actor) => {
                        crate::workflow_training_wire::request_on_creation(
                            &st, &actor, logical, &training,
                        )
                        .await
                    }
                    Err(_) => {
                        serde_json::json!({"capture_availability":"unavailable","error":{"code":"action_denied"}})
                    }
                };
                let wire = crate::mcp_logical_ref::format_logical_session_wire_ref(logical);
                return (
                    StatusCode::SEE_OTHER,
                    [
                        ("location", location),
                        ("x-plasm-logical-session-ref", wire.clone()),
                        (
                            "x-plasm-training-status",
                            format!("/v1/training/sessions/{wire}"),
                        ),
                    ],
                    Json(serde_json::json!({"logical_session_ref":wire,"training":status})),
                )
                    .into_response();
            }
            (
                StatusCode::SEE_OTHER,
                [
                    ("location", location),
                    ("x-plasm-logical-session-ref", wire.clone()),
                    (
                        "x-plasm-training-status",
                        format!("/v1/training/sessions/{wire}"),
                    ),
                ],
            )
                .into_response()
        }
        Err(error) => {
            use crate::http_execute::context::SessionMutateError;
            let (status, problem_type, title) = match &error {
                SessionMutateError::EmptyEntities => (
                    ProblemStatus::BAD_REQUEST,
                    problem_types::EXECUTE_EMPTY_ENTITIES,
                    "Bad Request",
                ),
                SessionMutateError::Auth(plasm_runtime::AuthResolutionError::PrincipalRequired) => {
                    (
                        ProblemStatus::BAD_REQUEST,
                        problem_types::EXECUTE_PRINCIPAL_REQUIRED,
                        "Bad Request",
                    )
                }
                SessionMutateError::Discovery(
                    plasm_core::discovery::DiscoveryError::UnknownEntry(_),
                )
                | SessionMutateError::Rehydrate(
                    crate::execute_session_rehydrate::RehydrateError::UnknownEntry(_),
                ) => (
                    ProblemStatus::NOT_FOUND,
                    problem_types::EXECUTE_UNKNOWN_CATALOG_ENTRY,
                    "Not Found",
                ),
                SessionMutateError::UnknownEntity { .. }
                | SessionMutateError::SeedResolution(
                    crate::http_execute::context::SeedResolutionError::UnknownEntity { .. }
                    | crate::http_execute::context::SeedResolutionError::AmbiguousEntity { .. }
                    | crate::http_execute::context::SeedResolutionError::EmptyEntity,
                ) => (
                    ProblemStatus::BAD_REQUEST,
                    problem_types::EXECUTE_UNKNOWN_ENTITY,
                    "Bad Request",
                ),
                _ => (
                    ProblemStatus::BAD_REQUEST,
                    problem_types::EXECUTE_REGISTRY_ERROR,
                    "Bad Request",
                ),
            };
            problem_response(
                Problem::custom(status, Uri::from_static(problem_type))
                    .with_title(title)
                    .with_detail(error.to_string()),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_creation_has_no_training_intent_and_opt_in_requires_explicit_key() {
        let ordinary: CreateSessionRequest =
            serde_json::from_value(serde_json::json!({"entry_id":"matrix","entities":["Record"]}))
                .unwrap();
        assert!(ordinary.training.is_none());
        assert!(serde_json::from_value::<CreateSessionRequest>(serde_json::json!({"entry_id":"matrix","entities":["Record"],"training":{"intent":"request"}})).is_err());
        let training: CreateSessionRequest = serde_json::from_value(serde_json::json!({"entry_id":"matrix","entities":["Record"],"training":{"intent":"request","idempotency_key":Uuid::new_v4()}})).unwrap();
        assert!(training.training.is_some());
    }
}
