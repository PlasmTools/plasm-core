//! Parsed Plasm line execute: branch cycle, artifacts, [`RunLineError`].

use crate::execute_pipeline::RunLineError;
use crate::run_artifacts::{persist_execute_run, PersistExecuteRunError, PersistExecuteRunInput};

use super::ingress::execute_session_parse_error_message;
use super::proof_bind::{
    maybe_proof_refresh_session_base_token, try_proof_document_share_bind, ProofBindError,
};
use super::trace::{emit_plasm_line_trace, trace_expr_api_meta, PlasmLineTraceSink};
use super::{resolve_paging_storage_handle, trace_api_entry_id_for_execute_root, *};

impl From<PersistExecuteRunError> for RunLineError {
    fn from(e: PersistExecuteRunError) -> Self {
        match e {
            PersistExecuteRunError::Mint(d) => RunLineError::Parse(d),
            PersistExecuteRunError::Serialization(e) => RunLineError::ArtifactSerialization(e),
            PersistExecuteRunError::Persist(d) => RunLineError::ArtifactPersist(d),
        }
    }
}

impl From<ProofBindError> for RunLineError {
    fn from(e: ProofBindError) -> Self {
        RunLineError::Parse(e.to_string())
    }
}

impl From<crate::graph_execute::GraphBranchRunError> for RunLineError {
    fn from(e: crate::graph_execute::GraphBranchRunError) -> Self {
        match e {
            crate::graph_execute::GraphBranchRunError::WriteConflict { details, attempts } => {
                RunLineError::GraphWriteConflict { details, attempts }
            }
        }
    }
}

fn run_line_error_metric_labels(err: &RunLineError) -> (&'static str, &'static str) {
    match err {
        RunLineError::Parse(_) => ("parse", "parse"),
        RunLineError::Normalize(_) => ("parse", "normalize"),
        RunLineError::Projection(_) => ("projection", "projection"),
        RunLineError::Runtime(_, _) => ("execute", "runtime"),
        RunLineError::ArtifactSerialization(_) => ("artifact", "serialization"),
        RunLineError::ArtifactPersist(_) => ("artifact", "persist"),
        RunLineError::GraphWriteConflict { .. } => ("execute", "graph_write_conflict"),
        RunLineError::Operation(_) => ("operation", "continuation"),
    }
}

fn record_run_line_error_metrics(
    entry_id: &str,
    operation: &str,
    err: &RunLineError,
    wall: Instant,
) {
    let ms = wall.elapsed().as_secs_f64() * 1000.0;
    let (status, phase) = run_line_error_metric_labels(err);
    crate::metrics::record_execute_expression_line(entry_id, operation, status, phase, ms, 0, 0);
}

async fn persist_session_credentials_after_mutation(
    st: &PlasmHostState,
    sess: &ExecuteSession,
    session_id: &str,
) {
    use crate::mcp_transport_store::execute_session_registry::ExecuteSessionPersistOutcome;

    match st.persist_session_bind_credentials(sess, session_id).await {
        Ok(ExecuteSessionPersistOutcome::Durable) => {}
        Ok(ExecuteSessionPersistOutcome::InMemoryOnly) => {}
        Err(e) => tracing::warn!(
            target: "plasm_agent::session_credentials",
            error = %e,
            "failed to persist session bind credentials to durable execute session"
        ),
    }
}

/// Parse one Plasm line for the active session (HTTP/MCP ingress).
pub(crate) fn parse_plasm_line_for_session(
    line: &str,
    sess: &ExecuteSession,
    st: &PlasmHostState,
) -> Result<ParsedExpr, RunLineError> {
    let pipeline = st.engine.prompt_pipeline();
    let mut parsed = crate::plasm_plan_run::parse_plasm_surface_line(
        sess,
        Some(st.sessions.symbol_map_cross_cache()),
        pipeline,
        line,
    )
    .map_err(|e| {
        let sym_map = crate::plasm_plan_run::symbol_map_for_plasm_surface_parse(
            sess,
            Some(st.sessions.symbol_map_cross_cache()),
        );
        RunLineError::Parse(execute_session_parse_error_message(
            &e,
            line,
            sess.cgs.as_ref(),
            sym_map.as_ref(),
        ))
    })?;
    if let Some(ref fed) = sess.federation_dispatch() {
        normalize_expr_query_capabilities_federated(
            &mut parsed.expr,
            fed.as_ref(),
            sess.cgs.as_ref(),
        )
    } else {
        normalize_expr_query_capabilities(&mut parsed.expr, sess.cgs.as_ref())
    }
    .map_err(|e| RunLineError::Normalize(e.to_string()))?;
    Ok(parsed)
}

