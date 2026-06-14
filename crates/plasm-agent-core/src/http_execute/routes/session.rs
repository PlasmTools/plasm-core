//! Session sub-resource Axum handlers (`/context`, `/symbols`, `/status`, `/runs`, `/plan`).

use super::super::response::{
    negotiate_accept, respond_plan_payload, AcceptNegotiationError, ExecResponseKind,
};
use super::super::*;

pub(crate) async fn post_execute_session_context(
    Extension(st): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    ExecutePath {
        prompt_hash,
        session_id,
    }: ExecutePath,
    Json(body): Json<ExecuteSessionContextBody>,
) -> Response {
    let Some(sess) = st
        .get_execute_session(prompt_hash.as_str(), session_id.as_str())
        .await
    else {
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
    let principal_stored = sess.principal.clone();
    let intent_owned = body.intent.unwrap_or_default();
    let intent_ref = intent_owned.trim();
    match apply_capability_seeds(
        &st,
        principal.as_ref(),
        Some((prompt_hash.as_str(), session_id.as_str())),
        body.seeds,
        principal_stored,
        None,
        None,
        intent_ref,
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    {
        Ok(out) => Json(out).into_response(),
        Err(msg) => problem_response(
            Problem::custom(
                ProblemStatus::BAD_REQUEST,
                Uri::from_static(problem_types::EXECUTE_REGISTRY_ERROR),
            )
            .with_title("Bad Request")
            .with_detail(msg),
        ),
    }
}

pub(crate) async fn get_execute_session_symbols(
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
    let loaded_catalogs: Vec<String> = sess.contexts_by_entry.keys().cloned().collect();
    let entity_symbols = sess
        .teaching_exposure
        .as_ref()
        .map(|ex| ex.symbol_map_arc().exposed_entity_symbol_rows())
        .unwrap_or_default();
    Json(ExecuteSessionSymbolsResponse {
        prompt_hash: sess.prompt_hash.clone(),
        session_id: session_id.to_string(),
        domain_revision: sess.domain_revision,
        entry_id: sess.entry_id.clone(),
        entities: sess.entities.clone(),
        loaded_catalogs,
        context_intent: sess.context_intent.clone(),
        entity_symbols,
    })
    .into_response()
}

pub(crate) async fn get_execute_session_status(
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
        return Json(ExecuteSessionStatusResponse {
            alive: false,
            prompt_hash: prompt_hash.to_string(),
            session_id: session_id.to_string(),
            domain_revision: 0,
            entry_id: String::new(),
            entities: vec![],
            loaded_catalogs: vec![],
            context_intent: None,
            principal: None,
        })
        .into_response();
    };
    if !session_allows_principal(&sess, principal.as_ref()) {
        return incoming_auth_problem(
            crate::incoming_auth::IncomingAuthFailure::Invalid(
                "execute session tenant does not match caller".into(),
            ),
            true,
        );
    }
    let loaded_catalogs: Vec<String> = sess.contexts_by_entry.keys().cloned().collect();
    Json(ExecuteSessionStatusResponse {
        alive: true,
        prompt_hash: sess.prompt_hash.clone(),
        session_id: session_id.to_string(),
        domain_revision: sess.domain_revision,
        entry_id: sess.entry_id.clone(),
        entities: sess.entities.clone(),
        loaded_catalogs,
        context_intent: sess.context_intent.clone(),
        principal: sess.principal.clone(),
    })
    .into_response()
}

pub(crate) async fn get_execute_session_runs(
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
    let runs = sess.core.list_run_summaries().await;
    Json(ExecuteSessionRunsResponse {
        prompt_hash: sess.prompt_hash.clone(),
        session_id: session_id.to_string(),
        runs,
    })
    .into_response()
}

pub(crate) async fn post_execute_session_plan(
    Extension(st): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    ExecutePath {
        prompt_hash,
        session_id,
    }: ExecutePath,
    headers: HeaderMap,
    Json(body): Json<crate::resolved_plan_http::ResolvedPlanRequest>,
) -> Response {
    let Some(sess) = st
        .get_execute_session(prompt_hash.as_str(), session_id.as_str())
        .await
    else {
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

    let prepared = match crate::resolved_plan_http::prepare_resolved_plan_request(body, &sess) {
        Ok(p) => p,
        Err(e) => {
            return problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_INVALID_REQUEST_BODY),
                )
                .with_title("Bad Request")
                .with_detail(e.to_string()),
            );
        }
    };
    let run_live = matches!(
        prepared.mode,
        crate::resolved_plan_http::ResolvedPlanRunMode::Run
    );
    let ph_str = prompt_hash.to_string();
    let sid_str = session_id.to_string();
    let outcome = crate::execute_pipeline::ExecutePipeline::run_program(
        &sess,
        &st,
        ph_str.as_str(),
        sid_str.as_str(),
        &prepared.bundle,
        if run_live {
            crate::execute_pipeline::ExecutionIntent::Live
        } else {
            crate::execute_pipeline::ExecutionIntent::PlanOnly
        },
        None,
    )
    .await;

    match outcome {
        Ok(result) => {
            let accept = headers.get(ACCEPT).and_then(|v| v.to_str().ok());
            let kind = match negotiate_accept(accept) {
                Ok(k) => k,
                Err(AcceptNegotiationError::NoSupportedMediaType) => ExecResponseKind::Json,
            };
            let payload = crate::resolved_plan_http::ResolvedPlanResponse {
                plan: true,
                dry_run: !run_live,
                comp: result.comp,
                node_results: Some(result.node_results),
                graph_summary: Some(result.graph_summary),
                run_markdown: result.run_markdown,
                meta: result.run_plasm_meta.map(serde_json::Value::Object),
            };
            if run_live {
                if let ExecResponseKind::Toon | ExecResponseKind::Ndjson = kind {
                    return respond_plan_payload(
                        kind,
                        serde_json::to_value(&payload).unwrap_or_default(),
                    );
                }
                if let Some(md) = payload.run_markdown.as_deref() {
                    if matches!(kind, ExecResponseKind::Table) {
                        return (
                            StatusCode::OK,
                            [(CONTENT_TYPE, "text/plain; charset=utf-8")],
                            md.to_string(),
                        )
                            .into_response();
                    }
                }
            }
            (
                StatusCode::OK,
                [(CONTENT_TYPE, "application/json; charset=utf-8")],
                Json(payload),
            )
                .into_response()
        }
        Err(e) => problem_response(
            Problem::custom(
                ProblemStatus::BAD_REQUEST,
                Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
            )
            .with_title("Bad Request")
            .with_detail(e),
        ),
    }
}
