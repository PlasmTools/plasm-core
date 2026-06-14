//! Primary `/execute` Axum handlers.

use super::super::*;

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
    let first = stream::once(async move {
        Ok::<Event, Infallible>(Event::default().event("snapshot").data(line))
    });
    let body: std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> =
        Box::pin(first.chain(stream::unfold(
            (rx, seq),
            |(mut rx, mut last_seq)| async move {
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            if ev.seq <= last_seq {
                                continue;
                            }
                            last_seq = ev.seq;
                            let event_name = if ev.terminal { "terminal" } else { "progress" };
                            return Some((
                                Ok(Event::default().event(event_name).data(ev.line)),
                                (rx, last_seq),
                            ));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                    }
                }
            },
        )));
    Ok(Sse::new(body)
        .keep_alive(KeepAlive::default())
        .into_response())
}

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

pub(crate) async fn handle_execute_session_get(
    Extension(st): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    ExecutePath {
        prompt_hash,
        session_id,
    }: ExecutePath,
    Query(query): Query<ExecuteSessionGetQuery>,
    headers: HeaderMap,
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

    let grammar_revision = grammar_revision_from_wire(
        query.grammar_revision.as_deref(),
        headers
            .get("x-plasm-grammar-revision")
            .and_then(|v| v.to_str().ok()),
    );
    let render_mode = st.engine.prompt_pipeline().render_mode;
    Json(create_execute_session_response(
        &sess,
        session_id.to_string(),
        wire_execute_session_prompt(&sess.prompt_text, render_mode, grammar_revision),
        false,
    ))
    .into_response()
}

pub(crate) async fn get_execute_run_evidence(
    Extension(st): Extension<PlasmHostState>,
    Path((ph, sid, rid)): Path<(String, String, String)>,
) -> Response {
    let prompt_hash = match ph.parse::<PromptHashHex>() {
        Ok(v) => v,
        Err(msg) => {
            return problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `prompt_hash` path segment: {msg}"),
            );
        }
    };
    let session_id = match sid.parse::<ExecuteSessionId>() {
        Ok(v) => v,
        Err(msg) => {
            return problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `session_id` path segment: {msg}"),
            );
        }
    };
    let run_id = match rid.trim().parse::<RunArtifactWire>() {
        Ok(w) => w.0,
        Err(e) => {
            return problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `run_id` path segment: {e}"),
            );
        }
    };
    match st
        .run_artifacts
        .get_evidence_bundle(prompt_hash.as_str(), session_id.as_str(), run_id)
        .await
    {
        Ok(Some(bundle)) => {
            let opts = plasm_evidence::VerifyOptions {
                trusted_public_keys: crate::evidence_chain::trusted_public_keys_from_env(),
            };
            let run_id_wire = run_id.to_wire();
            let artifact_bytes = st
                .run_artifacts
                .get(prompt_hash.as_str(), session_id.as_str(), run_id)
                .await;
            let (artifact_doc, parsed_for_seal) = if let Some(bytes) = artifact_bytes {
                match serde_json::from_slice::<plasm_evidence::RunArtifactForSeal>(&bytes) {
                    Ok(artifact_doc) => {
                        let parsed = artifact_doc.parsed_preimage.clone();
                        (Some(artifact_doc), Some(parsed))
                    }
                    Err(_) => (None, None),
                }
            } else {
                (None, None)
            };
            let artifact_ref = artifact_doc.as_ref();
            let parsed_ref = parsed_for_seal.as_ref();
            if let Err(e) = crate::evidence_chain::verify_evidence_for_http_serve(
                &bundle,
                &opts,
                run_id_wire.as_str(),
                artifact_ref,
                parsed_ref,
            ) {
                return problem_response(
                    Problem::custom(
                        ProblemStatus::UNPROCESSABLE_ENTITY,
                        Uri::from_static(problem_types::EXECUTE_UNKNOWN_ARTIFACT),
                    )
                    .with_title("Evidence verification failed")
                    .with_detail(e.to_string()),
                );
            }
            Json(bundle).into_response()
        }
        Ok(None) => problem_response(
            Problem::custom(
                ProblemStatus::NOT_FOUND,
                Uri::from_static(problem_types::EXECUTE_UNKNOWN_ARTIFACT),
            )
            .with_title("Not Found")
            .with_detail("evidence bundle not found for this run_id"),
        ),
        Err(e) => problem_response(
            Problem::custom(
                ProblemStatus::INTERNAL_SERVER_ERROR,
                Uri::from_static(problem_types::EXECUTE_UNKNOWN_ARTIFACT),
            )
            .with_title("Evidence decode failed")
            .with_detail(e.to_string()),
        ),
    }
}

