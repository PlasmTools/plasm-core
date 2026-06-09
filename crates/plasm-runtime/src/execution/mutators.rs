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

        let capability_template = parse_capability_template(&capability.mapping.template)?;

        let payload = if let Some(schema) = &capability.input_schema {
            InvokeInputPayload::lift(&create.input.to_value(), &schema.input_type, cgs)
        } else {
            create.input.clone()
        };

        let input = match capability.input_schema.as_ref() {
            Some(schema) => plasm_core::normalize_structured_string_inputs(
                payload.to_value(),
                &schema.input_type,
                cgs,
            ),
            None => payload.to_value(),
        };

        let mut env = CmlEnv::new();
        merge_plasm_execute_session_share_token_env(&mut env);
        merge_plasm_execute_session_proof_base_token_env(&mut env);
        env.insert("input".to_string(), input.clone());
        if let Value::Object(ref map) = input {
            // Path segments: same as the historical loop.
            for var_name in path_var_names_from_template(&capability_template) {
                if let Some(v) = map.get(&var_name) {
                    env.insert(var_name.clone(), v.clone());
                }
            }
            // Body/query template vars: mirror invoke's input overlay so `var title` (etc.)
            // resolves without stuffing path-only keys into `body: { type: var, name: input }`.
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

        merge_plasm_execute_session_env(&mut env);

        let compiled = compile_operation_dispatch(&capability_template, &env)?;

        match mode {
            ExecutionMode::Live => {
                ensure_http_operation(&compiled, "create")?;
                let (response, _) = with_dispatch_entity(
                    Some(create.entity.as_str()),
                    self.execute_operation_full(&compiled),
                )
                .await?;
                let response =
                    narrow_http_graphql_response_for_entity_decode(&capability_template, response)?;
                let decoder = mutating_capability_response_decoder(
                    create.entity.as_str(),
                    create.capability.as_str(),
                    cgs,
                    &env,
                    None,
                );
                let decoded = decode_entities(&decoder, &response)?;

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

                Ok(ExecutionResult {
                    entities,
                    count,
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Live,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: 1,
                        cache_hits: 0,
                        cache_misses: count,
                        ..Default::default()
                    },
                    request_fingerprints: Vec::new(),
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

        let capability_template = parse_capability_template(&capability.mapping.template)?;

        let mut env = CmlEnv::new();
        merge_plasm_execute_session_share_token_env(&mut env);
        merge_plasm_execute_session_proof_base_token_env(&mut env);
        let target_ent = cgs.get_entity(delete.target.entity_type.as_str());
        populate_template_path_env(
            &mut env,
            &capability_template,
            &delete.target,
            target_ent,
            delete.path_vars.as_ref(),
            None,
        );
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
                ensure_http_operation(&compiled, "delete")?;
                let (response, _) = with_dispatch_entity(
                    Some(delete.target.entity_type.as_str()),
                    self.execute_operation_full(&compiled),
                )
                .await?;
                preflight_fibery_command_envelope(&response)?;

                // Remove from cache if present
                mat.remove(&delete.target);

                Ok(ExecutionResult {
                    entities: vec![],
                    count: 0,
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Live,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: 1,
                        cache_hits: 0,
                        cache_misses: 0,
                        ..Default::default()
                    },
                    request_fingerprints: Vec::new(),
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

        let capability_template = parse_capability_template(&capability.mapping.template)?;

        let input_for_env = match &invoke.input {
            None => None,
            Some(input) => {
                let payload = if let Some(schema) = &capability.input_schema {
                    InvokeInputPayload::lift(&input.to_value(), &schema.input_type, cgs)
                } else {
                    input.clone()
                };
                Some(match capability.input_schema.as_ref() {
                    Some(schema) => plasm_core::normalize_structured_string_inputs(
                        payload.to_value(),
                        &schema.input_type,
                        cgs,
                    ),
                    None => payload.to_value(),
                })
            }
        };

        let mut env = CmlEnv::new();
        merge_plasm_execute_session_share_token_env(&mut env);
        merge_plasm_execute_session_proof_base_token_env(&mut env);
        let target_ent = cgs.get_entity(invoke.target.entity_type.as_str());
        populate_template_path_env(
            &mut env,
            &capability_template,
            &invoke.target,
            target_ent,
            invoke.path_vars.as_ref(),
            input_for_env.as_ref(),
        );

        if let Some(input) = &input_for_env {
            env.insert("input".to_string(), input.clone());
            if let Value::Object(map) = input {
                for (k, v) in map {
                    env.insert(k.clone(), v.clone());
                }
            }
        }
        normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: e.to_string(),
            }
        })?;
        merge_entity_id_from_into_input_env(&mut env, target_ent, capability);

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

        merge_plasm_execute_session_env(&mut env);

        let compiled = compile_operation_dispatch(&capability_template, &env)?;

        match mode {
            ExecutionMode::Live => {
                ensure_http_operation(&compiled, "invoke")?;
                let (response, _) = with_dispatch_entity(
                    Some(invoke.target.entity_type.as_str()),
                    self.execute_operation_full(&compiled),
                )
                .await?;
                let response =
                    narrow_http_graphql_response_for_entity_decode(&capability_template, response)?;

                // Decode the response as the capability's declared entity type.
                // When an action returns a projection of the same entity (e.g.
                // page_get_markdown returns {id, markdown, truncated} for a Page),
                // the decoder extracts only the fields present in the response, and
                // the cache's additive merge preserves existing fields from other
                // projections (e.g. url, timestamps from page_get).
                let rid = invoke.target.simple_id().map(|s| s.as_str());
                let decoder = mutating_capability_response_decoder(
                    invoke.target.entity_type.as_str(),
                    invoke.capability.as_str(),
                    cgs,
                    &env,
                    rid,
                );
                let decoded = decode_entities(&decoder, &response).unwrap_or_default();

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

                if count > 0 {
                    mat.merge(entities.clone())?;
                }

                Ok(ExecutionResult {
                    entities,
                    count,
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Live,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: 1,
                        cache_hits: 0,
                        cache_misses: count,
                        ..Default::default()
                    },
                    request_fingerprints: Vec::new(),
                })
            }
            _ => Err(RuntimeError::UnsupportedExecutionMode {
                mode: format!("invoke with {:?} mode", mode),
            }),
        }
    }
}
