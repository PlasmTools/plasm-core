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
        // Satisfy from cache only when we already hold a detail payload (RA-11: skip after write).
        if let Some(entity) = mat.consult_complete_get(&get.reference) {
            let cached = entity.clone();
            stamp_get_capability_params(mat, cgs, get, ambient, &cached);
            return Ok(ExecutionResult {
                entities: vec![cached],
                count: 1,
                has_more: false,
                coverage: ResultCoverage::Complete,
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
                operations: OperationLedger::empty(),
            });
        }

        let (cached, source) = self
            .fetch_get_decoded(
                get,
                cgs,
                mode,
                get.capability_name.as_deref(),
                true,
                mat,
                ambient,
            )
            .await?;
        mat.insert(cached.clone())?;
        stamp_get_capability_params(mat, cgs, get, ambient, &cached);

        Ok(ExecutionResult {
            entities: vec![cached],
            count: 1,
            has_more: false,
            coverage: ResultCoverage::Complete,
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
            operations: OperationLedger::empty(),
        })
    }

    /// Like [`Self::execute_get`] for HTTP-backed GET capabilities used **inside** a composed `views:` DAG.
    pub(crate) async fn execute_get_for_view_dag(
        &self,
        get: &GetExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        required_relation: Option<&str>,
    ) -> Result<ExecutionResult, RuntimeError> {
        if let Some(entity) = mat.consult_complete_get(&get.reference).filter(|entity| {
            required_relation.is_none_or(|name| entity.relations.contains_key(name))
        }) {
            return Ok(ExecutionResult {
                entities: vec![entity.clone()],
                count: 1,
                has_more: false,
                coverage: ResultCoverage::Complete,
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
                operations: OperationLedger::empty(),
            });
        }

        let ambient = ViewAmbientContext::default();
        let request = ResolvedGet::resolve(
            get,
            cgs,
            get.capability_name.as_deref(),
            mat,
            &ambient,
            GetPurpose::Authored,
        )?;
        let capability = request.capability;
        if capability.derived.is_some() {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "derived Get '{}' cannot nest as an inner node inside a views: DAG",
                    capability.name
                ),
            });
        }
        let capability_template = compiled_capability_template(capability)?;
        if matches!(capability_template, CapabilityTemplate::View(_)) {
            return Err(RuntimeError::ConfigurationError {
                message:
                    "composed-view GET transport cannot nest as an inner node inside another views: DAG"
                        .into(),
            });
        }
        let (cached, source) = self
            .fetch_http_transport_get_decoded(&request, cgs, mode, &capability_template, true, mat)
            .await?;
        mat.insert(cached.clone())?;
        stamp_get_capability_params(mat, cgs, get, &ambient, &cached);

        Ok(ExecutionResult {
            entities: vec![cached],
            count: 1,
            has_more: false,
            coverage: ResultCoverage::Complete,
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
            operations: OperationLedger::empty(),
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
            auth: aux.auth.clone(),
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
        request: &ResolvedGet<'_>,
        cgs: &CGS,
        mode: ExecutionMode,
        capability_template: &CapabilityTemplate,
        validate_identity: bool,
        cache: &mut SessionMaterialization,
    ) -> Result<(CachedEntity, ExecutionSource), RuntimeError> {
        let get = request.get;
        let capability = request.capability;
        let env = &request.env;

        let compiled = compile_operation_dispatch(capability_template, env)?;
        if hydration_trace::active() {
            hydration_trace::emit(
                "dispatch",
                serde_json::json!({
                    "entity":get.reference.entity_type.as_str(), "capability":capability.name.as_str(),
                    "request_fingerprint":crate::replay::RequestFingerprint::from_operation(&compiled).to_hex()
                }),
            );
        }
        let (response, source) = with_dispatch_entity(
            Some(get.reference.entity_type.as_str()),
            self.execute_with_replay(&compiled, mode, Some(cache)),
        )
        .await?;
        if hydration_trace::active() {
            hydration_trace::emit(
                "transport_value",
                serde_json::json!({"source":source, "shape":hydration_trace::shape(&response)}),
            );
        }
        let response = self
            .apply_auxiliary_http_merge_response(
                capability_template,
                env,
                mode,
                response,
                Some(get.reference.entity_type.as_str()),
            )
            .await?;
        let response =
            narrow_http_graphql_response_for_entity_decode(capability_template, response, &env)?;
        let rid = cgs.get_entity(&get.reference.entity_type).and_then(|ent| {
            if ent.implicit_request_identity || ent.id_field == "url" {
                get.reference.simple_id().map(|id| id.as_str())
            } else {
                None
            }
        });
        let identity_ambient = decode_identity_ambient_for_ref(&get.reference, env);
        let decoder = create_entity_decoder_for_capability(
            &get.reference.entity_type,
            cgs,
            Some(capability.name.as_str()),
            None,
            rid,
            Some(&identity_ambient),
        );
        if hydration_trace::active() {
            hydration_trace::emit("decode_input", hydration_trace::shape(&response));
        }
        let decoded_entities = decode_entities_with_cgs(&decoder, &response, Some(cgs))?;
        if hydration_trace::active() {
            hydration_trace::emit(
                "decode_output",
                serde_json::json!({"rows":decoded_entities.len(),
                "identity_matches":decoded_entities.first().map(|e| e.reference == get.reference),
                "fields":decoded_entities.first().map(|e| e.fields.keys().map(|k| k.as_str()).collect::<Vec<_>>())}),
            );
        }

        let decoded = decoded_entities
            .first()
            .ok_or_else(|| RuntimeError::CacheError {
                message: format!("zero rows — Entity not found: {}", get.reference),
            })?;

        if validate_identity
            && !get.reference.primary_slot_str().is_empty()
            && decoded.reference != get.reference
        {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "view GET identity mismatch: requested {}, returned {}",
                    get.reference, decoded.reference
                ),
            });
        }
        let timestamp = current_timestamp();
        let cached = cache_decoded_entity_tree(
            cache,
            decoded.clone(),
            timestamp,
            EntityCompleteness::Complete,
        )?;
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
        cache: &mut SessionMaterialization,
        ambient: &ViewAmbientContext,
    ) -> Result<(CachedEntity, ExecutionSource), RuntimeError> {
        let purpose = if inject_execute_session_env {
            GetPurpose::Authored
        } else {
            GetPurpose::Hydration
        };
        let request = ResolvedGet::resolve(get, cgs, hydrate_capability, cache, ambient, purpose)?;
        let capability = request.capability;
        let ambient = &request.ambient;

        if let Some(plan) = capability.derived.as_ref() {
            return crate::derived_get::execute_derived_get(
                self, plan, get, cgs, cache, mode, ambient,
            )
            .await;
        }

        let capability_template = compiled_capability_template(capability)?;

        if let CapabilityTemplate::View(vt) = &capability_template {
            let res = crate::view_execution::execute_view_get(
                self,
                vt.view.as_str(),
                get,
                cgs,
                cache,
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
            &request,
            cgs,
            mode,
            &capability_template,
            false,
            cache,
        )
        .await
    }
}
