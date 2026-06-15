//! Shared execute HTTP wire helpers (session JSON, path problems).

use axum::http::StatusCode;
use axum::response::Response;
use http_problem::prelude::{StatusCode as ProblemStatus, Uri};
use http_problem::Problem;
use plasm_core::{
    plasm_grammar_frontmatter_revision_hex, teaching_prompt_omit_contract_if_cached,
    PromptRenderMode,
};

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
    grammar_revision: Option<&str>,
) -> String {
    teaching_prompt_omit_contract_if_cached(
        stored_prompt,
        grammar_revision,
        Some(render_mode.markdown_fence_info_string()),
    )
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
        grammar_revision: plasm_grammar_frontmatter_revision_hex().to_string(),
        reused,
        principal: sess.principal.clone(),
    }
}