pub(crate) async fn get_execute_run_artifact(
    Extension(st): Extension<PlasmHostState>,
    Path((ph, sid, rid)): Path<(String, String, String)>,
) -> Response {
    let started = Instant::now();
    let prompt_hash = match ph.parse::<PromptHashHex>() {
        Ok(v) => v,
        Err(msg) => {
            crate::metrics::record_execute_artifact_serve("error", "bad_path", started.elapsed());
            return problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `prompt_hash` path segment: {msg}"),
            );
        }
    };
    let session_id = match sid.parse::<ExecuteSessionId>() {
        Ok(v) => v,
        Err(msg) => {
            crate::metrics::record_execute_artifact_serve("error", "bad_path", started.elapsed());
            return problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `session_id` path segment: {msg}"),
            );
        }
    };
    let run_id = match rid.trim().parse::<RunArtifactWire>() {
        Ok(w) => w.0,
        Err(e) => {
            crate::metrics::record_execute_artifact_serve("error", "bad_path", started.elapsed());
            return problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `run_id` path segment: {e}"),
            );
        }
    };

    let live_sess = st
        .get_execute_session(prompt_hash.as_str(), session_id.as_str())
        .await;
    let live_payload = if let Some(sess) = &live_sess {
        sess.core
            .get_run_artifact(run_id)
            .await
            .map(|a| a.payload.clone())
    } else {
        None
    };
    if live_payload.is_some() {
        crate::metrics::record_execute_artifact_resolve_layer("hot");
    }
    let persisted_payload = if live_payload.is_none() {
        match st
            .run_artifacts
            .get_payload_result(prompt_hash.as_str(), session_id.as_str(), run_id)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                crate::metrics::record_execute_artifact_serve(
                    "error",
                    "decode_failed",
                    started.elapsed(),
                );
                return problem_response(
                    Problem::custom(
                        ProblemStatus::INTERNAL_SERVER_ERROR,
                        Uri::from_static(problem_types::EXECUTE_SERIALIZATION_FAILED),
                    )
                    .with_title("Internal Server Error")
                    .with_detail(format!("run artifact decode failed: {e}")),
                );
            }
        }
    } else {
        None
    };
    if live_payload.is_none() && persisted_payload.is_some() {
        crate::metrics::record_execute_artifact_resolve_layer("archive");
    }
    let Some(payload) = live_payload.or(persisted_payload) else {
        crate::metrics::record_execute_artifact_serve("error", "not_found", started.elapsed());
        return problem_response(
            Problem::custom(
                ProblemStatus::NOT_FOUND,
                Uri::from_static(problem_types::EXECUTE_UNKNOWN_ARTIFACT),
            )
            .with_title("Not Found")
            .with_detail(
                "unknown run artifact for this session (wrong id, expired, or never stored)",
            ),
        );
    };

    let artifact_span = crate::spans::execute_artifact_serve();
    artifact_span.in_scope(|| {
        tracing::info!(
            target: "plasm_agent::http_execute",
            prompt_hash = %prompt_hash.as_str(),
            session_id = %session_id.as_str(),
            run_id = %run_id.to_wire(),
            bytes = payload.bytes.len(),
            "GET execute run artifact"
        );
    });

    let content_type = payload.metadata.content_type;
    let header = axum::http::HeaderValue::from_str(content_type.as_str())
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static("application/octet-stream"));
    crate::metrics::record_execute_artifact_serve("success", "none", started.elapsed());
    (StatusCode::OK, [(CONTENT_TYPE, header)], payload.bytes).into_response()
}

