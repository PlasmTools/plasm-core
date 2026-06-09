//! Resume paginated queries from [`QueryPaginationResumeData`] snapshots.

use super::*;

impl ExecutionEngine {
    /// Resume a paginated query from a prior [`QueryPaginationResumeData`] snapshot (opaque LLM paging).
    pub fn execute_pagination_resume<'a>(
        &'a self,
        resume: QueryPaginationResumeData,
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: Option<ExecutionMode>,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let start_time = std::time::Instant::now();
            let base = self.resolve_http_base_from_opts(&opts);
            let auth_override = opts.auth_resolver_override.clone();
            let plugin_hooks = PluginCompileHooks::snapshot_from_execute_options(&opts);
            let fp_sink = opts.request_fingerprint_sink.clone();
            let federation = opts.federation.clone();
            let execute_session = opts.execute_session.clone();
            let cancel = opts.cancel.clone();
            let rows_progress = opts.rows_progress.clone();
            let mut result = Self::run_in_execute_task_scopes(
                base,
                auth_override,
                plugin_hooks,
                fp_sink.clone(),
                federation,
                execute_session,
                cancel,
                rows_progress,
                async move {
                    let mut stream = self.execute_pagination_resume_stream(
                        resume,
                        cgs,
                        mat,
                        mode,
                        consume.clone(),
                        &opts,
                    )?;
                    collect_query_stream(&mut stream, &consume).await
                },
            )
            .await?;
            result.stats.duration_ms = start_time.elapsed().as_millis() as u64;
            result.request_fingerprints = fp_sink
                .map(|m| m.lock().unwrap_or_else(|e| e.into_inner()).clone())
                .unwrap_or_default();
            Ok(result)
        })
    }

    /// Lazy stream for [`Self::execute_pagination_resume`].
    pub fn execute_pagination_resume_stream<'a>(
        &'a self,
        resume: QueryPaginationResumeData,
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: Option<ExecutionMode>,
        consume: StreamConsumeOpts,
        opts: &ExecuteOptions,
    ) -> Result<QueryStream<'a>, RuntimeError> {
        let qexpr = plasm_core::Expr::Query(resume.query.clone());
        if opts.preflight.is_none() {
            if let Some(ref fed) = opts.federation {
                type_check_expr_federated(&qexpr, fed.as_ref(), cgs)?;
            } else {
                type_check_expr(&qexpr, cgs)?;
            }
            reject_domain_placeholder_in_executable(&qexpr)?;
        }
        let execution_mode = mode.unwrap_or(self.config.default_mode);
        let capability = cgs
            .get_capability(resume.capability_name.as_str())
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!(
                    "unknown capability `{}` in pagination resume",
                    resume.capability_name
                ),
            })?;
        let state: PaginationLoopState = resume.state.try_into()?;
        let QueryPaginationResumeData {
            query,
            env,
            template,
            config,
            ..
        } = resume;
        self.paginated_query_stream(
            query,
            cgs,
            mat,
            execution_mode,
            template,
            config,
            env,
            capability,
            consume,
            Some(state),
            opts.graph_page_spill.clone(),
        )
    }
}
