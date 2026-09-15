//! Create, delete, and invoke expression execution.

use super::*;

impl ExecutionEngine {
    /// Execute a create expression (no target ID — creates a new resource)
    pub(crate) async fn execute_create(
        &self,
        create: &plasm_core::CreateExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        let capability = cgs
            .get_capability(create.capability.as_str())
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: create.capability.to_string(),
                entity: create.entity.to_string(),
            })?;

        let capability_template = compiled_capability_template(capability)?;

        let payload = if let Some(schema) = &capability.inputs.payload {
            InvokeInputPayload::lift(&create.input.to_value(), &schema.input_type, cgs)
        } else {
            create.input.clone()
        };

        let input = match capability.inputs.payload.as_ref() {
            Some(schema) => plasm_core::normalize_structured_string_inputs(
                payload.to_value(),
                &schema.input_type,
                cgs,
            ),
            None => payload.to_value(),
        };

        let input = plasm_core::prepare_create_capability_input(capability, create, input, cgs);

        let mut env = CmlEnv::new();
        env.insert("input".to_string(), input.clone());
        if let Value::Object(ref map) = input {
            // Full input overlay: path/query/body vars resolve from the same object (no
            // separate path-var harvest — that duplicated this loop).
            for (k, v) in map {
                env.insert(k.clone(), v.clone());
            }
        }
        normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: e.to_string(),
            }
        })?;

        apply_preflight_steps(self, capability, cgs, mat, mode, &mut env, None, true).await?;

        if crate::workflow_reconcile::should_skip_write_after_preflight(&env) {
            return Ok(crate::workflow_reconcile::skipped_write_result(
                create.entity.as_str(),
            ));
        }

        merge_plasm_execute_session_env(&mut env);

        let compiled = compile_operation_dispatch(&capability_template, &env)?;

        match mode {
            ExecutionMode::Live => {
                ensure_mutating_operation(&compiled, "create")?;
                let http_res = with_dispatch_entity(
                    Some(create.entity.as_str()),
                    self.execute_operation_full(&compiled),
                )
                .await;
                let (response, _) = match http_res {
                    Ok(v) => v,
                    Err(e) => {
                        return self
                            .try_reconcile_mutator_error(
                                e,
                                capability,
                                cgs,
                                mat,
                                mode,
                                &input,
                                create.entity.as_str(),
                            )
                            .await;
                    }
                };
                let response =
                    narrow_http_graphql_response_for_entity_decode(&capability_template, response)?;
                let identity_ambient = cml_env_to_identity_strings(&env);
                let decoder = mutating_capability_response_decoder(
                    create.entity.as_str(),
                    create.capability.as_str(),
                    cgs,
                    &identity_ambient,
                    None,
                );
                let decoded = decode_entities_with_cgs(&decoder, &response, Some(cgs))?;

                let timestamp = current_timestamp();
                let entities: Vec<CachedEntity> = decoded
                    .into_iter()
                    .map(|d| {
                        CachedEntity::from_decoded(
                            d.reference,
                            d.fields,
                            d.relations,
                            timestamp,
                            EntityCompleteness::Complete,
                        )
                    })
                    .collect();
                let count = entities.len();
                let stats = ExecutionStats {
                    duration_ms: 0,
                    network_requests: usize::from(!matches!(
                        compiled,
                        CompiledOperation::CredentialBind(_)
                    )),
                    cache_hits: 0,
                    cache_misses: count,
                    ..Default::default()
                };

                Ok(ExecutionResult {
                    entities,
                    count,
                    has_more: false,
                    coverage: ResultCoverage::Complete,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Live,
                    stats,
                    request_fingerprints: Vec::new(),
                    operations: write_operations(
                        create.catalog_entry_id.as_deref(),
                        create.entity.as_str(),
                        capability,
                        ExecutionSource::Live,
                        1,
                        0,
                    ),
                })
            }
            _ => Err(RuntimeError::UnsupportedExecutionMode {
                mode: format!("create with {:?}", mode),
            }),
        }
    }

    /// Execute a delete expression
    pub(crate) async fn execute_delete(
        &self,
        delete: &plasm_core::DeleteExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        let capability = cgs
            .get_capability(delete.capability.as_str())
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: delete.capability.to_string(),
                entity: delete.target.entity_type.to_string(),
            })?;

        let capability_template = compiled_capability_template(capability)?;

        let mut env = CmlEnv::new();
        let target_ent = cgs
            .get_entity(delete.target.entity_type.as_str())
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!(
                    "unknown entity `{}` for delete identity-env projection",
                    delete.target.entity_type
                ),
            })?;
        let input_for_env = super::compile_preflight::targeted_call_input(delete, capability, cgs);
        let mut overlay_map = mat.capability_params_for(&delete.target);
        if let Some(Value::Object(input)) = &input_for_env {
            overlay_map.extend(input.clone());
        }
        let session_overlay = (!overlay_map.is_empty()).then(|| Value::Object(overlay_map));
        populate_template_path_env(
            &mut env,
            capability,
            &delete.target,
            plasm_core::IdentityProjectionCtx::Entity(target_ent),
            session_overlay.as_ref(),
        )?;
        if let Some(input) = input_for_env {
            env.insert("input".to_string(), input);
        }
        normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: e.to_string(),
            }
        })?;

        merge_plasm_execute_session_env(&mut env);

        let compiled = compile_operation_dispatch(&capability_template, &env)?;

        match mode {
            ExecutionMode::Live => {
                ensure_mutating_operation(&compiled, "delete")?;
                let (response, _) = with_dispatch_entity(
                    Some(delete.target.entity_type.as_str()),
                    self.execute_operation_full(&compiled),
                )
                .await?;
                preflight_fibery_command_envelope(&response)?;

                // Remove from cache if present
                mat.remove(&delete.target);
                mat.poison_read_caches_after_mutation();

                let stats = ExecutionStats {
                    duration_ms: 0,
                    network_requests: usize::from(!matches!(
                        compiled,
                        CompiledOperation::CredentialBind(_)
                    )),
                    cache_hits: 0,
                    cache_misses: 0,
                    ..Default::default()
                };

                Ok(ExecutionResult {
                    entities: vec![],
                    count: 0,
                    has_more: false,
                    coverage: ResultCoverage::Complete,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Live,
                    stats,
                    request_fingerprints: Vec::new(),
                    operations: write_operations(
                        delete.catalog_entry_id.as_deref(),
                        delete.target.entity_type.as_str(),
                        capability,
                        ExecutionSource::Live,
                        1,
                        0,
                    ),
                })
            }
            _ => Err(RuntimeError::UnsupportedExecutionMode {
                mode: format!("delete with {:?}", mode),
            }),
        }
    }

    /// Execute an invoke expression
    pub(crate) async fn execute_invoke(
        &self,
        invoke: &InvokeExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        let capability = cgs
            .get_capability(invoke.capability.as_str())
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: invoke.capability.to_string(),
                entity: invoke.target.entity_type.to_string(),
            })?;

        let capability_template = compiled_capability_template(capability)?;

        let target_ent = cgs
            .get_entity(invoke.target.entity_type.as_str())
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!(
                    "unknown entity `{}` for invoke identity-env projection",
                    invoke.target.entity_type
                ),
            })?;

        let input_for_env = super::compile_preflight::targeted_call_input(invoke, capability, cgs);

        let mut env = CmlEnv::new();
        let mut overlay_map = mat.capability_params_for(&invoke.target);
        if let Some(Value::Object(input)) = &input_for_env {
            for (k, v) in input {
                overlay_map.insert(k.clone(), v.clone());
            }
        } else if let Some(input) = &input_for_env {
            // Non-object invoke payloads still flow via the dedicated `input` env key below.
            let _ = input;
        }
        let overlay = (!overlay_map.is_empty()).then(|| Value::Object(overlay_map));
        populate_template_path_env(
            &mut env,
            capability,
            &invoke.target,
            plasm_core::IdentityProjectionCtx::Entity(target_ent),
            overlay.as_ref(),
        )?;

        // Aggregate `input` for body: { type: var, name: input }. Object field keys were
        // already merged into overlay above — do not splat them a second time.
        if let Some(input) = &input_for_env {
            env.insert("input".to_string(), input.clone());
        }
        normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: e.to_string(),
            }
        })?;
        merge_entity_id_from_into_input_env(&mut env, Some(target_ent), capability);

        apply_preflight_steps(
            self,
            capability,
            cgs,
            mat,
            mode,
            &mut env,
            Some(PreflightInvoke { invoke }),
            false,
        )
        .await?;

        if crate::workflow_reconcile::should_skip_write_after_preflight(&env) {
            return Ok(crate::workflow_reconcile::skipped_write_result(
                invoke.target.entity_type.as_str(),
            ));
        }

        merge_plasm_execute_session_env(&mut env);

        let compiled = compile_operation_dispatch(&capability_template, &env)?;

        match mode {
            ExecutionMode::Live => {
                ensure_mutating_operation(&compiled, "invoke")?;
                let http_res = with_dispatch_entity(
                    Some(invoke.target.entity_type.as_str()),
                    self.execute_operation_full(&compiled),
                )
                .await;
                let (response, _) = match http_res {
                    Ok(v) => v,
                    Err(e) => {
                        let input_val = input_for_env
                            .clone()
                            .unwrap_or(Value::Object(indexmap::IndexMap::new()));
                        return self
                            .try_reconcile_mutator_error(
                                e,
                                capability,
                                cgs,
                                mat,
                                mode,
                                &input_val,
                                invoke.target.entity_type.as_str(),
                            )
                            .await;
                    }
                };
                let response =
                    narrow_http_graphql_response_for_entity_decode(&capability_template, response)?;

                // Decode the response as the capability's declared entity type.
                // When an action returns a projection of the same entity (e.g.
                // page_get_markdown returns {id, markdown, truncated} for a Page),
                // the decoder extracts only the fields present in the response, and
                // the cache's additive merge preserves existing fields from other
                // projections (e.g. url, timestamps from page_get).
                let rid = invoke.target.simple_id().map(|s| s.as_str());
                let identity_ambient = cml_env_to_identity_strings(&env);
                let decoder = mutating_capability_response_decoder(
                    invoke.target.entity_type.as_str(),
                    invoke.capability.as_str(),
                    cgs,
                    &identity_ambient,
                    rid,
                );
                let decoded = if capability.provides.is_empty() {
                    // True side-effect Actions may return empty/opaque bodies.
                    decode_entities_with_cgs(&decoder, &response, Some(cgs)).unwrap_or_default()
                } else {
                    // Action-with-`provides` must materialize catalog-qualified rows (e.g. AuthSession
                    // access_token) for downstream hole fill / CML env — never swallow decode failure.
                    decode_entities_with_cgs(&decoder, &response, Some(cgs))?
                };

                let timestamp = current_timestamp();
                let entities: Vec<CachedEntity> = decoded
                    .into_iter()
                    .map(|d| {
                        CachedEntity::from_decoded(
                            d.reference,
                            d.fields,
                            d.relations,
                            timestamp,
                            EntityCompleteness::Complete,
                        )
                    })
                    .collect();
                let count = entities.len();

                // Merge only when `provides` names authoritative fields. Side-effect actions
                // often echo a projection under an empty / wrong Ref (identity lives in method
                // params, not the invoke target); merging that ghost must not satisfy later Gets.
                // Always invalidate + poison when `invalidates_entities` is set — even if decode
                // yields zero rows — so composed primary_read re-fetches live.
                if count > 0 && !capability.provides.is_empty() {
                    mat.merge(entities.clone())?;
                    let mut overlay = IndexMap::new();
                    for entity in &entities {
                        for name in &capability.provides {
                            if let Some(field) = entity.get_field(name) {
                                overlay.insert(name.clone(), field.to_value());
                            }
                        }
                    }
                    if let Some(plasm_core::Value::String(token)) = overlay.get("access_token") {
                        if let Some(material) =
                            super::session::try_current_execute_session_material()
                        {
                            material.note_login_access_token(token);
                        }
                    }
                    mat.stamp_provided_session_params(
                        SessionMaterialization::provide_catalog_key(
                            cgs,
                            invoke.catalog_entry_id.as_deref(),
                        ),
                        overlay,
                    );
                }
                if count > 0 || !capability.invalidates_entities.is_empty() {
                    mat.apply_post_mutation_cache_effects(capability, cgs)?;
                    mat.poison_read_caches_after_mutation();
                }

                let stats = ExecutionStats {
                    duration_ms: 0,
                    network_requests: usize::from(!matches!(
                        compiled,
                        CompiledOperation::CredentialBind(_)
                    )),
                    cache_hits: 0,
                    cache_misses: count,
                    ..Default::default()
                };

                Ok(ExecutionResult {
                    entities,
                    count,
                    has_more: false,
                    coverage: ResultCoverage::Complete,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Live,
                    stats,
                    request_fingerprints: Vec::new(),
                    operations: write_operations(
                        invoke.catalog_entry_id.as_deref(),
                        invoke.target.entity_type.as_str(),
                        capability,
                        ExecutionSource::Live,
                        1,
                        0,
                    ),
                })
            }
            _ => Err(RuntimeError::UnsupportedExecutionMode {
                mode: format!("invoke with {:?} mode", mode),
            }),
        }
    }
}

fn write_operations(
    entry_id: Option<&str>,
    entity: &str,
    capability: &plasm_core::CapabilitySchema,
    source: ExecutionSource,
    completed: usize,
    failed: usize,
) -> super::OperationLedger {
    super::OperationLedger::from_ack(super::OperationAck::from_capability(
        entry_id.unwrap_or(""),
        entity,
        capability,
        source,
        completed,
        failed,
    ))
}