fn synthetic_page_result(
    sess: &ExecuteSession,
    handle: &PagingHandle,
    mut cursor: crate::execute_session::SyntheticPageCursor,
    trace: Option<&PlasmTraceContext>,
) -> ExecutionResult {
    let start = cursor.offset.min(cursor.rows.len());
    let end = start
        .saturating_add(cursor.page_size)
        .min(cursor.rows.len());
    let entities = cursor.rows[start..end].to_vec();
    cursor.offset = end;
    let has_more = cursor.offset < cursor.rows.len();
    let request_fingerprints = cursor.request_fingerprints.clone();
    let paging_handle = if has_more {
        sess.upsert_synthetic_paging_resume(handle, cursor);
        Some(handle.clone())
    } else {
        sess.remove_paging_resume(handle);
        None
    };
    let _ = trace;
    ExecutionResult {
        count: entities.len(),
        entities,
        has_more,
        pagination_resume: None,
        paging_handle,
        source: ExecutionSource::Cache,
        stats: ExecutionStats {
            duration_ms: 0,
            network_requests: 0,
            cache_hits: 0,
            cache_misses: 0,
            ..Default::default()
        },
        request_fingerprints,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_parsed_plasm_line(
    line: &str,
    sess: &ExecuteSession,
    st: &PlasmHostState,
    session_id: &str,
    parsed: ParsedExpr,
    trace: Option<&PlasmTraceContext>,
    line_index: i64,
    host_page_size: Option<usize>,
    surface_read_budget: Option<crate::plan_read_bounds::PushedReadBudget>,
    rows_progress: Option<plasm_runtime::RowsProgressFn>,
    preflight: Option<plasm_core::PreflightToken>,
    plan_shared: Option<&crate::plan_execute_shared::PlanLineExecuteShared>,
) -> Result<(ParsedExpr, ExecutionResult, Option<RunArtifactHandle>), RunLineError> {
    let preflight_token = match preflight {
        Some(token) => token,
        None => {
            crate::execute_pipeline::PlasmPreflight::preflight_parsed_line(sess, line, &parsed)
                .map_err(RunLineError::Parse)?;
            plasm_core::PreflightToken::VERIFIED
        }
    };
    match &parsed.expr {
        Expr::Wait(w) => {
            let out = handle_wait_operation(sess, Some(st), trace, &w.handle)
                .await
                .map_err(|e| {
                    RunLineError::Parse(crate::http_execute::operation_error_to_string(e))
                })?;
            return Err(RunLineError::Operation(Box::new(out)));
        }
        Expr::Cancel(c) => {
            let out = handle_cancel_operation(sess, trace, &c.handle)
                .await
                .map_err(|e| {
                    RunLineError::Parse(crate::http_execute::operation_error_to_string(e))
                })?;
            return Err(RunLineError::Operation(Box::new(out)));
        }
        _ => {}
    }

    let wall = Instant::now();
    let mut log_expr = format!("→ {}", crate::expr_display::expr_display(&parsed.expr));
    if let Some(ref proj) = parsed.projection {
        if !proj.is_empty() {
            log_expr.push_str(&format!("\n  projection: [{}]", proj.join(", ")));
        }
    }
    let expr_span =
        crate::spans::execute_expression_line(sess.entry_id.as_str(), line.len(), log_expr.len());
    expr_span.in_scope(|| {
        tracing::trace!(
            target: "plasm_agent::http_execute",
            entry_id = %sess.entry_id,
            source_expression = %line,
            parsed_expression = %log_expr,
            "execute expression"
        );
    });
    let page_storage_key: Option<PagingHandle> = match &parsed.expr {
        plasm_core::Expr::Page(p) => Some(resolve_paging_storage_handle(trace, &p.handle)?),
        _ => None,
    };
    let _page_paging_serial = if page_storage_key.is_some() {
        Some(sess.paging_op_lock.lock().await)
    } else {
        None
    };
    if let Some(ref key) = page_storage_key {
        if let Some(cursor) = sess.peek_synthetic_paging_resume(key) {
            let result = synthetic_page_result(sess, key, cursor, trace);
            let artifact = persist_execute_run(PersistExecuteRunInput {
                st,
                sess,
                session_id,
                entry_id: sess.entry_id.as_str(),
                source_line: line,
                display_lines: vec![line.to_string()],
                parsed: &parsed,
                result: &result,
                trace,
            })
            .await?;
            return Ok((parsed, result, Some(artifact)));
        }
    }
    let page_resume_owned: Option<QueryPaginationResumeData> = if let Some(ref key) =
        page_storage_key
    {
        Some(sess.peek_paging_resume(key).ok_or_else(|| {
                let detail = match trace.and_then(|t| t.logical_session_ref.as_deref()) {
                    Some(r) => format!(
                        "unknown paging handle `{}` — stale continuation or wrong logical session; use `page({r}_pgN)` from the latest tool result for this `logical_session_ref`",
                        key.as_str()
                    ),
                    None => format!(
                        "unknown paging handle `{}` (handles are minted when a paginated query returns additional pages)",
                        key.as_str()
                    ),
                };
                RunLineError::Parse(detail)
            })?)
    } else {
        None
    };

    let root_entity_owned: String = if let Some(r) = &page_resume_owned {
        r.query.entity.to_string()
    } else {
        parsed.expr.primary_entity().to_string()
    };
    let root_entity = root_entity_owned.as_str();
    let fed_holder = sess.federation_dispatch();
    let exec_cgs = crate::catalog_ownership::resolve_cgs_for_entity(sess, root_entity, None)
        .map_err(RunLineError::Parse)?;
    let parsed = crate::execute_pipeline::preflight_line_compile_dispatch(
        sess, sess, &parsed, line, exec_cgs,
    )
    .map_err(RunLineError::Parse)?;
    let engine_override = st
        .engine
        .config()
        .base_url
        .as_deref()
        .and_then(|b| crate::http_backend::ReplHttpOverride::from_engine_base(b).ok());
    let catalog_backend = fed_holder
        .as_ref()
        .and_then(|fed| fed.http_backend_for_entity(root_entity))
        .map(crate::http_backend::CatalogHttpBackend::from_cgs_field)
        .or_else(|| {
            sess.http_backend
                .as_deref()
                .map(crate::http_backend::CatalogHttpBackend::from_cgs_field)
        });
    let http_backend_for_root = crate::catalog_ownership::plan_http_origin(
        engine_override.as_ref(),
        catalog_backend.as_ref(),
    )
    .map(|origin| origin.as_str().to_string());
    let fp_sink = Arc::new(Mutex::new(Vec::<String>::new()));
    let (_, operation) = trace_expr_api_meta(&parsed.expr);

    let exec_opts = if let Some(shared) = plan_shared {
        shared
            .build_exec_opts(
                sess,
                st,
                exec_cgs,
                root_entity,
                fp_sink.clone(),
                preflight_token,
                rows_progress.clone(),
            )
            .await
    } else {
        let auth_for_exec = exec_cgs.auth.clone();
        let secret_provider = st.effective_outbound_secret_provider();
        let bound_share = sess.session_share_token.read().await.clone();
        let bound_proof_base_token = sess.session_proof_base_token.read().await.clone();
        let catalog_entry_for_bind = sess
            .federation_dispatch()
            .as_ref()
            .and_then(|_| {
                sess.contexts_by_entry.keys().find(|eid| {
                    sess.contexts_by_entry
                        .get(*eid)
                        .and_then(|ctx| ctx.get_entity(root_entity))
                        .is_some()
                })
            })
            .cloned()
            .unwrap_or_else(|| sess.entry_id.clone());
        let catalog_bind = sess
            .session_bindings_for_entry(&catalog_entry_for_bind)
            .map(|m| m.cml_env_entries());
        ExecuteOptions {
            request_fingerprint_sink: Some(fp_sink.clone()),
            http_base_url_override: http_backend_for_root.clone(),
            auth_resolver_override: auth_for_exec.map(|scheme| {
                Arc::new(
                    AuthResolver::new(scheme, secret_provider.clone())
                        .with_session_bearer_override(bound_share.clone()),
                )
            }),
            federation: fed_holder.clone(),
            preflight: Some(preflight_token),
            execute_session: Some(Arc::new(ExecuteSessionMaterial {
                prompt_hash: sess.prompt_hash.clone(),
                session_id: session_id.to_string(),
                share_token: bound_share,
                proof_base_token: bound_proof_base_token,
                transport_origin: http_backend_for_root.clone(),
                ui_origin: http_backend_for_root,
                catalog_bind,
            })),
            cancel: crate::operation::plan_execute_cancel_signal(),
            graph_page_spill: crate::graph_page_spill_host::graph_page_spill_for_execute(
                st.session_graph_persistence.as_ref(),
                sess.core.clone(),
                sess.prompt_hash.as_str(),
                session_id,
            ),
            rows_progress: rows_progress.clone(),
        }
    };
    let graph_spill_active = exec_opts.graph_page_spill.is_some();
    let page_resume_backup = page_resume_owned.clone();

    let mut credential_mutation = false;
    let mut result = match try_proof_document_share_bind(sess, exec_cgs, &parsed.expr).await? {
        Some(r) => {
            credential_mutation = true;
            r
        }
        None => {
            let input = crate::graph_execute::LiveBranchExecuteInput {
                line,
                log_expr: log_expr.as_str(),
                sess,
                st,
                session_id,
                parsed: &parsed,
                exec_cgs,
                root_entity,
                page_resume_backup: page_resume_backup.clone(),
                exec_opts: exec_opts.clone(),
                graph_spill_active,
                host_page_size,
                surface_read_budget: surface_read_budget.clone(),
                expr_span: expr_span.clone(),
            };
            match crate::graph_execute::run_with_write_conflict_retry(sess, &input).await {
                Ok(r) => r,
                Err(e) => {
                    record_run_line_error_metrics(
                        sess.entry_id.as_str(),
                        operation.as_str(),
                        &e,
                        wall,
                    );
                    return Err(e);
                }
            }
        }
    };

    if let Some(ref storage_key) = page_storage_key {
        if result.has_more {
            if let Some(next) = result.pagination_resume.take() {
                sess.upsert_paging_resume(storage_key, next);
            } else {
                sess.remove_paging_resume(storage_key);
            }
        } else {
            sess.remove_paging_resume(storage_key);
        }
    } else if result.has_more {
        if let Some(resume) = result.pagination_resume.take() {
            let h = sess.register_paging_continuation(
                resume,
                trace.and_then(|t| t.logical_session_ref.as_deref()),
            );
            result.paging_handle = Some(h);
        }
    }

    let base_token_refreshed =
        maybe_proof_refresh_session_base_token(sess, exec_cgs, &parsed, &result).await;
    if credential_mutation || base_token_refreshed {
        persist_session_credentials_after_mutation(st, sess, session_id).await;
    }

    result.request_fingerprints = fp_sink.lock().unwrap_or_else(|e| e.into_inner()).clone();

    let artifact = persist_execute_run(PersistExecuteRunInput {
        st,
        sess,
        session_id,
        entry_id: sess.entry_id.as_str(),
        source_line: line,
        display_lines: vec![line.to_string()],
        parsed: &parsed,
        result: &result,
        trace,
    })
    .await
    .map_err(|e| {
        let ms = wall.elapsed().as_secs_f64() * 1000.0;
        let phase = match &e {
            PersistExecuteRunError::Serialization(_) => "serialization",
            PersistExecuteRunError::Persist(_) => "artifact_persist",
            PersistExecuteRunError::Mint(_) => "parse",
        };
        if phase != "parse" {
            crate::metrics::record_execute_expression_line(
                sess.entry_id.as_str(),
                operation.as_str(),
                "error",
                phase,
                ms,
                0,
                0,
            );
        }
        RunLineError::from(e)
    })?;
    let artifact = Some(artifact);

    if let Some(ctx) = trace {
        if plan_shared.is_none() {
            let run_id = artifact.as_ref().map(|a| a.run_id.to_wire());
            let api = Some(trace_api_entry_id_for_execute_root(sess, root_entity));
            let call_idx = ctx.call_index.unwrap_or(0).max(0) as u64;
            emit_plasm_line_trace(
                PlasmLineTraceSink::Durable {
                    st,
                    ctx,
                    sess,
                    session_id,
                    run_id,
                },
                line,
                &parsed,
                &result,
                api,
                call_idx,
                line_index.max(0) as usize,
            )
            .await;
        }
    }

    let ms = wall.elapsed().as_secs_f64() * 1000.0;
    crate::metrics::record_execute_expression_line(
        sess.entry_id.as_str(),
        operation.as_str(),
        "success",
        "none",
        ms,
        result.stats.cache_hits as u64,
        result.stats.cache_misses as u64,
    );

    Ok((parsed, result, artifact))
}
