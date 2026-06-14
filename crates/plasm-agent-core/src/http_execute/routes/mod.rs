//! Axum `/execute/*` routes and handlers.

mod handlers;
mod session;

#[cfg(test)]
mod tests;

use axum::routing::{get, post};
use axum::Router;

use handlers::{
    get_execute_run_artifact, get_execute_run_evidence, get_operation_progress_stream,
    handle_execute_session_get, post_create_execute_session, post_run_execute_session,
};
use session::{
    get_execute_session_runs, get_execute_session_status, get_execute_session_symbols,
    post_execute_session_context, post_execute_session_plan,
};

pub fn execute_routes() -> Router {
    Router::new()
        .route("/execute", post(post_create_execute_session))
        .route(
            "/execute/{prompt_hash}/{session_id}/operations/{operation_handle}/stream",
            get(get_operation_progress_stream),
        )
        .route(
            "/execute/{prompt_hash}/{session_id}/artifacts/{run_id}/evidence",
            get(get_execute_run_evidence),
        )
        .route(
            "/execute/{prompt_hash}/{session_id}/artifacts/{run_id}",
            get(get_execute_run_artifact),
        )
        .route(
            "/execute/{prompt_hash}/{session_id}/runs",
            get(get_execute_session_runs),
        )
        .route(
            "/execute/{prompt_hash}/{session_id}/symbols",
            get(get_execute_session_symbols),
        )
        .route(
            "/execute/{prompt_hash}/{session_id}/status",
            get(get_execute_session_status),
        )
        .route(
            "/execute/{prompt_hash}/{session_id}/context",
            post(post_execute_session_context),
        )
        .route(
            "/execute/{prompt_hash}/{session_id}/plan",
            post(post_execute_session_plan),
        )
        .route(
            "/execute/{prompt_hash}/{session_id}",
            get(handle_execute_session_get).post(post_run_execute_session),
        )
}
