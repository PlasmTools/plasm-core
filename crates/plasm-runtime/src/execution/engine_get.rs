//! GET / fetch paths on ExecutionEngine.

use super::*;
use tracing::Instrument;

impl ExecutionEngine {
    pub(crate) async fn execute_get(
        &self,
        get: &GetExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        ambient: &ViewAmbientContext,
    ) -> Result<ExecutionResult, RuntimeError> {
        self.execute_get_inner(get, cgs, mat, mode, ambient)
            .instrument(crate::spans::execute_get())
            .await
    }

    pub(crate) async fn execute_get_inner(
        &self,
        get: &GetExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        ambient: &ViewAmbientContext,
    ) -> Result<ExecutionResult, RuntimeError> {
        // Satisfy from cache only when we already hold a detail payload.
        if let Some(entity) = mat.get(&get.reference) {
            if entity.completeness == EntityCompleteness::Complete {
                return Ok(ExecutionResult {
                    entities: vec![entity.clone()],
                    count: 1,
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Cache,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: 0,
                        cache_hits: 1,
                        cache_misses: 0,
                        ..Default::default()
                    },
                    request_fingerprints: Vec::new(),
                });
            }
        }

        let get = get_with_session_params(get, cgs, mat);
        let (cached, source) = self
            .fetch_get_decoded(
                &get,
                cgs,
                mode,
                get.capability_name.as_deref(),
                true,
                Some(mat),
                ambient,
            )
            .await?;
        mat.insert(cached.clone())?;
        if let Some(pv) = get.path_vars.clone() {
            mat.stamp_capability_params(&cached.reference, pv);
        }

