//! Create, delete, and invoke expression execution.

use super::*;

fn execution_mode_label(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Live => "live",
        ExecutionMode::Replay => "replay",
        ExecutionMode::Hybrid => "hybrid",
    }
}

/// Materialization and bindings held by the caller while dispatch is paused.
struct RetainedMutationFrame<'a> {
    materialization: &'a SessionMaterialization,
    env: &'a CmlEnv,
}

fn retained_http_request_capacity(operation: &CompiledOperation) -> usize {
    let request = match operation {
        CompiledOperation::Http(request) | CompiledOperation::GraphQl(request) => request,
        // A classified HTTP rejection must have an inspectable HTTP request.
        _ => return usize::MAX,
    };
    let mut bytes = request.path.capacity();
    for value in [&request.query, &request.body, &request.headers]
        .into_iter()
        .flatten()
    {
        bytes = bytes.saturating_add(crate::materialization::value_retention_capacity(value, 0));
    }
    if let Some(multipart) = &request.multipart {
        bytes = bytes.saturating_add(
            multipart
                .parts
                .capacity()
                .saturating_mul(std::mem::size_of::<plasm_compile::CompiledMultipartPart>()),
        );
        for part in &multipart.parts {
            bytes = bytes
                .saturating_add(part.name.capacity())
                .saturating_add(part.file_name.as_ref().map_or(0, String::capacity))
                .saturating_add(part.content_type.as_ref().map_or(0, String::capacity))
                .saturating_add(crate::materialization::value_retention_capacity(
                    &part.content,
                    0,
                ));
        }
    }
    if let Some(credential) = &request.credential {
        bytes = bytes
            .saturating_add(credential.slot.capacity())
            .saturating_add(credential.reference.capacity())
            .saturating_add(crate::materialization::json_retention_capacity(
                &credential.resource,
                0,
            ));
    }
    bytes
}

fn retained_rejection_capacity(
    response: &crate::HttpStatusFailure,
    body: &Option<Box<serde_json::Value>>,
) -> usize {
    response
        .method
        .capacity()
        .saturating_add(response.url.capacity())
        .saturating_add(response.detail.capacity())
        .saturating_add(
            response
                .mutation_request_fingerprint
                .as_ref()
                .map_or(0, String::capacity),
        )
        .saturating_add(
            response
                .authorization
                .scheme
                .as_ref()
                .map_or(0, String::capacity),
        )
        .saturating_add(
            response
                .authorization
                .token_tail
                .as_ref()
                .map_or(0, String::capacity),
        )
        .saturating_add(
            response
                .login_token_tail
                .as_ref()
                .map_or(0, String::capacity),
        )
        .saturating_add(body.as_ref().map_or(0, |value| {
            crate::materialization::json_retention_capacity(value, 0)
        }))
}

/// Copy only a catalog-classified HTTP rejection into public checkpoint
/// evidence. The original error remains owned by the suspended dispatch.
fn checkpoint_failure(
    error: &RuntimeError,
    attempt: Vec<crate::MutationDispatch>,
) -> Option<crate::ExecutionFailure> {
    let [dispatch] = attempt.as_slice() else {
        return None;
    };
    if dispatch.status != crate::MutationDispatchStatus::RejectedWithoutEffect {
        return None;
    }
    let RuntimeError::RequestError {
        source: crate::RequestFailure::HttpStatus(response),
        attempts,
        status,
        body,
    } = error
    else {
        return None;
    };
    let origin = dispatch
        .resolved_effect
        .as_ref()
        .and_then(|effect| effect.origin.clone());
    let copied = RuntimeError::RequestError {
        source: crate::RequestFailure::HttpStatus(response.clone()),
        attempts: *attempts,
        status: *status,
        body: body.clone(),
    };
    let mut failure = crate::ExecutionFailure::from(copied).with_dispatches(attempt);
    if let Some(origin) = origin {
        failure = failure.at(origin.scope_path.join("/"), origin.occurrence_path);
    }
    Some(failure)
}

