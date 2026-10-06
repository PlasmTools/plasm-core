//! Create execute session.

use super::super::super::*;

pub(crate) async fn post_create_execute_session(
    Extension(st): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    Json(body): Json<CreateExecuteSessionBody>,
) -> Response {
    match execute_session_create_response(&st, principal.as_ref(), body).await {
        Ok(created) => {
            let location = format!("/execute/{}/{}", created.prompt_hash, created.session);
            // `prompt_hash` and `session` are in the URL; full session JSON (including Plasm instructions in `prompt`) is
            // served by GET on that same path — safe for clients that follow 303 with GET.
            (StatusCode::SEE_OTHER, [(LOCATION, location)]).into_response()
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