        Ok(ExecutionResult {
            entities: vec![cached],
            count: 1,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source,
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: if source == ExecutionSource::Live {
                    1
                } else {
                    0
                },
                cache_hits: 0,
                cache_misses: 1,
                ..Default::default()
            },
            request_fingerprints: Vec::new(),
        })
    }

    /// Like [`Self::execute_get`] for HTTP-backed GET capabilities used **inside** a composed `views:` DAG.
    pub(crate) async fn execute_get_for_view_dag(
        &self,
        get: &GetExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        if let Some(entity) = mat.get(&get.reference) {
            if entity.completeness == EntityCompleteness::Complete {
                return Ok(ExecutionResult {
                    entities: vec![entity.clone()],
                    count: 1,
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Cache,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: 0,
                        cache_hits: 1,
                        cache_misses: 0,
                        ..Default::default()
                    },
                    request_fingerprints: Vec::new(),
                });
            }
        }

        let capability = cgs
            .find_capability(&get.reference.entity_type, plasm_core::CapabilityKind::Get)
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: "get".to_string(),
                entity: get.reference.entity_type.to_string(),
            })?;
        let capability_template = parse_capability_template(&capability.mapping.template)?;
        if matches!(capability_template, CapabilityTemplate::View(_)) {
            return Err(RuntimeError::ConfigurationError {
                message:
                    "composed-view GET transport cannot nest as an inner node inside another views: DAG"
                        .into(),
            });
        }
        let get = get_with_session_params(get, cgs, mat);
        let (cached, source) = self
            .fetch_http_transport_get_decoded(
                &get,
                cgs,
                mode,
                capability,
                &capability_template,
                true,
                Some(mat),
            )
            .await?;
        mat.insert(cached.clone())?;
        if let Some(pv) = get.path_vars.clone() {
            mat.stamp_capability_params(&cached.reference, pv);
        }

        Ok(ExecutionResult {
            entities: vec![cached],
            count: 1,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source,
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: if source == ExecutionSource::Live {
                    1
                } else {
                    0
                },
                cache_hits: 0,
                cache_misses: 1,
                ..Default::default()
            },
            request_fingerprints: Vec::new(),
        })
    }

    /// When the primary capability's CML [`HttpResponseDecode::auxiliary_merge`] is set, perform that
    /// follow-up GET (same [`CmlEnv`] as the primary request) and shallow-merge one field into the
    /// primary JSON before narrowing/decoding.
    pub(crate) async fn apply_auxiliary_http_merge_response(
        &self,
        capability_template: &CapabilityTemplate,
        env: &CmlEnv,
        mode: ExecutionMode,
        primary: serde_json::Value,
        entity_dispatch_hint: Option<&str>,
    ) -> Result<serde_json::Value, RuntimeError> {
        let CapabilityTemplate::Http(primary_cml) = capability_template else {
            return Ok(primary);
        };
        let Some(ref decode) = primary_cml.response else {
            return Ok(primary);
        };
        let Some(ref aux) = decode.auxiliary_merge else {
            return Ok(primary);
        };

        let aux_req = CmlRequest {
            method: aux.method.clone(),
            path: aux.path.clone(),
            query: aux.query.clone(),
            body: None,
            body_format: HttpBodyFormat::default(),
            multipart: None,
            headers: aux.headers.clone(),
            pagination: None,
            response: None,
        };
        let aux_template = CapabilityTemplate::Http(aux_req);
        let compiled = compile_operation_dispatch(&aux_template, env)?;

        let aux_body = match with_dispatch_entity(
            entity_dispatch_hint,
            self.execute_with_replay(&compiled, mode, None),
        )
        .await
        {
            Ok((v, _)) => v,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "auxiliary_merge request failed; continuing with primary response only"
                );
                return Ok(primary);
            }
        };

        let extracted = if aux.from_path.is_empty() {
            aux_body
        } else {
            walk_json_path(&aux_body, &aux.from_path)
                .cloned()
                .unwrap_or(serde_json::Value::Null)
        };

        let serde_json::Value::Object(mut primary_obj) = primary else {
            return Ok(primary);
        };
        primary_obj.insert(aux.into_key.clone(), extracted);
        Ok(serde_json::Value::Object(primary_obj))
    }

    /// HTTP(S)/GraphQL GET path only — never dispatches composed [`CapabilityTemplate::View`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn fetch_http_transport_get_decoded(
        &self,
        get: &GetExpr,
        cgs: &CGS,
        mode: ExecutionMode,
        capability: &CapabilitySchema,
        capability_template: &CapabilityTemplate,
        inject_execute_session_env: bool,
        mut cache: Option<&mut SessionMaterialization>,
    ) -> Result<(CachedEntity, ExecutionSource), RuntimeError> {
        let mut env = CmlEnv::new();
        if inject_execute_session_env {
            merge_plasm_execute_session_proof_base_token_env(&mut env);
        }
        let target_ent = cgs.get_entity(get.reference.entity_type.as_str());
        populate_template_path_env(
            &mut env,
            capability_template,
            &get.reference,
            target_ent,
            get.path_vars.as_ref(),
            None,
        );
        normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: e.to_string(),
            }
        })?;

        if inject_execute_session_env {
            merge_plasm_execute_session_env(&mut env);
        }

        let compiled = compile_operation_dispatch(capability_template, &env)?;
        let (response, source) = with_dispatch_entity(
            Some(get.reference.entity_type.as_str()),
            self.execute_with_replay(&compiled, mode, cache.as_deref_mut()),
        )
        .await?;
        let response = self
            .apply_auxiliary_http_merge_response(
                capability_template,
                &env,
                mode,
                response,
                Some(get.reference.entity_type.as_str()),
            )
            .await?;
        let response =
            narrow_http_graphql_response_for_entity_decode(capability_template, response)?;
        let rid = cgs.get_entity(&get.reference.entity_type).and_then(|ent| {
            if ent.implicit_request_identity || ent.id_field == "url" {
                get.reference.simple_id().map(|id| id.as_str())
            } else {
                None
            }
        });
        let identity_ambient = decode_identity_ambient_for_ref(&get.reference, &env);
        let decoder = create_entity_decoder_for_capability(
            &get.reference.entity_type,
            cgs,
            Some(capability.name.as_str()),
            None,
            rid,
            Some(&identity_ambient),
        );
        let decoded_entities = decode_entities_with_cgs(&decoder, &response, Some(cgs))?;

        let decoded = decoded_entities
            .first()
            .ok_or_else(|| RuntimeError::CacheError {
                message: format!("Entity not found: {}", get.reference),
            })?;

        let timestamp = current_timestamp();
        let cached = if let Some(session) = cache {
            cache_decoded_entity_tree(
                session,
                decoded.clone(),
                timestamp,
                EntityCompleteness::Complete,
            )?
        } else {
            CachedEntity::from_decoded(
                decoded.reference.clone(),
                decoded.fields.clone(),
                decoded.relations.clone(),
                timestamp,
                EntityCompleteness::Complete,
            )
        };
        Ok((cached, source))
    }

    /// Run GET + decode without consulting the graph cache (used for query hydration and cache refresh).
    ///
    /// When `hydrate_capability` is `Some(name)`, use that named GET capability instead of the
    /// default per-entity `find_capability(.., Get)` (used by preflight hydrate steps).
    ///
    /// When `inject_execute_session_env` is true, reserved `plasm_execute_*` keys are merged for
    /// user-facing GETs only — internal preflight/hydrate GETs pass `false`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn fetch_get_decoded(
        &self,
        get: &GetExpr,
        cgs: &CGS,
        mode: ExecutionMode,
        hydrate_capability: Option<&str>,
        inject_execute_session_env: bool,
        cache: Option<&mut SessionMaterialization>,
        ambient: &ViewAmbientContext,
    ) -> Result<(CachedEntity, ExecutionSource), RuntimeError> {
        let capability: &CapabilitySchema = match hydrate_capability {
            Some(name) => {
                let c =
                    cgs.get_capability(name)
                        .ok_or_else(|| RuntimeError::CapabilityNotFound {
                            capability: name.to_string(),
                            entity: get.reference.entity_type.to_string(),
                        })?;
                if c.kind != plasm_core::CapabilityKind::Get {
                    return Err(RuntimeError::ConfigurationError {
                        message: format!("preflight hydrate get '{name}' must be kind get"),
                    });
                }
                if c.domain.as_str() != get.reference.entity_type.as_str() {
                    return Err(RuntimeError::ConfigurationError {
                        message: format!(
                            "preflight: hydrate capability '{name}' is for entity {}, expected {}",
                            c.domain.as_str(),
                            get.reference.entity_type
                        ),
                    });
                }
                c
            }
            None => cgs
                .find_capability(&get.reference.entity_type, plasm_core::CapabilityKind::Get)
                .ok_or_else(|| RuntimeError::CapabilityNotFound {
                    capability: "get".to_string(),
                    entity: get.reference.entity_type.to_string(),
                })?,
        };

        let capability_template = parse_capability_template(&capability.mapping.template)?;

        if let CapabilityTemplate::View(vt) = &capability_template {
            let mut ephemeral = SessionMaterialization::new();
            let cache_ref = cache.unwrap_or(&mut ephemeral);
            let res = crate::view_execution::execute_view_get(
                self,
                vt.view.as_str(),
                get,
                cgs,
                cache_ref,
                mode,
                ambient,
            )
            .await?;
            let cached = res
                .entities
                .first()
                .cloned()
                .ok_or_else(|| RuntimeError::CacheError {
                    message: format!("composed view `{}` returned no entity row", vt.view),
                })?;
            return Ok((cached, res.source));
        }

        self.fetch_http_transport_get_decoded(
            get,
            cgs,
            mode,
            capability,
            &capability_template,
            inject_execute_session_env,
            cache,
        )
        .await
    }
}
