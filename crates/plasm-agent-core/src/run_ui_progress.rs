//! Run Explorer HTTP progress for MCP App iframes (Cursor in-chat fallback).
//!
//! Access is scoped by bound `logical_session_ref` (128-bit wire token) — the same capability
//! model as MCP `plasm_run`. Handlers do not re-check execute principal; iframe clients rely on
//! ref secrecy plus same-origin cookies when incoming auth is enabled.

use axum::extract::{Extension, Path, Query};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use http_problem::prelude::{StatusCode as ProblemStatus, Uri};
use http_problem::Problem;
use std::sync::Arc;
use std::time::Duration;

use crate::http_problem_util::{problem_response, problem_types};
use crate::operation_progress_sse::{
    operation_progress_json_poll_sse, operation_progress_json_sse,
};
use crate::run_progress_resolve::{RunProgressError, RunningOpQuery};
use crate::server_state::PlasmHostState;

pub use crate::op_ui_telemetry::OpUiTelemetry as RunUiProgressJson;

#[derive(Debug, serde::Deserialize)]
pub struct RunUiProgressQuery {
    pub plan_commit_ref: Option<String>,
}

const POLL_INTERVAL: Duration = Duration::from_millis(900);

pub fn run_ui_progress_routes() -> Router {
    Router::new()
        .route(
            "/v1/run/ui/progress/{logical_session_ref}",
            get(get_run_ui_progress_json),
        )
        .route(
            "/v1/run/ui/progress/{logical_session_ref}/stream",
            get(get_run_ui_progress_stream),
        )
}

async fn get_run_ui_progress_json(
    Extension(st): Extension<PlasmHostState>,
    Path(logical_session_ref): Path<String>,
    Query(query): Query<RunUiProgressQuery>,
) -> Result<Json<RunUiProgressJson>, Response> {
    let resolved =
        resolve_for_http(&st, &logical_session_ref, query.plan_commit_ref.as_deref()).await?;
    Ok(Json(st.snapshot_for_running_op(&resolved)))
}

async fn get_run_ui_progress_stream(
    Extension(st): Extension<PlasmHostState>,
    Path(logical_session_ref): Path<String>,
    Query(query): Query<RunUiProgressQuery>,
) -> Result<Response, Response> {
    let resolved =
        resolve_for_http(&st, &logical_session_ref, query.plan_commit_ref.as_deref()).await?;
    let handle = resolved.handle.clone();
    let snapshot = st.snapshot_for_running_op(&resolved);
    let initial_seq = snapshot.n;
    let initial_line = snapshot.json_line();

    if let Some(sess) = resolved.live_session.clone() {
        if sess.operation_has_live_executor(&handle) {
            if let Some(rx) = sess.operation_progress_subscribe(&handle) {
                return Ok(operation_progress_json_sse(rx, initial_seq, initial_line));
            }
        }
    }

    let st_poll = st.clone();
    let logical_ref = logical_session_ref.clone();
    let plan_commit = query.plan_commit_ref.clone();
    Ok(operation_progress_json_poll_sse(
        POLL_INTERVAL,
        st_poll,
        Arc::new(move |host| {
            let logical_ref = logical_ref.clone();
            let plan_commit = plan_commit.clone();
            Box::pin(async move {
                let resolved = host
                    .resolve_running_operation(RunningOpQuery {
                        logical_session_ref: logical_ref,
                        plan_commit_ref: plan_commit,
                    })
                    .await
                    .ok()?;
                Some(host.snapshot_for_running_op(&resolved))
            })
        }),
    ))
}

async fn resolve_for_http(
    st: &PlasmHostState,
    logical_session_ref: &str,
    plan_commit_ref: Option<&str>,
) -> Result<crate::run_progress_resolve::ResolvedRunningOp, Response> {
    st.resolve_running_operation(RunningOpQuery {
        logical_session_ref: logical_session_ref.to_string(),
        plan_commit_ref: plan_commit_ref.map(str::to_string),
    })
    .await
    .map_err(run_progress_error_to_response)
}

