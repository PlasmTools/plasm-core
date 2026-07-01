//! POST run execute session.

use super::super::super::*;

use super::plan_run_response::respond_plan_run_live_result;

pub(crate) async fn post_run_execute_session(
    Extension(st): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    ExecutePath {
        prompt_hash,
        session_id,
    }: ExecutePath,
    Query(run_q): Query<ExecuteRunQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(sess) = st
        .get_execute_session(prompt_hash.as_str(), session_id.as_str())
        .await
    else {
        let _miss = crate::spans::execute_session_lookup_miss().entered();
        tracing::debug!(
            prompt_hash = %prompt_hash,
            session_id = %session_id,
            "execute session lookup miss"
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

    let accept = headers.get(ACCEPT).and_then(|v| v.to_str().ok());
    let kind = match negotiate_accept(accept) {
        Ok(k) => k,
        Err(AcceptNegotiationError::NoSupportedMediaType) => {
            return problem_response(
                Problem::custom(
                    ProblemStatus::NOT_ACCEPTABLE,
                    Uri::from_static(problem_types::EXECUTE_UNSUPPORTED_ACCEPT),
                )
                .with_title("Not Acceptable")
                .with_detail(
                    "supported Accept values include application/json, application/x-ndjson, text/plain, text/toon (default when Accept is omitted: text/toon)",
                ),
            );
        }
    };

    let content_type = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok());

    let program = match parse_execute_program_body(content_type, &body) {
        Ok(v) => v,
        Err(msg) => {
            let type_uri = if msg.starts_with("invalid UTF-8:") {
                problem_types::EXECUTE_INVALID_BODY_ENCODING
            } else {
                problem_types::EXECUTE_INVALID_REQUEST_BODY
            };
            return problem_response(
                Problem::custom(ProblemStatus::BAD_REQUEST, Uri::from_static(type_uri))
                    .with_title("Bad Request")
                    .with_detail(msg),
            );
        }
    };

    if let Some(op_result) = try_dispatch_operation_program(
        &sess,
        Some(&st),
        Some(&http_operation_trace()),
        &program,
        Some(st.sessions.symbol_map_cross_cache()),
    )
    .await
    {
        return match op_result {
            Ok(result) => respond_plan_run_live_result(kind, &result, &sess),
            Err(e) => problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Bad Request")
                .with_detail(e),
            ),
        };
    }

    let plan_only = run_mode_is_plan(&headers, &run_q);
    let plan_name = "http_execute_program";
    let pipeline = st.engine.prompt_pipeline();
    let cross = st.sessions.symbol_map_cross_cache();
    let bundle = match crate::plasm_compile::compile_plasm_expression(
        pipeline,
        Some(cross),
        &sess,
        plan_name,
        &program,
    ) {
        Ok(b) => b,
        Err(e) => {
            return problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Bad Request")
                .with_detail(e),
            );
        }
    };

    if plan_only {
        let dry = match crate::plasm_plan_run::evaluate_plasm_comp_dry(&sess, &bundle) {
            Ok(d) => d,
            Err(e) => {
                return problem_response(
                    Problem::custom(
                        ProblemStatus::BAD_REQUEST,
                        Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                    )
                    .with_title("Bad Request")
                    .with_detail(e),
                );
            }
        };
        let comp_json = crate::plasm_comp_wire::trace_comp_wire_from_dry(&dry).to_json_value();
        let compact = crate::plan_dry_display::build_plan_dry_compact_view(
            dry.validated_plan(),
            &dry.topological_order,
            &dry.review,
            &dry.graph_summary,
            Some(&sess),
        );
        let commit_ref = sess.mint_plan_commit_ref();
        if let Err(e) = crate::plan_commit_store::register_plan_commit_and_persist(
            &st,
            Arc::clone(&sess),
            prompt_hash.as_str(),
            session_id.as_str(),
            crate::operation::PlanCommitRecord::from_dry_review(
                commit_ref.clone(),
                crate::operation::compute_plan_commit_id_from_dry(&dry),
                sess.domain_revision,
                &dry,
                program.clone(),
                compact.verdict,
                std::time::Instant::now() + crate::operation::PLAN_COMMIT_TTL,
            ),
        )
        .await
        {
            return problem_response(
                Problem::custom(
                    ProblemStatus::INTERNAL_SERVER_ERROR,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Plan commit persistence failed")
                .with_detail(e.to_string()),
            );
        }
        let mut plasm_meta =
            crate::operation::plan_commit_meta(&commit_ref, &dry.review, compact.verdict);
        plasm_meta.insert("dry_run".into(), serde_json::json!(true));
        plasm_meta.insert("comp".into(), comp_json.clone());
        let ux_ctx = crate::plan_ux_reflection::PlanUxBuildContext {
            session: Some(&sess),
            param_bindings: &[],
        };
        plasm_meta.insert(
            "plan_ux_reflection".into(),
            crate::plan_ux_reflection::plan_ux_reflection_value(&dry, &ux_ctx),
        );
        let preview = serde_json::json!({
            "plan": true,
            "comp": comp_json,
            "plan_ux_reflection": plasm_meta.get("plan_ux_reflection").cloned(),
            "node_results": dry.node_results,
            "graph_summary": dry.graph_summary,
            "source": program,
            "_meta": {
                "plasm": plasm_meta,
            },
        });
        crate::http_execute::trace::maybe_emit_http_code_plan_evaluate(
            &st,
            &sess,
            prompt_hash.as_str(),
            session_id.as_str(),
            &program,
            &bundle,
            1,
        )
        .await;
        return respond_plan_payload(kind, preview);
    }

    let wait_live = run_q.wait.unwrap_or(true);
    let force_run = run_q.force.unwrap_or(false);
    let plan_commit_ref = run_q
        .plan_commit_ref
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(plasm_core::PlanCommitRef::parse);
    let ph_str = prompt_hash.to_string();
    let sid_str = session_id.to_string();

    if let Err(e) = crate::evidence_chain::begin_plan_evidence_with_anchors(
        &sess,
        session_id.as_str(),
        crate::evidence_chain::evidence_anchors(plan_commit_ref.as_ref(), None, None),
    ) {
        return problem_response(
            Problem::custom(
                ProblemStatus::INTERNAL_SERVER_ERROR,
                Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
            )
            .with_title("Evidence error")
            .with_detail(e.to_string()),
        );
    }
    let dry_gate = match crate::plasm_plan_run::evaluate_plasm_comp_dry(&sess, &bundle) {
        Ok(d) => d,
        Err(e) => {
            return problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Bad Request")
                .with_detail(e),
            );
        }
    };
    let compact = crate::plan_dry_display::build_plan_dry_compact_view(
        dry_gate.validated_plan(),
        &dry_gate.topological_order,
        &dry_gate.review,
        &dry_gate.graph_summary,
        Some(&sess),
    );
    let accepted = match crate::plan_commit_store::accept_plan_commit_for_bundle(
        &sess,
        plan_commit_ref.as_ref(),
        &bundle,
        compact.verdict,
        &dry_gate.review,
    ) {
        Ok(a) => a,
        Err(e) => {
            return problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Bad Request")
                .with_detail(e.detail()),
            );
        }
    };
    if crate::operation::plan_requires_review_gate(
        accepted.verdict_for_gate,
        force_run,
        plan_commit_ref.as_ref(),
    ) {
        return problem_response(
            Problem::custom(
                ProblemStatus::BAD_REQUEST,
                Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
            )
            .with_title("Bad Request")
            .with_detail(
                "plan_requires_review: call plan dry-run first, then pass plan_commit_ref or force=true",
            ),
        );
    }
    match crate::run_delivery::deliver_http_live_run(crate::run_delivery::HttpLiveRunRequest {
        es: Arc::clone(&sess),
        st: Arc::new(st.clone()),
        prompt_hash: ph_str.clone(),
        session_id: sid_str.clone(),
        bundle: bundle.clone(),
        dry: dry_gate,
        wait_live,
        review: accepted.review_for_delivery,
        verdict_for_gate: accepted.verdict_for_gate,
        plan_commit_ref: plan_commit_ref.clone(),
    })
    .await
    {
        Ok(crate::run_delivery::HttpLiveRunOutcome::Accept { result, .. }) => {
            respond_plan_run_live_result(kind, &result, &sess)
        }
        Ok(crate::run_delivery::HttpLiveRunOutcome::Completed(result)) => {
            crate::http_execute::trace::maybe_emit_http_code_plan_execute(
                &st,
                &sess,
                ph_str.as_str(),
                sid_str.as_str(),
                &program,
                &bundle,
                1,
                &result,
            )
            .await;
            respond_plan_run_live_result(kind, &result, &sess)
        }
        Err(crate::run_delivery::LiveRunError::Timeout(d)) => problem_response(
            Problem::custom(
                ProblemStatus::GATEWAY_TIMEOUT,
                Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
            )
            .with_title("Gateway Timeout")
            .with_detail(format!("live run timed out after {d:?}")),
        ),
        Err(crate::run_delivery::LiveRunError::Failed(e)) => problem_response(
            Problem::custom(
                ProblemStatus::BAD_REQUEST,
                Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
            )
            .with_title("Bad Request")
            .with_detail(e),
        ),
    }
}