pub(crate) fn respond_plan_run_live_result(
    kind: ExecResponseKind,
    result: &crate::plasm_plan_run::PlasmPlanRunResult,
    sess: &ExecuteSession,
) -> Response {
    let steps = &result.return_steps;
    if steps.is_empty() {
        if result.run_markdown.is_some() || result.run_plasm_meta.is_some() {
            let payload = serde_json::json!({
                "operation": true,
                "run_markdown": result.run_markdown,
                "_meta": result
                    .run_plasm_meta
                    .as_ref()
                    .map(|m| serde_json::Value::Object(m.clone())),
                "comp": result.comp,
            });
            if let ExecResponseKind::Table = kind {
                let md = result.run_markdown.as_deref().unwrap_or("");
                return (
                    StatusCode::OK,
                    [(CONTENT_TYPE, "text/plain; charset=utf-8")],
                    md.to_string(),
                )
                    .into_response();
            }
            if matches!(kind, ExecResponseKind::Toon | ExecResponseKind::Ndjson) {
                return respond_plan_payload(kind, payload);
            }
            return (
                StatusCode::OK,
                [(CONTENT_TYPE, "application/json; charset=utf-8")],
                Json(payload),
            )
                .into_response();
        }
        return problem_response(
            Problem::custom(
                ProblemStatus::INTERNAL_SERVER_ERROR,
                Uri::from_static(problem_types::EXECUTE_SERIALIZATION_FAILED),
            )
            .with_title("Internal Server Error")
            .with_detail("plan run returned no results"),
        );
    }
    if steps.len() == 1 {
        let step = &steps[0];
        let json_value = http_execute_results_value(&step.result);
        let cgs = step.cgs.as_deref().or(Some(sess.cgs.as_ref()));
        let omitted = reference_only_omitted_field_names(&step.result, cgs);
        let handles: Vec<RunArtifactHandle> = step.artifact.clone().into_iter().collect();
        let response_meta = tool_meta_from_handles(&handles, &omitted);
        return respond_execute_result(
            kind,
            json_value,
            &step.result,
            response_meta,
            cgs,
            step.artifact.as_ref(),
        );
    }
    let mut step_values = Vec::with_capacity(steps.len());
    let mut step_tables = if kind == ExecResponseKind::Table {
        Some(Vec::with_capacity(steps.len()))
    } else {
        None
    };
    let mut step_artifacts = Vec::new();
    let mut omitted_union: BTreeSet<String> = BTreeSet::new();
    for step in steps {
        if let Some(h) = &step.artifact {
            step_artifacts.push(h.clone());
        }
        let cgs = step.cgs.as_deref().or(Some(sess.cgs.as_ref()));
        step_values.push(http_execute_results_value(&step.result));
        if let Some(ref mut tabs) = step_tables {
            let (table, omitted, _) =
                format_result_with_cgs(&step.result, OutputFormat::Table, cgs);
            omitted_union.extend(omitted);
            tabs.push(table);
        } else {
            omitted_union.extend(reference_only_omitted_field_names(&step.result, cgs));
        }
    }
    let omitted_vec: Vec<String> = omitted_union.into_iter().collect();
    let steps_response_meta = tool_meta_from_handles(&step_artifacts, &omitted_vec);
    respond_staged_lines_execute_result(
        kind,
        step_values,
        step_tables,
        steps_response_meta,
        step_artifacts.last(),
    )
}

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

    if let Some(op_result) =
        try_dispatch_operation_program(&sess, Some(&http_operation_trace()), &program).await
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
        if let Err(e) = crate::evidence_chain::begin_plan_evidence(&sess, session_id.as_str()) {
            return problem_response(
                Problem::custom(
                    ProblemStatus::INTERNAL_SERVER_ERROR,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Evidence error")
                .with_detail(e.to_string()),
            );
        }
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
        let comp_json = crate::plasm_comp_wire::plasm_comp_json_from_dry(&dry);
        let compact = crate::plan_dry_display::build_plan_dry_compact_view(
            dry.validated_plan(),
            &dry.topological_order,
            &dry.review,
            &dry.graph_summary,
            Some(&sess),
        );
        let commit_ref = sess.mint_plan_commit_ref();
        sess.register_plan_commit(crate::operation::PlanCommitRecord {
            commit_ref: commit_ref.clone(),
            commit_id: crate::operation::compute_plan_commit_id_from_dry(&dry),
            dry_review: dry.review.clone(),
            verdict: compact.verdict,
            expires_at: std::time::Instant::now() + crate::operation::PLAN_COMMIT_TTL,
        });
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
    if crate::operation::plan_requires_review_gate(
        compact.verdict,
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
    if let Some(pc) = plan_commit_ref.as_ref() {
        if let Err(e) = crate::operation::verify_plan_commit_for_dry(&sess, pc, &dry_gate) {
            return problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Bad Request")
                .with_detail(e),
            );
        }
    }

    if crate::operation::should_spawn_async_live_run(wait_live, &dry_gate.review) {
        let auto_async = crate::operation::live_run_should_auto_async(&dry_gate.review, wait_live);
        if let Err(e) = sess.try_begin_live_program_run() {
            return problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Bad Request")
                .with_detail(e),
            );
        }
        let handle = sess.mint_operation_handle_plain();
        let payload =
            crate::run_explorer_meta::build_run_explorer_accept_payload(&dry_gate, Some(&sess));
        let mut accept = crate::operation::op_accept_context_from_executable(
            plan_commit_ref.clone(),
            Some(compact.verdict),
            auto_async,
            None,
            bundle.executable(),
            &bundle.artifact().comp,
        );
        accept.comp = Some(payload.comp.clone());
        accept.plan_ux_reflection = Some(payload.plan_ux_reflection.clone());
        accept.step_order = payload.step_order.clone();
        if let Err(e) = crate::operation::spawn_async_plan_run(
            Arc::clone(&sess),
            Arc::new(st.clone()),
            ph_str.clone(),
            sid_str.clone(),
            bundle.clone(),
            handle.clone(),
            plasm_runtime::CancelSignal::new(),
            accept,
        ) {
            return problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Bad Request")
                .with_detail(e),
            );
        }
        let (markdown, mut meta) = crate::operation::async_live_run_accept_parts(
            &handle,
            plan_commit_ref.as_ref(),
            compact.verdict,
            auto_async,
        );
        crate::run_explorer_meta::merge_accept_payload_into_meta(&mut meta, "oN", &payload);
        return respond_plan_run_live_result(
            kind,
            &crate::plasm_plan_run::PlasmPlanRunResult {
                version: serde_json::json!({}),
                node_results: Vec::new(),
                graph_summary: serde_json::json!({}),
                comp: payload.comp,
                code_plan_run_artifacts: Vec::new(),
                run_markdown: Some(markdown),
                run_plasm_meta: Some(meta),
                return_steps: Vec::new(),
            },
            &sess,
        );
    }

    if let Err(e) = sess.begin_sync_live_run() {
        return problem_response(
            Problem::custom(
                ProblemStatus::BAD_REQUEST,
                Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
            )
            .with_title("Bad Request")
            .with_detail(e),
        );
    }
    let sync_result = crate::execute_pipeline::ExecutePipeline::run_program(
        &sess,
        &st,
        ph_str.as_str(),
        sid_str.as_str(),
        &bundle,
        crate::execute_pipeline::ExecutionIntent::Live,
        None,
    )
    .await;
    sess.end_sync_live_run();
    match sync_result {
        Ok(result) => respond_plan_run_live_result(kind, &result, &sess),
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
