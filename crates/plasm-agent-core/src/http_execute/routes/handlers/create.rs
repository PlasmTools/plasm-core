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
        Err(e) => {
            if e == "`entities` must be non-empty" {
                return problem_response(
                    Problem::custom(
                        ProblemStatus::BAD_REQUEST,
                        Uri::from_static(problem_types::EXECUTE_EMPTY_ENTITIES),
                    )
                    .with_title("Bad Request")
                    .with_detail(e),
                );
            }
            if e.contains("PLASM_AUTH_RESOLUTION=delegated") && e.contains("principal") {
                return problem_response(
                    Problem::custom(
                        ProblemStatus::BAD_REQUEST,
                        Uri::from_static(problem_types::EXECUTE_PRINCIPAL_REQUIRED),
                    )
                    .with_title("Bad Request")
                    .with_detail(e),
                );
            }
            if e.starts_with("unknown catalog entry:") {
                return problem_response(
                    Problem::custom(
                        ProblemStatus::NOT_FOUND,
                        Uri::from_static(problem_types::EXECUTE_UNKNOWN_CATALOG_ENTRY),
                    )
                    .with_title("Not Found")
                    .with_detail(e),
                );
            }
            if e.contains("unknown entity `") && e.contains("` in this schema") {
                return problem_response(
                    Problem::custom(
                        ProblemStatus::BAD_REQUEST,
                        Uri::from_static(problem_types::EXECUTE_UNKNOWN_ENTITY),
                    )
                    .with_title("Bad Request")
                    .with_detail(e),
                );
            }
            problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_REGISTRY_ERROR),
                )
                .with_title("Bad Request")
                .with_detail(e),
            )
        }
    }
}
