//! Shared execute HTTP wire helpers (session JSON, path problems).

use axum::http::StatusCode;
use axum::response::Response;
use http_problem::prelude::{StatusCode as ProblemStatus, Uri};
use http_problem::Problem;
use plasm_core::{teaching_tsv_table_from_wrapped_prompt, PromptRenderMode};

use crate::http_problem_util::{problem_response, problem_types};

use super::CreateExecuteSessionResponse;

pub(crate) fn problem_response_invalid_execute_path(
    axum_status: StatusCode,
    detail: impl Into<String>,
) -> Response {
    let pstatus = if axum_status == StatusCode::BAD_REQUEST {
        ProblemStatus::BAD_REQUEST
    } else if axum_status == StatusCode::INTERNAL_SERVER_ERROR {
        ProblemStatus::INTERNAL_SERVER_ERROR
    } else {
        ProblemStatus::BAD_REQUEST
    };
    let title = if pstatus == ProblemStatus::INTERNAL_SERVER_ERROR {
        "Internal Server Error"
    } else {
        "Bad Request"
    };
    problem_response(
        Problem::custom(
            pstatus,
            Uri::from_static(problem_types::EXECUTE_INVALID_PATH_PARAM),
        )
        .with_title(title)
        .with_detail(detail.into()),
    )
}

pub(crate) fn wire_execute_session_prompt(
    stored_prompt: &str,
    render_mode: PromptRenderMode,
) -> String {
    let fence = render_mode.markdown_fence_info_string();
    if let Some(table) = teaching_tsv_table_from_wrapped_prompt(stored_prompt, fence) {
        format!("```{fence}\n{}\n```\n", table.trim_end())
    } else {
        stored_prompt.to_string()
    }
}

pub(crate) fn create_execute_session_response(
    sess: &crate::execute_session::ExecuteSession,
    session_id: String,
    prompt: String,
    reused: bool,
) -> CreateExecuteSessionResponse {
    CreateExecuteSessionResponse {
        prompt_hash: sess.prompt_hash.clone(),
        session: session_id,
        prompt,
        entry_id: sess.entry_id.clone(),
        entities: sess.entities.clone(),
        reused,
        principal: sess.principal.clone(),
    }
}