fn run_progress_error_to_response(err: RunProgressError) -> Response {
    match err {
        RunProgressError::BadLogicalRef(detail) | RunProgressError::BadHandle(detail) => {
            problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Bad Request")
                .with_detail(detail),
            )
        }
        RunProgressError::BindingNotFound => {
            not_found("logical session binding not found or expired")
        }
        RunProgressError::SessionNotFound => not_found("execute session not found or expired"),
        RunProgressError::NoRunningOperation => {
            not_found("no running operation for logical session")
        }
    }
}

fn not_found(detail: impl Into<String>) -> Response {
    problem_response(
        Problem::custom(
            ProblemStatus::NOT_FOUND,
            Uri::from_static(problem_types::EXECUTE_UNKNOWN_SESSION),
        )
        .with_title("Not Found")
        .with_detail(detail.into()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_progress_resolve::RunningOpQuery;

    #[test]
    fn run_ui_progress_json_alias_matches_op_ui_telemetry() {
        let _snap: RunUiProgressJson = RunUiProgressJson::default();
    }

    #[tokio::test]
    async fn http_resolve_delegates_to_host() {
        use crate::execute_path_ids::{ExecuteSessionId, PromptHashHex};
        use crate::execute_session::{ExecuteSession, SessionReuseKey};
        use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
        use crate::mcp_logical_ref::parse_logical_session_wire_ref;
        use plasm_core::discovery::InMemoryCgsRegistry;
        use plasm_core::CGS;
        use plasm_runtime::{ExecutionEngine, ExecutionMode};

        let cgs = Arc::new(CGS::new());
        let st = Arc::new(build_plasm_host_state(PlasmHostBootstrap {
            engine: ExecutionEngine::new(Default::default()).expect("engine"),
            mode: ExecutionMode::Live,
            registry: Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
                "default".into(),
                "Default".into(),
                vec!["default".into()],
                cgs.clone(),
            )])),
            catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
            plugin_manager: None,
            incoming_auth: None,
            run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        }));

        let ref_str = "l_AAAAAAAAQACAAAAAAAAAAQ";
        let logical_id = parse_logical_session_wire_ref(ref_str).expect("logical ref");
        let ph = PromptHashHex::from_prompt_sha256("run-ui-progress-bind-test").to_string();
        let sid = ExecuteSessionId::new_random().to_string();
        let sess = ExecuteSession::new(
            ph.clone(),
            "p".into(),
            cgs.clone(),
            indexmap::IndexMap::new(),
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let handle = sess.mint_operation_handle(ref_str);
        sess.try_begin_async_operation(
            handle.clone(),
            plasm_runtime::CancelSignal::new(),
            crate::operation::OpAcceptContext::default(),
        )
        .expect("begin op");

        let reuse_key = SessionReuseKey {
            tenant_scope: String::new(),
            entry_id: "default".into(),
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
            entities: vec!["Pet".into()],
            context_intent: None,
            ranked_capabilities: None,
            principal: None,
            plugin_generation_id: None,
            logical_session_id: Some(logical_id.as_uuid().to_string()),
        };
        st.sessions
            .insert(reuse_key, ph.clone(), sid.clone(), sess)
            .await;
        st.logical_execute_bindings
            .insert(logical_id.as_uuid(), ph.clone(), sid.clone())
            .await;

        let resolved = resolve_for_http(&st, ref_str, None).await.expect("resolve");
        let snap = st.snapshot_for_running_op(&resolved);
        assert!(!snap.line.is_empty());
        assert!(!snap.terminal);
        assert_eq!(resolved.handle.as_str(), handle.as_str());

        let via_host = st
            .resolve_running_operation(RunningOpQuery {
                logical_session_ref: ref_str.to_string(),
                plan_commit_ref: None,
            })
            .await
            .expect("host resolve");
        assert_eq!(via_host.handle.as_str(), handle.as_str());
    }
}
