//! HTTP routes for workflow view-model and instantiate.

use axum::extract::{Extension, Path};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_problem::prelude::{StatusCode as ProblemStatus, Uri};
use http_problem::Problem;

use serde::Serialize;

use crate::http_problem_util::{problem_response, problem_types};
use crate::server_state::PlasmHostState;
use crate::workflow_manifest::{InstantiateRequest, InstantiateResponse};
use crate::workflow_view_model::{
    build_workflow_view_model_with_readiness, instantiate_workflow_program,
};

pub fn workflow_routes() -> Router {
    Router::new()
        .route("/v1/workflows", get(list_workflows))
        .merge(crate::mcp_app::mount_bundle(&crate::mcp_app::WORKFLOW))
        .route(
            "/v1/workflows/{id}/view-model",
            get(get_workflow_view_model),
        )
        .route(
            "/v1/workflows/{id}/instantiate",
            post(post_workflow_instantiate),
        )
}

async fn list_workflows(Extension(st): Extension<PlasmHostState>) -> Response {
    let mut items: Vec<WorkflowListItem> = st
        .workflows()
        .list_ids()
        .into_iter()
        .filter_map(|id| {
            st.workflows().get(&id).map(|m| WorkflowListItem {
                id: m.id,
                title: m.title,
            })
        })
        .collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    Json(items).into_response()
}

#[derive(Serialize)]
struct WorkflowListItem {
    id: String,
    title: String,
}

async fn get_workflow_view_model(
    Extension(st): Extension<PlasmHostState>,
    Path(id): Path<String>,
) -> Response {
    let Some(manifest) = st.workflows().get(&id) else {
        return problem_response(
            Problem::custom(
                ProblemStatus::NOT_FOUND,
                Uri::from_static(problem_types::DISCOVERY_UNKNOWN_ENTRY),
            )
            .with_title("Not Found")
            .with_detail(format!("unknown workflow `{id}`")),
        );
    };
    let catalog = st.catalog.snapshot();
    let vm = build_workflow_view_model_with_readiness(&manifest, Some((catalog.as_ref(), None)));
    Json(vm).into_response()
}

async fn post_workflow_instantiate(
    Extension(st): Extension<PlasmHostState>,
    Extension(crate::incoming_auth::IncomingPrincipal(principal)): Extension<
        crate::incoming_auth::IncomingPrincipal,
    >,
    Path(id): Path<String>,
    Json(body): Json<InstantiateRequest>,
) -> Response {
    let Some(manifest) = st.workflows().get(&id) else {
        return problem_response(
            Problem::custom(
                ProblemStatus::NOT_FOUND,
                Uri::from_static(problem_types::DISCOVERY_UNKNOWN_ENTRY),
            )
            .with_title("Not Found")
            .with_detail(format!("unknown workflow `{id}`")),
        );
    };
    let template = match manifest.parsed_template() {
        Ok(t) => t,
        Err(e) => {
            return problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Bad Request")
                .with_detail(e.to_string()),
            );
        }
    };
    let scope = crate::incoming_auth::tenant_scope(principal.as_ref());
    let logical =
        match crate::mcp_logical_ref::parse_logical_session_wire_ref(&body.logical_session_ref) {
            Ok(logical) => logical,
            Err(error) => {
                return (axum::http::StatusCode::BAD_REQUEST, error.to_string()).into_response()
            }
        };
    if !st.logical_sessions.verify_tenant(logical, &scope).await {
        return (
            axum::http::StatusCode::NOT_FOUND,
            "unknown workflow session",
        )
            .into_response();
    }
    let Some((prompt_hash, session_id)) = st.logical_execute_bindings.get(&logical.as_uuid()).await
    else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            "workflow session has no execution binding",
        )
            .into_response();
    };
    let Some(session) = st.get_execute_session(&prompt_hash, &session_id).await else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            "workflow execution session expired",
        )
            .into_response();
    };
    if session.tenant_scope != scope {
        return (
            axum::http::StatusCode::NOT_FOUND,
            "unknown workflow session",
        )
            .into_response();
    }
    let symbols = crate::symbol_map_resolve::resolve_session_symbol_map(
        &crate::symbol_map_resolve::SessionSymbolMapContext {
            session: &session,
            cross_cache: None,
        },
    );
    match instantiate_workflow_program(&manifest, &template, &body.parameters, symbols.as_ref()) {
        Ok(program) => {
            if let Err(error) = crate::plasm_dag::compile_python_program_checked(&session, &program)
            {
                return (axum::http::StatusCode::BAD_REQUEST, error.to_string()).into_response();
            }
            Json(InstantiateResponse { program }).into_response()
        }
        Err(e) => problem_response(
            Problem::custom(
                ProblemStatus::BAD_REQUEST,
                Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
            )
            .with_title("Bad Request")
            .with_detail(e.to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use crate::workflow_registry::demo_workflow_manifests;
    use crate::workflow_view_model::build_workflow_view_model;

    #[test]
    fn demo_manifests_parse() {
        for m in demo_workflow_manifests() {
            let _ = build_workflow_view_model(&m);
            assert!(m.parsed_template().is_ok());
        }
    }
}