impl ExecutionEngine {
    /// Dispatch one already-compiled mutation under its original entity,
    /// operation and optional request-identity scopes.
    async fn dispatch_compiled_mutation(
        &self,
        compiled: &CompiledOperation,
        entity: &str,
        operation: OperationIdentity,
        capability: &CapabilitySchema,
        identity: Option<&super::request_identity::RequestIdentity>,
        retained: RetainedMutationFrame<'_>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let RetainedMutationFrame {
            materialization,
            env: retained_env,
        } = retained;
        let control = super::effect_continuation::EffectContinuation::current().filter(|_| {
            super::effect_continuation::EffectContinuation::supports_capability(capability)
        });
        // A host with opaque credentials cannot prove that a later dispatch is
        // in the same account. Such an operation executes normally, but cannot
        // acquire a resume capability from this mechanism.
        let pinned = if control.is_some() && identity.is_none() {
            match compiled {
                CompiledOperation::Http(_) | CompiledOperation::GraphQl(_) => {
                    match self.prepare_request_identity(compiled).await {
                        Ok(identity) => Some(identity),
                        Err(RuntimeError::RequestIdentityAuthOpaque) => None,
                        Err(error) => return Err(error),
                    }
                }
                _ => None,
            }
        } else {
            None
        };
        let journal = if control.is_some() && (identity.is_some() || pinned.is_some()) {
            super::mutation_evidence::MutationJournal::current()
        } else {
            None
        };
        loop {
            let start = journal.as_ref().map(|journal| journal.len());
            let dispatch = with_dispatch_entity(
                Some(entity),
                super::mutation_evidence::with_mutation_identity(
                    operation.clone(),
                    self.execute_operation_full(compiled),
                ),
            );
            let result = match (identity, pinned.as_ref()) {
                (Some(identity), _) => identity.scope(dispatch).await,
                (None, Some(identity)) => identity.scope_auth(dispatch).await,
                (None, None) => dispatch.await,
            };
            if result.is_ok() {
                if let Some(control) = &control {
                    control.scalar_dispatch_completed();
                }
            }
            if let (Err(error), Some(control), Some(journal), Some(start)) =
                (&result, control.as_ref(), journal.as_ref(), start)
            {
                let attempt =
                    journal.classify_attempt(start, error, &capability.no_effect_rejections);
                if let Some(failure) = checkpoint_failure(error, attempt.clone()) {
                    let Some(original_identity) = identity.or(pinned.as_ref()) else {
                        return result;
                    };
                    let RuntimeError::RequestError {
                        source: crate::RequestFailure::HttpStatus(response),
                        attempts,
                        status,
                        body,
                    } = error
                    else {
                        return result;
                    };
                    // The request, caller bindings, materialization, pinned
                    // authority and rejection remain live across the pause.
                    // If they do not fit, return the original typed rejection:
                    // no continuation capability or retry is issued.
                    let mut retained_dispatch =
                        match super::effect_continuation::EffectContinuation::reserve_retained(&(
                            compiled,
                            entity,
                            &operation,
                            retained_env,
                            original_identity.retention_auth(),
                            materialization.retention_view(),
                            (
                                &response.mutation_request_fingerprint,
                                &response.method,
                                &response.url,
                                response.status,
                                &response.detail,
                                response.empty_body,
                                response.authorization.present,
                                &response.authorization.scheme,
                                &response.authorization.token_tail,
                                &response.login_token_tail,
                                response.retry_budget_exhausted,
                                attempts,
                                status,
                                body,
                            ),
                        )) {
                            Ok(reservation) => reservation,
                            Err(_) => return result,
                        };
                    if retained_dispatch
                        .add_capacity_bytes(
                            materialization
                                .retention_capacity_bytes()
                                .saturating_add(retained_http_request_capacity(compiled))
                                .saturating_add(crate::materialization::value_map_capacity(
                                    retained_env,
                                    0,
                                ))
                                .saturating_add(operation.entry_id.capacity())
                                .saturating_add(operation.capability.capacity())
                                .saturating_add(original_identity.retention_auth_capacity())
                                .saturating_add(retained_rejection_capacity(response, body)),
                        )
                        .is_err()
                    {
                        return result;
                    }
                    let _retained_dispatch = retained_dispatch;
                    let _retained_journal = match journal.freeze_retention(start, &attempt) {
                        Ok(reservation) => reservation,
                        Err(_) => return result,
                    };
                    match control.checkpoint(failure).await {
                        Ok(()) => {
                            // The original request and auth remain pinned, but
                            // the grant must still resolve to the same scope.
                            let original = identity.or(pinned.as_ref());
                            let reason = match (original, self.prepare_request_identity(compiled).await) {
                                (Some(original), Ok(current)) if original.same_scope(&current) => {
                                    match control.check_active() {
                                        Ok(()) => continue,
                                        Err(super::effect_continuation::EffectContinuationError::Cancelled) => return Err(RuntimeError::Cancelled),
                                        Err(_) => return result,
                                    }
                                }
                                (Some(_), Ok(_)) | (None, Ok(_)) => crate::error::ContinuationAuthorizationReason::ScopeChanged,
                                (_, Err(error)) => crate::error::ContinuationAuthorizationReason::RevalidationFailed(Box::new(error)),
                            };
                            return match result {
                                Err(rejection) => Err(RuntimeError::ContinuationAuthorization {
                                    reason,
                                    rejection: Box::new(rejection),
                                }),
                                Ok(response) => Ok(response),
                            };
                        }
                        Err(super::effect_continuation::EffectContinuationError::Cancelled) => {
                            return Err(RuntimeError::Cancelled);
                        }
                        Err(_) => {}
                    }
                }
            }
            return result;
        }
    }

    /// Execute a create expression (no target ID — creates a new resource)
    pub(crate) async fn execute_create(
        &self,
        create: &plasm_core::CreateExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        let (capability, mut env) = super::mutation_prepare::create(create, cgs)?;
        let capability_template = compiled_capability_template(capability)?;

        apply_preflight_steps(self, capability, cgs, mat, mode, &mut env, None, true).await?;

        if crate::workflow_reconcile::should_skip_write_after_preflight(&env) {
            return crate::workflow_reconcile::skipped_write_result(
                create.entity.as_str(),
                plasm_core::collection_codec::CollectionIdentity::for_expression(
                    cgs,
                    create,
                    mat.graph.stats().version,
                )?,
            );
        }

        merge_plasm_execute_session_env(&mut env);

        let compiled = compile_operation_dispatch(&capability_template, &env)?;

        match mode {
            ExecutionMode::Live => {
                ensure_mutating_operation(&compiled, "create")?;
                let http_res = self
                    .dispatch_compiled_mutation(
                        &compiled,
                        create.entity.as_str(),
                        OperationIdentity {
                            entry_id: create.catalog_entry_id.as_deref().unwrap_or("").into(),
                            capability: create.capability.to_string(),
                        },
                        capability,
                        None,
                        RetainedMutationFrame {
                            materialization: mat,
                            env: &env,
                        },
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
                                &Value::Object(env.clone()),
                            )
                            .await;
                    }
                };
                let response = narrow_http_graphql_response_for_entity_decode(
                    &capability_template,
                    response,
                    &env,
                )?;
                let identity_ambient = cml_env_to_identity_strings(&env);
                let decoder = super::entity_decoder::mutating_capability_output_decoder(
                    create.entity.as_str(),
                    create.capability.as_str(),
                    cgs,
                    &identity_ambient,
                    None,
                );
                let decoded =
                    plasm_compile::decode_output_with_cgs(&decoder, &response, Some(cgs))?;
                super::entity_decoder::validate_declared_output_cardinality(
                    capability,
                    decoded.len(),
                )?;

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
                    collection: ExecutionCollection::observe_for(
                        cgs,
                        create,
                        mat.graph.stats().version,
                        entities,
                        plasm_core::collection_codec::Observation::ExactOutput { decoded: count },
                    )?,
                    has_more: false,
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
                mode: format!("create with {}", execution_mode_label(mode)),
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
        let (capability, _, mut env) = super::mutation_prepare::targeted(delete, cgs, mat)?;
        let capability_template = compiled_capability_template(capability)?;

        merge_plasm_execute_session_env(&mut env);

        let compiled = compile_operation_dispatch(&capability_template, &env)?;
        // Delete needs neither the template nor its bindings after compilation.
        drop(env);
        drop(capability_template);

        match mode {
            ExecutionMode::Live => {
                ensure_mutating_operation(&compiled, "delete")?;
                let retained_env = CmlEnv::new();
                let (response, _) = self
                    .dispatch_compiled_mutation(
                        &compiled,
                        delete.target.entity_type.as_str(),
                        OperationIdentity {
                            entry_id: delete.catalog_entry_id.as_deref().unwrap_or("").into(),
                            capability: delete.capability.to_string(),
                        },
                        capability,
                        None,
                        RetainedMutationFrame {
                            materialization: mat,
                            env: &retained_env,
                        },
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
                    collection: ExecutionCollection::observe_for(
                        cgs,
                        delete,
                        mat.graph.stats().version,
                        vec![],
                        plasm_core::collection_codec::Observation::ExactOutput { decoded: 0 },
                    )?,
                    has_more: false,
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
                mode: format!("delete with {}", execution_mode_label(mode)),
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
        let (capability, _, mut env) = super::mutation_prepare::invoke(invoke, cgs, mat)?;
        let capability_template = compiled_capability_template(capability)?;

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
            return crate::workflow_reconcile::skipped_write_result(
                invoke.target.entity_type.as_str(),
                plasm_core::collection_codec::CollectionIdentity::for_expression(
                    cgs,
                    invoke,
                    mat.graph.stats().version,
                )?,
            );
        }

        merge_plasm_execute_session_env(&mut env);

        let compiled = compile_operation_dispatch(&capability_template, &env)?;

        match mode {
            ExecutionMode::Live => {
                ensure_mutating_operation(&compiled, "invoke")?;
                let request_identity = if !capability.requires_receiver() {
                    self.request_identity_for(
                        cgs,
                        invoke.target.entity_type.as_str(),
                        &capability_template,
                        &compiled,
                    )
                    .await?
                } else {
                    None
                };
                let request_key = request_identity.as_ref().map(|identity| identity.key());
                let http_res = self
                    .dispatch_compiled_mutation(
                        &compiled,
                        invoke.target.entity_type.as_str(),
                        OperationIdentity {
                            entry_id: invoke.catalog_entry_id.as_deref().unwrap_or("").into(),
                            capability: invoke.capability.to_string(),
                        },
                        capability,
                        request_identity.as_ref(),
                        RetainedMutationFrame {
                            materialization: mat,
                            env: &env,
                        },
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
                                &Value::Object(env.clone()),
                            )
                            .await;
                    }
                };
                let response = narrow_http_graphql_response_for_entity_decode(
                    &capability_template,
                    response,
                    &env,
                )?;

                // Decode the response as the capability's declared entity type.
                // When an action returns a projection of the same entity (e.g.
                // page_get_markdown returns {id, markdown, truncated} for a Page),
                // the decoder extracts only the fields present in the response, and
                // the cache's additive merge preserves existing fields from other
                // projections (e.g. url, timestamps from page_get).
                let rid = request_key
                    .as_deref()
                    .or_else(|| invoke.target.simple_id().map(|s| s.as_str()));
                let identity_ambient = cml_env_to_identity_strings(&env);
                let decoder = super::entity_decoder::mutating_capability_output_decoder(
                    invoke.target.entity_type.as_str(),
                    invoke.capability.as_str(),
                    cgs,
                    &identity_ambient,
                    rid,
                );
                let decoded = if capability.provides.is_empty()
                    && capability.declared_entity_output().is_none()
                    && !matches!(
                        capability
                            .output_schema
                            .as_ref()
                            .map(|output| &output.output_type),
                        Some(plasm_core::OutputType::EntityUnion { .. })
                    ) {
                    // True side-effect Actions may return empty/opaque bodies.
                    plasm_compile::decode_output_with_cgs(&decoder, &response, Some(cgs))
                        .unwrap_or_default()
                } else {
                    // Action-with-`provides` must materialize catalog-qualified rows (e.g. AuthSession
                    // access_token) for downstream hole fill / CML env — never swallow decode failure.
                    plasm_compile::decode_output_with_cgs(&decoder, &response, Some(cgs))?
                };
                super::entity_decoder::validate_declared_output_cardinality(
                    capability,
                    decoded.len(),
                )?;
                if let Some(identity) = &request_identity {
                    identity.validate_rows(invoke.target.entity_type.as_str(), decoded.len())?;
                }

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
                    collection: ExecutionCollection::observe_for(
                        cgs,
                        invoke,
                        mat.graph.stats().version,
                        entities,
                        plasm_core::collection_codec::Observation::ExactOutput { decoded: count },
                    )?,
                    has_more: false,
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
                mode: format!("invoke with {} mode", execution_mode_label(mode)),
            }),
        }
    }
}

