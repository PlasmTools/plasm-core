//! GET execute session.

use super::super::super::*;

pub(crate) async fn handle_execute_session_get(
    Extension(st): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    ExecutePath {
        prompt_hash,
        session_id,
    }: ExecutePath,
) -> Response {
    let Some(sess) = st
        .get_execute_session(prompt_hash.as_str(), session_id.as_str())
        .await
    else {
        let _miss = crate::spans::execute_session_lookup_miss().entered();
        tracing::debug!(
            prompt_hash = %prompt_hash,
            session_id = %session_id,
            "execute session GET lookup miss"
        );
        return problem_response(
            Problem::custom(
                ProblemStatus::NOT_FOUND,
                Uri::from_static(problem_types::EXECUTE_UNKNOWN_SESSION),
            )
            .with_title("Not Found")
            .with_detail("unknown or expired execute session"),
        );
    };

    if !session_allows_principal(&sess, principal.as_ref()) {
        return incoming_auth_problem(
            crate::incoming_auth::IncomingAuthFailure::Invalid(
                "execute session tenant does not match caller".into(),
            ),
            true,
        );
    }

    let render_mode = st.engine.prompt_pipeline().render_mode;
    Json(create_execute_session_response(
        &sess,
        session_id.to_string(),
        wire_execute_session_prompt(&sess.prompt_text, render_mode),
        false,
    ))
    .into_response()
}
