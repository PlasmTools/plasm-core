//! Operation progress SSE stream.

use super::super::super::*;

#[derive(Deserialize)]
pub(crate) struct OperationStreamPath {
    prompt_hash: String,
    session_id: String,
    operation_handle: String,
}

pub(crate) async fn get_operation_progress_stream(
    Extension(st): Extension<crate::server_state::PlasmHostState>,
    Path(path): Path<OperationStreamPath>,
) -> Result<Response, Response> {
    let ph: PromptHashHex = path.prompt_hash.parse::<PromptHashHex>().map_err(|e| {
        problem_response_invalid_execute_path(StatusCode::BAD_REQUEST, e.to_string())
    })?;
    let sid: ExecuteSessionId = path.session_id.parse::<ExecuteSessionId>().map_err(|e| {
        problem_response_invalid_execute_path(StatusCode::BAD_REQUEST, e.to_string())
    })?;
    let handle =
        plasm_core::OperationHandle::parse(path.operation_handle.as_str()).map_err(|e| {
            problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Bad Request")
                .with_detail(e.to_string()),
            )
        })?;
    let Some(sess) = st.get_execute_session(ph.as_str(), sid.as_str()).await else {
        return Err(problem_response(
            Problem::custom(
                ProblemStatus::NOT_FOUND,
                Uri::from_static(problem_types::EXECUTE_UNKNOWN_SESSION),
            )
            .with_title("Not Found")
            .with_detail("execute session not found or expired"),
        ));
    };
    let Some((seq, line)) = sess.operation_progress_snapshot_line(&handle) else {
        return Err(problem_response(
            Problem::custom(
                ProblemStatus::NOT_FOUND,
                Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
            )
            .with_title("Not Found")
            .with_detail(format!("unknown operation handle `{}`", handle.as_str())),
        ));
    };
    let Some(rx) = sess.operation_progress_subscribe(&handle) else {
        return Err(problem_response(
            Problem::custom(
                ProblemStatus::NOT_FOUND,
                Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
            )
            .with_title("Not Found")
            .with_detail(format!("unknown operation handle `{}`", handle.as_str())),
        ));
    };
    Ok(crate::operation_progress_sse::operation_progress_wire_sse(
        rx, seq, line,
    ))
}