#[cfg(test)]
mod continuation_tests {
    use super::*;
    use crate::execution::effect_continuation::{EffectContinuation, EffectContinuationError};
    use crate::http_transport::{HttpTransport, TransportAuthScope};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;
    use uuid::Uuid;

    #[derive(Default)]
    struct Secrets {
        changed: AtomicBool,
        missing: AtomicBool,
        oversized: AtomicBool,
        reads: AtomicUsize,
    }

    impl crate::auth::SecretProvider for Secrets {
        fn get_secret<'a>(
            &'a self,
            _key: &'a str,
        ) -> futures_util::future::BoxFuture<'a, Option<String>> {
            Box::pin(async move {
                self.reads.fetch_add(1, Ordering::SeqCst);
                if self.missing.load(Ordering::SeqCst) {
                    return None;
                }
                if self.oversized.load(Ordering::SeqCst) {
                    return Some(
                        "x".repeat(super::super::effect_continuation::MAX_EFFECT_SELECTION_BYTES),
                    );
                }
                Some(if self.changed.load(Ordering::SeqCst) {
                    "second-token".into()
                } else {
                    "first-token".into()
                })
            })
        }
    }

    struct Transport {
        statuses: Vec<u16>,
        opaque: bool,
        requests: Mutex<Vec<(CompiledRequest, Option<crate::auth::ResolvedAuth>)>>,
    }

    #[async_trait]
    impl HttpTransport for Transport {
        fn auth_scope(
            &self,
            _: &CompiledRequest,
            _: Option<&crate::auth::ResolvedAuth>,
        ) -> Result<TransportAuthScope, RuntimeError> {
            Ok(if self.opaque {
                TransportAuthScope::Opaque
            } else {
                TransportAuthScope::EngineVisible
            })
        }

        async fn send_compiled_http(
            &self,
            _: &str,
            request: &CompiledRequest,
            auth: Option<crate::auth::ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            let index = {
                let mut requests = self.requests.lock().unwrap();
                let index = requests.len();
                requests.push((request.clone(), auth));
                index
            };
            let status = self.statuses.get(index).copied().unwrap_or(200);
            crate::execution::record_transport_response_received();
            if status != 200 {
                return Err(RuntimeError::RequestError {
                    source: crate::RequestFailure::HttpStatus(Box::new(
                        crate::HttpStatusFailure::without_request(status, "fixture 422".into()),
                    )),
                    attempts: 1,
                    status: Some(status),
                    body: None,
                });
            }
            Ok((serde_json::json!({"ok": true}), None))
        }

        async fn get_json_absolute(
            &self,
            _: &str,
            _: Option<crate::auth::ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            panic!("unexpected absolute request")
        }
    }

    fn capability() -> CapabilitySchema {
        let path = std::path::PathBuf::from(crate::test_manifest_dir())
            .join("../../fixtures/schemas/partial_write_relation_matrix");
        let cgs = plasm_core::loader::load_schema_dir(&path).unwrap();
        let mut capability = cgs.capabilities["pwexpense_create"].clone();
        capability
            .no_effect_rejections
            .push(plasm_core::schema::NoEffectRejection {
                status: 422,
                detail: plasm_core::schema::RejectionDetail::Exact {
                    value: "fixture 422".into(),
                },
            });
        capability
    }

    fn empty_env() -> &'static CmlEnv {
        static EMPTY: std::sync::OnceLock<CmlEnv> = std::sync::OnceLock::new();
        EMPTY.get_or_init(CmlEnv::new)
    }

    #[tokio::test]
    async fn oversized_retained_mutation_environment_cannot_create_a_checkpoint() {
        use crate::execution::effect_continuation::MAX_EFFECT_SELECTION_BYTES;
        let mut spare = String::with_capacity(MAX_EFFECT_SELECTION_BYTES + 1);
        spare.push('x');
        for value in ["x".repeat(MAX_EFFECT_SELECTION_BYTES), spare] {
            let transport = Arc::new(Transport {
                statuses: vec![422],
                opaque: false,
                requests: Mutex::new(Vec::new()),
            });
            let engine = ExecutionEngine::new_with_transport(
                ExecutionConfig {
                    base_url: Some("https://fixture.test".into()),
                    ..Default::default()
                },
                Arc::clone(&transport) as Arc<dyn HttpTransport>,
                None,
            );
            let compiled = request();
            let graph = SessionMaterialization::new();
            let env = CmlEnv::from([("unused".into(), Value::String(value))]);
            let capability = capability();
            let journal = super::super::mutation_evidence::MutationJournal::default();
            let (control, mut pauses) =
                EffectContinuation::new_native(Duration::from_secs(5)).unwrap();
            let run = control.scope(journal.scope(engine.dispatch_compiled_mutation(
                &compiled,
                "PwExpense",
                OperationIdentity {
                    entry_id: "matrix".into(),
                    capability: "pwexpense_create".into(),
                },
                &capability,
                None,
                RetainedMutationFrame {
                    materialization: &graph,
                    env: &env,
                },
            )));
            tokio::pin!(run);
            let result = tokio::select! {
                result = &mut run => result,
                pause = pauses.recv() => panic!("oversized retained environment admitted checkpoint: {}", pause.is_some()),
            };
            assert!(matches!(
                result,
                Err(RuntimeError::RequestError {
                    status: Some(422),
                    ..
                })
            ));
            assert_eq!(transport.requests.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn oversized_operation_identity_capacity_cannot_create_a_checkpoint() {
        use crate::execution::effect_continuation::MAX_EFFECT_SELECTION_BYTES;
        let transport = Arc::new(Transport {
            statuses: vec![422],
            opaque: false,
            requests: Mutex::new(Vec::new()),
        });
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("https://fixture.test".into()),
                ..Default::default()
            },
            Arc::clone(&transport) as Arc<dyn HttpTransport>,
            None,
        );
        let compiled = request();
        let graph = SessionMaterialization::new();
        let capability = capability();
        let mut entry_id = String::with_capacity(MAX_EFFECT_SELECTION_BYTES + 1);
        entry_id.push_str("matrix");
        let journal = super::super::mutation_evidence::MutationJournal::default();
        let (control, mut pauses) = EffectContinuation::new_native(Duration::from_secs(5)).unwrap();
        let run = control.scope(journal.scope(engine.dispatch_compiled_mutation(
            &compiled,
            "PwExpense",
            OperationIdentity {
                entry_id,
                capability: "pwexpense_create".into(),
            },
            &capability,
            None,
            RetainedMutationFrame {
                materialization: &graph,
                env: empty_env(),
            },
        )));
        tokio::pin!(run);
        let result = tokio::select! {
            result = &mut run => result,
            pause = pauses.recv() => panic!("oversized operation identity admitted checkpoint: {}", pause.is_some()),
        };
        assert!(matches!(
            result,
            Err(RuntimeError::RequestError {
                status: Some(422),
                ..
            })
        ));
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
    }

    fn request() -> CompiledOperation {
        CompiledOperation::Http(CompiledRequest {
            method: plasm_compile::HttpMethod::Post,
            path: "/fixture".into(),
            query: None,
            body: Some(Value::Object(indexmap::IndexMap::from([(
                "description".into(),
                Value::String("original".into()),
            )]))),
            body_format: plasm_compile::HttpBodyFormat::Json,
            multipart: None,
            headers: None,
            credential: None,
        })
    }

    #[test]
    fn rejection_retention_counts_spare_detail_and_body_capacity() {
        use crate::execution::effect_continuation::MAX_EFFECT_SELECTION_BYTES;
        let mut failure = crate::HttpStatusFailure::without_request(422, "fixture 422".into());
        failure.detail.reserve(MAX_EFFECT_SELECTION_BYTES + 1);
        assert!(retained_rejection_capacity(&failure, &None) > MAX_EFFECT_SELECTION_BYTES);
        let body = Some(Box::new(serde_json::Value::Array(Vec::with_capacity(
            MAX_EFFECT_SELECTION_BYTES / std::mem::size_of::<serde_json::Value>() + 1,
        ))));
        assert!(
            retained_rejection_capacity(
                &crate::HttpStatusFailure::without_request(422, "fixture 422".into()),
                &body
            ) > MAX_EFFECT_SELECTION_BYTES
        );
        let mut request = request();
        let CompiledOperation::Http(http) = &mut request else {
            unreachable!()
        };
        http.multipart = Some(plasm_compile::CompiledMultipartBody { parts: Vec::new() });
        assert_ne!(retained_http_request_capacity(&request), usize::MAX);
    }

    #[tokio::test]
    async fn oversized_rejected_request_capacity_cannot_create_a_checkpoint() {
        use crate::execution::effect_continuation::MAX_EFFECT_SELECTION_BYTES;
        let transport = Arc::new(Transport {
            statuses: vec![422],
            opaque: false,
            requests: Mutex::new(Vec::new()),
        });
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("https://fixture.test".into()),
                ..Default::default()
            },
            Arc::clone(&transport) as Arc<dyn HttpTransport>,
            None,
        );
        let mut compiled = request();
        let CompiledOperation::Http(request) = &mut compiled else {
            unreachable!()
        };
        request.path.reserve(MAX_EFFECT_SELECTION_BYTES + 1);
        let graph = SessionMaterialization::new();
        let journal = super::super::mutation_evidence::MutationJournal::default();
        let (control, mut pauses) = EffectContinuation::new_native(Duration::from_secs(5)).unwrap();
        let identity = OperationIdentity {
            entry_id: "matrix".into(),
            capability: "pwexpense_create".into(),
        };
        let capability = capability();
        let run = control.scope(journal.scope(engine.dispatch_compiled_mutation(
            &compiled,
            "PwExpense",
            identity,
            &capability,
            None,
            RetainedMutationFrame {
                materialization: &graph,
                env: empty_env(),
            },
        )));
        tokio::pin!(run);
        let result = tokio::select! {
            result = &mut run => result,
            _ = pauses.recv() => panic!("oversized retained request admitted a checkpoint"),
        };
        assert!(matches!(
            result,
            Err(RuntimeError::RequestError {
                status: Some(422),
                ..
            })
        ));
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn oversized_rejected_request_cannot_create_a_checkpoint() {
        use crate::execution::effect_continuation::MAX_EFFECT_SELECTION_BYTES;
        let transport = Arc::new(Transport {
            statuses: vec![422],
            opaque: false,
            requests: Mutex::new(Vec::new()),
        });
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("https://fixture.test".into()),
                ..Default::default()
            },
            Arc::clone(&transport) as Arc<dyn HttpTransport>,
            None,
        );
        let mut compiled = request();
        let CompiledOperation::Http(request) = &mut compiled else {
            unreachable!()
        };
        request.body = Some(Value::String("x".repeat(MAX_EFFECT_SELECTION_BYTES)));
        let graph = SessionMaterialization::new();
        let journal = super::super::mutation_evidence::MutationJournal::default();
        let (control, mut pauses) = EffectContinuation::new_native(Duration::from_secs(5)).unwrap();
        let result = control
            .scope(journal.scope(engine.dispatch_compiled_mutation(
                &compiled,
                "PwExpense",
                OperationIdentity {
                    entry_id: "matrix".into(),
                    capability: "pwexpense_create".into(),
                },
                &capability(),
                None,
                RetainedMutationFrame {
                    materialization: &graph,
                    env: empty_env(),
                },
            )))
            .await;
        assert!(matches!(
            result,
            Err(RuntimeError::RequestError {
                status: Some(422),
                ..
            })
        ));
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
        assert!(!matches!(
            tokio::time::timeout(Duration::from_millis(20), pauses.recv()).await,
            Ok(Some(_))
        ));
    }

    #[tokio::test]
    async fn oversized_private_authority_cannot_create_a_checkpoint() {
        let secrets = Arc::new(Secrets::default());
        secrets.oversized.store(true, Ordering::SeqCst);
        let transport = Arc::new(Transport {
            statuses: vec![422],
            opaque: false,
            requests: Mutex::new(Vec::new()),
        });
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("https://fixture.test".into()),
                ..Default::default()
            },
            Arc::clone(&transport) as Arc<dyn HttpTransport>,
            Some(crate::auth::AuthResolver::new(
                plasm_core::AuthScheme::BearerToken {
                    env: Some("fixture".into()),
                    hosted_kv: None,
                    optional_env: false,
                },
                Arc::clone(&secrets) as Arc<dyn crate::auth::SecretProvider>,
            )),
        );
        let compiled = request();
        let graph = SessionMaterialization::new();
        let journal = super::super::mutation_evidence::MutationJournal::default();
        let (control, mut pauses) = EffectContinuation::new_native(Duration::from_secs(5)).unwrap();
        let result = control
            .scope(journal.scope(engine.dispatch_compiled_mutation(
                &compiled,
                "PwExpense",
                OperationIdentity {
                    entry_id: "matrix".into(),
                    capability: "pwexpense_create".into(),
                },
                &capability(),
                None,
                RetainedMutationFrame {
                    materialization: &graph,
                    env: empty_env(),
                },
            )))
            .await;
        assert!(matches!(
            result,
            Err(RuntimeError::RequestError {
                status: Some(422),
                ..
            })
        ));
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
        assert!(!matches!(
            tokio::time::timeout(Duration::from_millis(20), pauses.recv()).await,
            Ok(Some(_))
        ));
    }

    #[tokio::test]
    async fn lazy_fork_base_capacity_cannot_be_pinned_by_a_checkpoint() {
        use crate::execution::effect_continuation::MAX_EFFECT_SELECTION_BYTES;
        use plasm_core::collection_codec::{
            CollectionCodec, CollectionIdentity, Observation, RecordingCodec, Transform,
        };
        use plasm_core::row_contract::RelationMembership;
        let mut hidden = String::with_capacity(MAX_EFFECT_SELECTION_BYTES + 1);
        hidden.push('x');
        let codec = RecordingCodec::new();
        let identity = CollectionIdentity::for_untyped_observation(&"fork-base-capacity").unwrap();
        let all = codec
            .record(
                identity.clone(),
                vec![
                    plasm_core::Ref::new("Child", "small"),
                    plasm_core::Ref::new("Child", hidden),
                ],
                Observation::Literal,
            )
            .unwrap();
        let selected = codec
            .derive(
                identity.derived(&"take-one").unwrap(),
                &[&all],
                Transform::Take(1),
            )
            .unwrap();
        let mut entity = crate::CachedEntity::new(plasm_core::Ref::new("Parent", "one"), 0);
        entity
            .relations
            .insert("children".into(), RelationMembership::from_record(selected));
        let mut graph = crate::GraphCache::new();
        graph.overwrite(entity).unwrap();
        let mut branch = graph.fork_for_branch();
        branch
            .overwrite(crate::CachedEntity::new(
                plasm_core::Ref::new("Parent", "one"),
                0,
            ))
            .unwrap();
        let graph = SessionMaterialization {
            graph: branch,
            ..Default::default()
        };
        let transport = Arc::new(Transport {
            statuses: vec![422],
            opaque: false,
            requests: Mutex::new(Vec::new()),
        });
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("https://fixture.test".into()),
                ..Default::default()
            },
            Arc::clone(&transport) as Arc<dyn HttpTransport>,
            None,
        );
        let compiled = request();
        let journal = super::super::mutation_evidence::MutationJournal::default();
        let (control, mut pauses) = EffectContinuation::new_native(Duration::from_secs(5)).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            control.scope(journal.scope(engine.dispatch_compiled_mutation(
                &compiled,
                "PwExpense",
                OperationIdentity {
                    entry_id: "matrix".into(),
                    capability: "pwexpense_create".into(),
                },
                &capability(),
                None,
                RetainedMutationFrame {
                    materialization: &graph,
                    env: empty_env(),
                },
            ))),
        )
        .await
        .expect("oversized fork base must not pause");
        assert!(matches!(
            result,
            Err(RuntimeError::RequestError {
                status: Some(422),
                ..
            })
        ));
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
        assert!(!matches!(
            tokio::time::timeout(Duration::from_millis(20), pauses.recv()).await,
            Ok(Some(_))
        ));
    }

    #[tokio::test]
    async fn hidden_relation_backing_cannot_be_pinned_by_a_checkpoint() {
        use crate::execution::effect_continuation::MAX_EFFECT_SELECTION_BYTES;
        use plasm_core::collection_codec::{
            CollectionCodec, CollectionIdentity, Observation, RecordingCodec, Transform,
        };
        use plasm_core::row_contract::RelationMembership;

        let codec = RecordingCodec::new();
        let identity = CollectionIdentity::for_untyped_observation(&"hidden-relation").unwrap();
        let all = codec
            .record(
                identity.clone(),
                vec![
                    plasm_core::Ref::new("Child", "small"),
                    plasm_core::Ref::new("Child", "x".repeat(MAX_EFFECT_SELECTION_BYTES)),
                ],
                Observation::Literal,
            )
            .unwrap();
        let selected = codec
            .derive(
                identity.derived(&"take-one").unwrap(),
                &[&all],
                Transform::Take(1),
            )
            .unwrap();
        let mut entity = crate::CachedEntity::new(plasm_core::Ref::new("Parent", "one"), 0);
        entity
            .relations
            .insert("children".into(), RelationMembership::from_record(selected));
        let mut graph = crate::GraphCache::new();
        graph.overwrite(entity).unwrap();
        let graph = SessionMaterialization {
            graph: graph.fork_for_branch(),
            ..Default::default()
        };
        let transport = Arc::new(Transport {
            statuses: vec![422],
            opaque: false,
            requests: Mutex::new(Vec::new()),
        });
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("https://fixture.test".into()),
                ..Default::default()
            },
            Arc::clone(&transport) as Arc<dyn HttpTransport>,
            None,
        );
        let journal = super::super::mutation_evidence::MutationJournal::default();
        let (control, mut pauses) = EffectContinuation::new_native(Duration::from_secs(5)).unwrap();
        let compiled = request();
        let result = control
            .scope(journal.scope(engine.dispatch_compiled_mutation(
                &compiled,
                "PwExpense",
                OperationIdentity {
                    entry_id: "matrix".into(),
                    capability: "pwexpense_create".into(),
                },
                &capability(),
                None,
                RetainedMutationFrame {
                    materialization: &graph,
                    env: empty_env(),
                },
            )))
            .await;
        assert!(matches!(
            result,
            Err(RuntimeError::RequestError {
                status: Some(422),
                ..
            })
        ));
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
        assert!(!matches!(
            tokio::time::timeout(Duration::from_millis(20), pauses.recv()).await,
            Ok(Some(_))
        ));
    }

    #[tokio::test]
    async fn oversized_unrelated_branch_graph_cannot_be_pinned_by_a_checkpoint() {
        use crate::execution::effect_continuation::MAX_EFFECT_SELECTION_BYTES;
        let transport = Arc::new(Transport {
            statuses: vec![422],
            opaque: false,
            requests: Mutex::new(Vec::new()),
        });
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("https://fixture.test".into()),
                ..Default::default()
            },
            Arc::clone(&transport) as Arc<dyn HttpTransport>,
            None,
        );
        let mut graph = crate::GraphCache::new();
        let unrelated_id = "x".repeat(MAX_EFFECT_SELECTION_BYTES);
        graph
            .insert(crate::CachedEntity::new(
                plasm_core::Ref::new("Unrelated", &unrelated_id),
                0,
            ))
            .unwrap();
        let graph = SessionMaterialization {
            graph: graph.fork_for_branch(),
            ..Default::default()
        };
        let compiled = request();
        let journal = super::super::mutation_evidence::MutationJournal::default();
        let (control, mut pauses) = EffectContinuation::new_native(Duration::from_secs(5)).unwrap();
        let result = control
            .scope(journal.scope(engine.dispatch_compiled_mutation(
                &compiled,
                "PwExpense",
                OperationIdentity {
                    entry_id: "matrix".into(),
                    capability: "pwexpense_create".into(),
                },
                &capability(),
                None,
                RetainedMutationFrame {
                    materialization: &graph,
                    env: empty_env(),
                },
            )))
            .await;
        assert!(matches!(
            result,
            Err(RuntimeError::RequestError {
                status: Some(422),
                ..
            })
        ));
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
        assert!(!matches!(
            tokio::time::timeout(Duration::from_millis(20), pauses.recv()).await,
            Ok(Some(_))
        ));
    }

    #[tokio::test]
    async fn oversized_auxiliary_branch_stores_cannot_be_pinned_by_a_checkpoint() {
        use crate::execution::effect_continuation::MAX_EFFECT_SELECTION_BYTES;
        use plasm_core::collection_codec::{CollectionCodec, Observation, RecordingCodec};
        for store_kind in 0..3 {
            let transport = Arc::new(Transport {
                statuses: vec![422],
                opaque: false,
                requests: Mutex::new(Vec::new()),
            });
            let engine = ExecutionEngine::new_with_transport(
                ExecutionConfig {
                    base_url: Some("https://fixture.test".into()),
                    ..Default::default()
                },
                Arc::clone(&transport) as Arc<dyn HttpTransport>,
                None,
            );
            let compiled = request();
            let mut materialization = SessionMaterialization::new();
            let payload = "x".repeat(MAX_EFFECT_SELECTION_BYTES);
            if store_kind == 0 {
                let fingerprint = crate::RequestFingerprint::from_operation(&compiled).unwrap();
                materialization.responses.store(
                    fingerprint,
                    crate::replay::TransportOutcome {
                        response: serde_json::json!({"private": payload}),
                        next_link: None,
                    },
                    ExecutionSource::Cache,
                );
            } else if store_kind == 1 {
                materialization.provided_session_params.insert(
                    "fixture".into(),
                    indexmap::IndexMap::from([("private".into(), Value::String(payload))]),
                );
            } else {
                let query = plasm_core::QueryExpr::all("Item");
                let key = crate::query_index::QueryCacheKey::from_query(
                    &query,
                    "fixture",
                    &plasm_core::CGS::new(),
                    &Default::default(),
                    0,
                )
                .unwrap();
                let record = RecordingCodec::new()
                    .record(
                        key.identity().clone(),
                        vec![plasm_core::Ref::new("Item", &payload)],
                        Observation::ExactOutput { decoded: 1 },
                    )
                    .unwrap();
                materialization.query_index.insert(key, record).unwrap();
            }
            let journal = super::super::mutation_evidence::MutationJournal::default();
            let (control, mut pauses) =
                EffectContinuation::new_native(Duration::from_secs(5)).unwrap();
            let result = control
                .scope(journal.scope(engine.dispatch_compiled_mutation(
                    &compiled,
                    "PwExpense",
                    OperationIdentity {
                        entry_id: "matrix".into(),
                        capability: "pwexpense_create".into(),
                    },
                    &capability(),
                    None,
                    RetainedMutationFrame {
                        materialization: &materialization,
                        env: empty_env(),
                    },
                )))
                .await;
            assert!(matches!(
                result,
                Err(RuntimeError::RequestError {
                    status: Some(422),
                    ..
                })
            ));
            assert_eq!(transport.requests.lock().unwrap().len(), 1);
            assert!(!matches!(
                tokio::time::timeout(Duration::from_millis(20), pauses.recv()).await,
                Ok(Some(_))
            ));
        }
    }

    #[tokio::test]
    async fn paused_mutation_blocks_competing_dispatch_then_resumes_original() {
        let transport = Arc::new(Transport {
            statuses: vec![422, 200],
            opaque: false,
            requests: Mutex::new(Vec::new()),
        });
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("https://fixture.test".into()),
                ..Default::default()
            },
            Arc::clone(&transport) as Arc<dyn HttpTransport>,
            None,
        );
        let compiled = request();
        let capability = capability();
        let identity = OperationIdentity {
            entry_id: "matrix".into(),
            capability: "pwexpense_create".into(),
        };
        let graph = SessionMaterialization::new();
        let journal = super::super::mutation_evidence::MutationJournal::default();
        let (control, mut pauses) = EffectContinuation::new_native(Duration::from_secs(5)).unwrap();
        let run = control
            .clone()
            .scope(journal.scope(engine.dispatch_compiled_mutation(
                &compiled,
                "PwExpense",
                identity.clone(),
                &capability,
                None,
                RetainedMutationFrame {
                    materialization: &graph,
                    env: empty_env(),
                },
            )));
        tokio::pin!(run);
        let mut pause = tokio::select! {
            pause = pauses.recv() => pause.expect("first rejection must pause"),
            _ = &mut run => panic!("original returned before checkpoint"),
        };
        let CompiledOperation::Http(request) = &compiled else {
            unreachable!()
        };
        let competing = journal
            .scope(super::super::mutation_evidence::with_mutation_identity(
                identity,
                engine.execute_http_request_full(request),
            ))
            .await;
        assert!(matches!(
            competing,
            Err(RuntimeError::MutationJournalPaused)
        ));
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
        control.resume(&mut pause).unwrap();
        assert!(run.await.is_ok());
        assert_eq!(transport.requests.lock().unwrap().len(), 2);
    }

    async fn exercise_resume(
        change_auth: bool,
        opaque: bool,
        first_status: u16,
        cancel: bool,
        rejections: usize,
        missing_auth: bool,
    ) {
        let secrets = Arc::new(Secrets::default());
        let transport = Arc::new(Transport {
            statuses: vec![first_status; rejections],
            opaque,
            requests: Mutex::new(Vec::new()),
        });
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("https://fixture.test".into()),
                ..Default::default()
            },
            Arc::clone(&transport) as Arc<dyn HttpTransport>,
            Some(crate::auth::AuthResolver::new(
                plasm_core::AuthScheme::BearerToken {
                    env: Some("fixture".into()),
                    hosted_kv: None,
                    optional_env: false,
                },
                Arc::clone(&secrets) as Arc<dyn crate::auth::SecretProvider>,
            )),
        );
        let capability = capability();
        let compiled = request();
        let graph = SessionMaterialization::new();
        let journal = super::super::mutation_evidence::MutationJournal::default();
        let workflow = Uuid::new_v4();
        let (control, mut pauses) =
            EffectContinuation::new(workflow, Duration::from_secs(5)).unwrap();
        let run = control
            .clone()
            .scope(journal.scope(engine.dispatch_compiled_mutation(
                &compiled,
                "PwExpense",
                OperationIdentity {
                    entry_id: "matrix".into(),
                    capability: "pwexpense_create".into(),
                },
                &capability,
                None,
                RetainedMutationFrame {
                    materialization: &graph,
                    env: empty_env(),
                },
            )));
        tokio::pin!(run);
        if opaque || first_status != 422 {
            assert!(run.await.is_err());
            assert_eq!(transport.requests.lock().unwrap().len(), 1);
            assert!(
                tokio::time::timeout(Duration::from_millis(20), pauses.recv())
                    .await
                    .is_err()
            );
            return;
        }
        let mut pause = tokio::select! {
            pause = pauses.recv() => pause.expect("known rejection should suspend"),
            _ = &mut run => panic!("execution returned before the checkpoint"),
        };
        assert_eq!(pause.failure().dispatches.len(), 1);
        assert_eq!(
            pause.failure().dispatches[0].status,
            crate::MutationDispatchStatus::RejectedWithoutEffect
        );
        assert!(matches!(
            pause.resume(Uuid::new_v4()),
            Err(EffectContinuationError::WorkflowMismatch)
        ));
        if cancel {
            control.cancel();
            assert!(matches!(run.await, Err(RuntimeError::Cancelled)));
            assert_eq!(transport.requests.lock().unwrap().len(), 1);
            assert!(matches!(
                pause.resume(workflow),
                Err(EffectContinuationError::AlreadyResolved)
            ));
            return;
        }
        if change_auth {
            secrets.changed.store(true, Ordering::SeqCst);
        }
        if missing_auth {
            secrets.missing.store(true, Ordering::SeqCst);
        }
        pause.resume(workflow).unwrap();
        if rejections == 2 && !change_auth && !missing_auth {
            let mut second = tokio::select! {
                pause = pauses.recv() => pause.expect("second known rejection should suspend"),
                _ = &mut run => panic!("execution returned before the second checkpoint"),
            };
            assert_eq!(
                second.failure().dispatches[0].status,
                crate::MutationDispatchStatus::RejectedWithoutEffect
            );
            second.resume(workflow).unwrap();
        }
        let result = run.await;
        let requests = transport.requests.lock().unwrap();
        if change_auth || missing_auth {
            let error = result.expect_err("changed auth cannot resume the rejected dispatch");
            let RuntimeError::ContinuationAuthorization { reason, rejection } = &error else {
                panic!("expected typed continuation authorization failure");
            };
            assert!(matches!(
                rejection.as_ref(),
                RuntimeError::RequestError {
                    status: Some(422),
                    ..
                }
            ));
            if missing_auth {
                assert!(matches!(
                    reason,
                    crate::error::ContinuationAuthorizationReason::RevalidationFailed(_)
                ));
            } else {
                assert!(matches!(
                    reason,
                    crate::error::ContinuationAuthorizationReason::ScopeChanged
                ));
            }
            let failure = crate::ExecutionFailure::from(error).with_dispatches(journal.snapshot());
            assert_eq!(failure.cause, crate::FailureCause::Authorization);
            assert_eq!(
                failure.code,
                if missing_auth {
                    "continuation_auth_revalidation_failed"
                } else {
                    "continuation_auth_scope_changed"
                }
            );
            let wire = serde_json::to_string(&failure).unwrap();
            assert!(!wire.contains("first-token") && !wire.contains("second-token"));
            assert_eq!(requests.len(), 1);
        } else {
            assert!(result.is_ok());
            assert_eq!(requests.len(), rejections + 1);
            for retry in &requests[1..] {
                assert_eq!(
                    requests[0].0, retry.0,
                    "retry must reuse the compiled request"
                );
                assert_eq!(
                    requests[0].1.as_ref().unwrap().headers,
                    retry.1.as_ref().unwrap().headers
                );
            }
            assert_eq!(
                requests[0].1.as_ref().unwrap().headers[0].1,
                "Bearer first-token"
            );
            assert_eq!(
                secrets.reads.load(Ordering::SeqCst),
                rejections + 1,
                "auth is revalidated once at resume"
            );
            let dispatches = journal.snapshot();
            assert_eq!(dispatches.len(), rejections + 1);
            assert!(dispatches[..rejections]
                .iter()
                .all(|dispatch| dispatch.status
                    == crate::MutationDispatchStatus::RejectedWithoutEffect));
        }
    }

    #[tokio::test]
    async fn known_rejection_resumes_the_same_compiled_request() {
        exercise_resume(false, false, 422, false, 1, false).await;
    }

    #[tokio::test]
    async fn repeated_known_rejections_preserve_each_attempt() {
        exercise_resume(false, false, 422, false, 2, false).await;
    }

    #[tokio::test]
    async fn changed_or_opaque_authority_cannot_resume() {
        exercise_resume(true, false, 422, false, 1, false).await;
        exercise_resume(false, false, 422, false, 1, true).await;
        exercise_resume(false, true, 422, false, 1, false).await;
    }

    #[tokio::test]
    async fn unknown_dispatch_cannot_be_retried() {
        exercise_resume(false, false, 500, false, 1, false).await;
    }

    #[tokio::test]
    async fn cancellation_prevents_a_suspended_dispatch_from_retrying() {
        exercise_resume(false, false, 422, true, 1, false).await;
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
