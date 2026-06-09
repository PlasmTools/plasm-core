//! Auto-resolve missing projected fields via provider capabilities.

use super::*;

impl ExecutionEngine {
    /// Auto-resolve missing projected fields by invoking the providing capabilities.
    ///
    /// When a projection `[field1, field2]` is requested and one or more fields are absent
    /// from the cached entity, this method:
    ///
    /// 1. Builds the `field → capability` reverse index from `CGS::field_providers`
    /// 2. For each entity, determines which projected fields are missing (null or absent)
    /// 3. Groups missing fields by their providing capability
    /// 4. Invokes each provider capability concurrently for the affected entities
    /// 5. The results are additive-merged into cache; returns the enriched entities
    ///
    /// This makes `Page("id")[markdown]` automatically invoke `page_get_markdown` when
    /// `markdown` is not yet in cache — without any manual multi-step workflow.
    #[allow(clippy::too_many_arguments)]
    pub async fn auto_resolve_projection(
        &self,
        entities: Vec<CachedEntity>,
        entity_type: &str,
        projection: &[String],
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        opts: ExecuteOptions,
    ) -> Result<Vec<CachedEntity>, RuntimeError> {
        let base = self.resolve_http_base_from_opts(&opts);
        let auth_override = opts.auth_resolver_override.clone();
        let plugin_hooks = PluginCompileHooks::snapshot_from_execute_options(&opts);
        let fp_sink = opts.request_fingerprint_sink.clone();
        let federation = opts.federation.clone();
        let execute_session = opts.execute_session.clone();
        let cancel = opts.cancel.clone();
        let rows_progress = opts.rows_progress.clone();
        Self::run_in_execute_task_scopes(
            base,
            auth_override,
            plugin_hooks,
            fp_sink,
            federation,
            execute_session,
            cancel,
            rows_progress,
            async {
                use futures_util::stream::{self, StreamExt};

                // Build reverse index: field → Vec<cap_name>
                let providers = cgs.field_providers(entity_type);

                // For each entity, find which projected fields are missing.
                // Group: capability_name → Vec<entity_id>
                let mut cap_to_ids: std::collections::HashMap<String, Vec<String>> =
                    std::collections::HashMap::new();

                for entity in &entities {
                    for field in projection {
                        // Field is "missing" if it's absent from the entity's field map or null.
                        let is_missing = entity
                            .fields
                            .get(field)
                            .map(|v| v.is_null())
                            .unwrap_or(true);

                        if is_missing {
                            if let Some(cap_names) = providers.get(field) {
                                // Use the first (highest-priority) provider.
                                if let Some(cap_name) = cap_names.first() {
                                    // Skip if we already have a Complete entry from this provider
                                    // for this entity (i.e. the field IS in the cache under this cap).
                                    cap_to_ids
                                        .entry(cap_name.clone())
                                        .or_default()
                                        .push(entity.reference.primary_slot_str());
                                }
                            }
                        }
                    }
                }

                if cap_to_ids.is_empty() {
                    return Ok(entities);
                }

                let projection_span =
                    crate::spans::projection_hydrate(entity_type, cap_to_ids.len());
                async {
                    // For each provider capability, invoke it for all entity IDs that need it.
                    let concurrency = self.config.hydrate_concurrency.max(1);

                    for (cap_name, ids) in cap_to_ids {
                        let Some(cap) = cgs.get_capability(&cap_name) else {
                            continue;
                        };

                        // Deduplicate IDs
                        let mut unique_ids = ids;
                        unique_ids.sort_unstable();
                        unique_ids.dedup();

                        // Build one expression per entity ID
                        let exprs: Vec<(String, Expr)> = unique_ids
                            .into_iter()
                            .map(|id| {
                                let expr = match cap.kind {
                                    plasm_core::CapabilityKind::Get => {
                                        let get = GetExpr::new(entity_type, &id);
                                        Expr::Get(get)
                                    }
                                    _ => {
                                        // action / update / etc. — invoke with no input
                                        let inv =
                                            InvokeExpr::new(&cap_name, entity_type, &id, None);
                                        Expr::Invoke(inv)
                                    }
                                };
                                (id, expr)
                            })
                            .collect();

                        let branch_seed = {
                            let snap = mat.snapshot();
                            SessionMaterialization {
                                graph: snap.into_graph(),
                                responses: mat.responses.clone(),
                                query_index: mat.query_index.clone(),
                            }
                        };

                        let mut stream = stream::iter(exprs.into_iter().map(|(_id, expr)| {
                            let mut branch = branch_seed.clone();
                            async move {
                                let result = match &expr {
                                    Expr::Get(g) => {
                                        self.execute_get(g, cgs, &mut branch, mode).await
                                    }
                                    Expr::Invoke(inv) => {
                                        self.execute_invoke(inv, cgs, &mut branch, mode).await
                                    }
                                    _ => Err(RuntimeError::ConfigurationError {
                                        message: "auto_resolve_projection: unexpected expr type"
                                            .into(),
                                    }),
                                };
                                result.map(|r| (r, branch))
                            }
                        }))
                        .buffer_unordered(concurrency);

                        while let Some(res) = stream.next().await {
                            cooperative_cancel_check()?;
                            match res {
                                Ok((_result, branch)) => {
                                    mat.absorb_branch(branch)?;
                                }
                                Err(e) => {
                                    // Best-effort: log the error but don't fail the whole resolution.
                                    // The field will simply remain absent in the output.
                                    tracing::warn!(
                                        target: "plasm_runtime::projection",
                                        capability = cap_name.as_str(),
                                        error = %e,
                                        "projection provider invocation failed"
                                    );
                                }
                            }
                        }
                    }
                    Ok::<(), RuntimeError>(())
                }
                .instrument(projection_span)
                .await?;

                // Re-read the (now-enriched) entities from cache.
                let refreshed: Vec<CachedEntity> = entities
                    .iter()
                    .filter_map(|e| mat.get(&e.reference).cloned())
                    .collect();

                Ok(refreshed)
            },
        )
        .await
    }
}
