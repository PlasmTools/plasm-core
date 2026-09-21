//! Auto-resolve missing projected fields via provider capabilities.

use super::*;

impl ExecutionEngine {
    /// Auto-resolve missing projected fields by invoking the providing capabilities.
    ///
    /// When a projection `[field1, field2]` is requested and one or more fields are absent
    /// from the cached entity, this method:
    ///
    /// 1. Builds the `field → capability` reverse index from `CGS::read_field_providers`
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
        let fp_sink = opts.request_fingerprint_sink.clone();
        let federation = opts.federation.clone();
        let execute_session = opts.execute_session.clone();
        let compiled_catalog = opts.required_compiled_catalog()?;
        let cancel = opts.cancel.clone();
        let rows_progress = opts.rows_progress.clone();
        Self::run_in_execute_task_scopes(
            base,
            auth_override,
            fp_sink,
            federation,
            execute_session,
            compiled_catalog,
            cancel,
            rows_progress,
            async {
                use futures_util::stream::{self, StreamExt};

                // Build reverse index: field → Vec<cap_name>
                let providers = cgs.read_field_providers(entity_type);

                // Select a satisfiable read for each missing field and preserve the
                // full reference (including composite identity) and scoped inputs.
                let mut cap_to_ids: std::collections::BTreeMap<
                    String,
                    Vec<(GetExpr, ViewAmbientContext)>,
                > = std::collections::BTreeMap::new();
                let identity = identity_keys_for_entity(cgs, entity_type);
                for entity in &entities {
                    let mut selected = HashSet::new();
                    for field in projection {
                        if entity
                            .fields
                            .get(field)
                            .is_some_and(|value| !value.is_null())
                        {
                            continue;
                        }
                        for name in providers.get(field).into_iter().flatten() {
                            let Some(cap) = cgs.get_capability(name) else {
                                continue;
                            };
                            let inherit =
                                CapabilityParamEnv::from_source_row_for_cap(cgs, mat, entity, cap);
                            if !inherit.missing_required(cap, &identity).is_empty() {
                                continue;
                            }
                            if selected.insert(name.clone()) {
                                let mut get = GetExpr::from_ref(entity.reference.clone());
                                get.capability_name = Some(cap.name.clone());
                                cap_to_ids.entry(name.clone()).or_default().push((
                                    get,
                                    ViewAmbientContext::default()
                                        .with_capability_params(inherit.bindings().clone()),
                                ));
                            }
                            break;
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
                    let concurrency = self
                        .config
                        .effective_hydrate_concurrency(cgs.entry_id.as_deref());

                    for (cap_name, mut gets) in cap_to_ids {
                        let mut seen = HashSet::new();
                        gets.retain(|(get, _)| seen.insert(get.reference.clone()));
                        let branch_seed = {
                            let snap = mat.snapshot();
                            SessionMaterialization::seed_read_branch(mat, snap.into_graph())
                        };
                        let mut stream = stream::iter(gets.into_iter().map(|(get, ambient)| {
                            let mut branch = branch_seed.clone();
                            async move {
                                // Entity completeness from another Get is not proof that
                                // this provider's missing fields have been retrieved.
                                let (row, _) = self
                                    .fetch_get_decoded(
                                        &get,
                                        cgs,
                                        mode,
                                        get.capability_name.as_deref(),
                                        false,
                                        Some(&mut branch),
                                        &ambient,
                                    )
                                    .await?;
                                if row.reference != get.reference {
                                    return Err(RuntimeError::ConfigurationError {
                                        message:
                                            "projection Get changed the requested entity identity"
                                                .into(),
                                    });
                                }
                                branch.insert(row.clone())?;
                                stamp_get_capability_params(&mut branch, cgs, &get, &ambient, &row);
                                Ok::<_, RuntimeError>(branch)
                            }
                        }))
                        .buffer_unordered(concurrency);

                        while let Some(res) = stream.next().await {
                            cooperative_cancel_check()?;
                            match res {
                                Ok(branch) => {
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
                    .map(|e| mat.get(&e.reference).cloned().unwrap_or_else(|| e.clone()))
                    .collect();

                Ok(refreshed)
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::ResolvedAuth;
    use crate::http_transport::HttpTransport;
    use async_trait::async_trait;
    use plasm_compile::CompiledRequest;
    use std::sync::{Arc, Mutex};

    struct ProjectionTransport(Arc<Mutex<Vec<String>>>);

    #[async_trait]
    impl HttpTransport for ProjectionTransport {
        async fn send_compiled_http(
            &self,
            _: &str,
            request: &CompiledRequest,
            _: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            self.0.lock().unwrap().push(request.path.clone());
            let body = match request.path.trim_start_matches('/') {
                "records/1" => serde_json::json!({"id":"1", "email":"one@example.test"}),
                "records/2" => serde_json::json!({"id":"2", "email":"two@example.test"}),
                "secrets/1" => serde_json::json!({"id":"1", "secret":"read value"}),
                "secrets/2" => serde_json::json!({"id":"2", "secret":"read value"}),
                unexpected => panic!("unexpected implicit provider: {unexpected}"),
            };
            Ok((body, None))
        }

        async fn get_json_absolute(
            &self,
            _: &str,
            _: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            panic!("unexpected absolute request")
        }
    }

    #[tokio::test]
    async fn projection_uses_exact_read_despite_other_complete_get_and_preserves_rows() {
        let cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/entity_projection_matrix"),
        )
        .unwrap();
        let cgs: CGS = serde_json::from_slice(&serde_json::to_vec(&cgs).unwrap()).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                ..Default::default()
            },
            Arc::new(ProjectionTransport(calls.clone())),
            None,
        );
        let mut mat = SessionMaterialization::new();
        let mut initial = Vec::new();
        for id in ["2", "1"] {
            let reference = Ref::new("Record", id);
            mat.stamp_capability_params(
                &reference,
                IndexMap::from([
                    ("credential".into(), Value::String("credential".into())),
                    ("unknown_input".into(), Value::String("scope".into())),
                ]),
            );
            let row = engine
                .execute(
                    &Expr::Get(GetExpr::from_ref(reference.clone())),
                    &cgs,
                    &mut mat,
                    Some(ExecutionMode::Live),
                    StreamConsumeOpts::default(),
                    ExecuteOptions::for_catalog(&cgs).unwrap(),
                )
                .await
                .unwrap()
                .entities
                .remove(0);
            initial.push(row);
        }
        initial.push(initial[0].clone());
        let rows = engine
            .auto_resolve_projection(
                initial,
                "Record",
                &["secret".into(), "unavailable".into()],
                &cgs,
                &mut mat,
                ExecutionMode::Live,
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        for (row, id) in rows.into_iter().zip(["2", "1", "2"]) {
            assert_eq!(row.reference, Ref::new("Record", id));
            assert_eq!(
                row.fields["secret"].to_value(),
                Value::String("read value".into())
            );
            assert!(!row.fields.contains_key("unavailable"));
        }
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 4, "one read per provider and identity");
        assert!(calls.iter().any(|path| path.ends_with("secrets/1")));
        assert!(calls
            .iter()
            .all(|path| ["records/1", "records/2", "secrets/1", "secrets/2"]
                .iter()
                .any(|expected| path.ends_with(expected))));
    }
}
