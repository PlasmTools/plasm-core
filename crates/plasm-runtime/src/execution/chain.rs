//! Chain (Kleisli) navigation and relation fanout.

use super::*;

impl ExecutionEngine {
    /// Execute a chain expression (Kleisli EntityRef navigation).
    ///
    /// 1. Execute the source expression to get one or more entities.
    /// 2. For each entity, extract the EntityRef field value → target ID.
    /// 3. **Batch**: deduplicate IDs, satisfy from cache, fetch uncached via concurrent GETs.
    /// 4. Reassemble in source order (preserving duplicates).
    pub(crate) async fn execute_chain(
        &self,
        chain: &plasm_core::ChainExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> Result<ExecutionResult, RuntimeError> {
        let source_result = self
            .execute(
                &chain.source,
                cgs,
                mat,
                Some(mode),
                consume.clone(),
                opts.clone(),
            )
            .await?;

        if source_result.entities.is_empty() {
            return Ok(ExecutionResult {
                entities: vec![],
                count: 0,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats,
                request_fingerprints: source_result.request_fingerprints.clone(),
            });
        }

        let source_entity_name_owned: String = if matches!(chain.source.as_ref(), Expr::Chain(_)) {
            source_result
                .entities
                .first()
                .map(|e| e.reference.entity_type.to_string())
                .unwrap_or_else(|| chain.source.primary_entity().to_string())
        } else {
            chain.source.primary_entity().to_string()
        };
        let source_entity_name = source_entity_name_owned.as_str();
        let source_entity =
            cgs.get_entity(source_entity_name)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("Chain source entity '{}' not in CGS", source_entity_name),
                })?;

        // Resolve the target entity type — either from an EntityRef field or a
        // declared relation (cardinality-one decode, scoped query, embedded GET refs).
        let target_entity_name: String = if let Some(field_schema) =
            source_entity.fields.get(chain.selector.as_str())
        {
            let nv = cgs.named_value_for_slot(field_schema).map_err(|e| {
                RuntimeError::ConfigurationError {
                    message: format!(
                        "Field '{}.{}': invalid value_ref — {}",
                        source_entity_name, chain.selector, e
                    ),
                }
            })?;
            match &nv.field_type {
                FieldType::EntityRef { target } => target.to_string(),
                _ => {
                    return Err(RuntimeError::ConfigurationError {
                        message: format!(
                            "Field '{}.{}' is {:?}, not EntityRef",
                            source_entity_name, chain.selector, nv.field_type
                        ),
                    });
                }
            }
        } else if let Some(rel) = source_entity.relations.get(chain.selector.as_str()) {
            match rel.cardinality {
                plasm_core::Cardinality::Many => {
                    let rel_mat = rel
                        .materialize
                        .as_ref()
                        .unwrap_or(&RelationMaterialization::Unavailable);
                    match rel_mat {
                        RelationMaterialization::QueryScoped { capability, param } => {
                            return self
                                .execute_chain_via_param(
                                    &source_result,
                                    rel,
                                    capability,
                                    param,
                                    cgs,
                                    mat,
                                    mode,
                                    &chain.step,
                                    consume.clone(),
                                    opts.clone(),
                                )
                                .await;
                        }
                        RelationMaterialization::QueryScopedBindings {
                            capability,
                            bindings,
                        } => {
                            return self
                                .execute_chain_via_bindings(
                                    &source_result,
                                    source_entity,
                                    rel,
                                    capability,
                                    bindings,
                                    cgs,
                                    mat,
                                    mode,
                                    &chain.step,
                                    consume.clone(),
                                    opts.clone(),
                                )
                                .await;
                        }
                        RelationMaterialization::FromParentGet { .. }
                        | RelationMaterialization::ViewEmbed { .. } => {
                            return self
                                .execute_chain_from_embedded_relations(
                                    &source_result,
                                    rel,
                                    cgs,
                                    mat,
                                    mode,
                                    &chain.step,
                                    consume.clone(),
                                    opts.clone(),
                                )
                                .await;
                        }
                        RelationMaterialization::PreferFromParentGet { .. } => {
                            return self
                                .execute_chain_prefer_from_parent_get(
                                    &source_result,
                                    source_entity,
                                    rel,
                                    cgs,
                                    mat,
                                    mode,
                                    &chain.step,
                                    consume.clone(),
                                    opts.clone(),
                                )
                                .await;
                        }
                        RelationMaterialization::Unavailable => {
                            return Err(RuntimeError::ConfigurationError {
                                message: format!(
                                    "Relation '{}.{}' is not configured for chain traversal (materialize unavailable)",
                                    source_entity_name, chain.selector
                                ),
                            });
                        }
                        RelationMaterialization::GetScopedBindings { .. } => {
                            return Err(RuntimeError::ConfigurationError {
                                message: format!(
                                    "Relation '{}.{}': get_scoped_bindings requires cardinality one",
                                    source_entity_name, chain.selector
                                ),
                            });
                        }
                    }
                }
                plasm_core::Cardinality::One => {
                    match rel.materialize.as_ref() {
                        Some(RelationMaterialization::GetScopedBindings {
                            capability,
                            bindings,
                        }) => {
                            return self
                                .execute_chain_via_get_bindings(
                                    &source_result,
                                    source_entity,
                                    rel.target_resource.clone(),
                                    capability,
                                    bindings,
                                    cgs,
                                    mat,
                                    mode,
                                )
                                .await;
                        }
                        Some(RelationMaterialization::FromParentGet { .. }) => {
                            return self
                                .execute_chain_from_embedded_relations(
                                    &source_result,
                                    rel,
                                    cgs,
                                    mat,
                                    mode,
                                    &chain.step,
                                    consume.clone(),
                                    opts.clone(),
                                )
                                .await;
                        }
                        Some(RelationMaterialization::QueryScoped { .. })
                        | Some(RelationMaterialization::QueryScopedBindings { .. }) => {
                            return Err(RuntimeError::ConfigurationError {
                                message: format!(
                                    "Relation '{}.{}': query-scoped materialization is invalid for cardinality one",
                                    source_entity_name, chain.selector
                                ),
                            });
                        }
                        Some(RelationMaterialization::PreferFromParentGet { .. })
                        | Some(RelationMaterialization::ViewEmbed { .. }) => {
                            return Err(RuntimeError::ConfigurationError {
                                message: format!(
                                    "Relation '{}.{}': prefer_from_parent_get/view_embed requires cardinality many",
                                    source_entity_name, chain.selector
                                ),
                            });
                        }
                        Some(RelationMaterialization::Unavailable) | None => {}
                    }
                    rel.target_resource.to_string()
                }
            }
        } else {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "Chain selector '{}' not found on entity '{}' (not an EntityRef field or relation)",
                    chain.selector, source_entity_name
                ),
            });
        };

        // ── Extract ref IDs from source entities ─────────────────────────
        let ref_ids: Vec<Option<String>> = source_result
            .entities
            .iter()
            .map(|e| extract_ref_id(e, &chain.selector, cgs))
            .collect();

        // ── Explicit continuation: no batching, dispatch per-entity ──────
        if matches!(chain.step, ChainStep::Explicit { .. }) {
            let mut resolved = Vec::new();
            let mut total_network = source_result.stats.network_requests;
            let mut total_cache_hits = source_result.stats.cache_hits;
            let mut any_live = source_result.source == ExecutionSource::Live;

            if let ChainStep::Explicit { expr } = &chain.step {
                for id_opt in &ref_ids {
                    let Some(_id) = id_opt else { continue };
                    let r = self
                        .execute(
                            expr,
                            cgs,
                            mat,
                            Some(mode),
                            StreamConsumeOpts::default(),
                            opts.clone(),
                        )
                        .await?;
                    if r.source == ExecutionSource::Live {
                        any_live = true;
                    }
                    total_network += r.stats.network_requests;
                    total_cache_hits += r.stats.cache_hits;
                    resolved.extend(r.entities);
                }
            }

            let count = resolved.len();
            return Ok(ExecutionResult {
                entities: resolved,
                count,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: if any_live {
                    ExecutionSource::Live
                } else {
                    ExecutionSource::Cache
                },
                stats: ExecutionStats {
                    duration_ms: 0,
                    network_requests: total_network,
                    cache_hits: total_cache_hits,
                    cache_misses: count,
                    ..Default::default()
                },
                request_fingerprints: Vec::new(),
            });
        }

        // ── AutoGet with batching ────────────────────────────────────────
        // Deduplicate IDs.
        let unique_ids: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            ref_ids
                .iter()
                .filter_map(|o| o.as_ref())
                .filter(|id| seen.insert((*id).clone()))
                .cloned()
                .collect()
        };

        // Partition: cached (Complete) vs uncached.
        let mut cached_hits = 0usize;
        let to_fetch: Vec<Ref> = unique_ids
            .iter()
            .filter(|id| {
                let r = Ref::new(&target_entity_name, id.as_str());
                if matches!(
                    mat.get(&r).map(|e| e.completeness),
                    Some(crate::EntityCompleteness::Complete)
                ) {
                    cached_hits += 1;
                    false
                } else {
                    true
                }
            })
            .map(|id| Ref::new(&target_entity_name, id.as_str()))
            .collect();

        // Fetch uncached via concurrent GETs (same pattern as hydrate).
        let mut extra_network = 0usize;
        let mut any_live = source_result.source == ExecutionSource::Live;

        if !to_fetch.is_empty() {
            use futures_util::stream::{self, StreamExt};

            let concurrency = self.config.hydrate_concurrency.max(1);
            let mut stream = stream::iter(to_fetch.into_iter().map(|reference| {
                let get = GetExpr::from_ref(reference.clone());
                async move {
                    self.fetch_get_decoded(
                        &get,
                        cgs,
                        mode,
                        None,
                        false,
                        None,
                        &ViewAmbientContext::default(),
                    )
                    .await
                }
            }))
            .buffer_unordered(concurrency);

            while let Some(res) = stream.next().await {
                cooperative_cancel_check()?;
                let (entity, source) = res?;
                if source == ExecutionSource::Live {
                    any_live = true;
                    extra_network += 1;
                }
                mat.insert(entity)?;
            }
        }

        // Reassemble in source order from cache.
        let mut resolved = Vec::with_capacity(ref_ids.len());
        for id_opt in &ref_ids {
            let Some(id) = id_opt else { continue };
            let r = Ref::new(&target_entity_name, id.as_str());
            if let Some(e) = mat.get(&r) {
                resolved.push(e.clone());
            }
        }

        let count = resolved.len();
        Ok(ExecutionResult {
            entities: resolved,
            count,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: if any_live {
                ExecutionSource::Live
            } else {
                ExecutionSource::Cache
            },
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: source_result.stats.network_requests + extra_network,
                cache_hits: source_result.stats.cache_hits + cached_hits,
                cache_misses: count,
                ..Default::default()
            },
            request_fingerprints: Vec::new(),
        })
    }

    /// Parallel scoped-query fanout; `network_jobs` must already list `(parent_index, query)`.
    async fn fanout_scoped_query_parallel(
        &self,
        source_result: &ExecutionResult,
        mut per_parent: Vec<Vec<CachedEntity>>,
        network_jobs: Vec<(usize, QueryExpr)>,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        use futures_util::stream::{self, StreamExt};

        let mut total_network = source_result.stats.network_requests;
        let mut merged_stats = ExecutionStats::default();
        merged_stats.merge_telemetry(&source_result.stats.cache);
        let mut any_live = source_result.source == ExecutionSource::Live;
        let graph_hits = per_parent.iter().map(|v| v.len()).sum::<usize>();

        if !network_jobs.is_empty() {
            let concurrency = self.config.hydrate_concurrency.max(1);
            let branch_seed = {
                let snap = mat.snapshot();
                SessionMaterialization {
                    graph: snap.into_graph(),
                    responses: mat.responses.clone(),
                    query_index: mat.query_index.clone(),
                }
            };

            let mut stream = stream::iter(network_jobs.into_iter().map(|(parent_idx, q)| {
                let mut branch = branch_seed.clone();
                async move {
                    let result = self
                        .execute_query(
                            &q,
                            cgs,
                            &mut branch,
                            mode,
                            StreamConsumeOpts::default(),
                            &ViewAmbientContext::default(),
                        )
                        .await?;
                    Ok::<_, RuntimeError>((parent_idx, result, branch))
                }
            }))
            .buffer_unordered(concurrency);

            while let Some(res) = stream.next().await {
                cooperative_cancel_check()?;
                let (parent_idx, result, branch) = res?;
                mat.absorb_branch(branch)?;
                if result.source == ExecutionSource::Live {
                    any_live = true;
                }
                total_network += result.stats.network_requests;
                merged_stats.merge_telemetry(&result.stats.cache);
                // At most one scoped query per parent today; order within parent = query row order.
                per_parent[parent_idx].extend(result.entities);
            }
        }

        let all_entities: Vec<CachedEntity> = per_parent.into_iter().flatten().collect();
        let count = all_entities.len();
        merged_stats.cache_hits = merged_stats
            .cache
            .legacy_cache_hits()
            .saturating_add(graph_hits);
        merged_stats.cache_misses = merged_stats.cache.legacy_cache_misses();
        merged_stats.network_requests = total_network;
        merged_stats.record_rows_materialized(count);
        Ok(ExecutionResult {
            entities: all_entities,
            count,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: if any_live {
                ExecutionSource::Live
            } else {
                ExecutionSource::Cache
            },
            stats: merged_stats,
            request_fingerprints: Vec::new(),
        })
    }

    /// Execute a `via_param` relation traversal: for each source entity, run a scoped
    /// query on the target using the source entity's `id_field` value as `via_param`.
    ///
    /// When every parent row has decoded `relations[rel]`, uses embedded graph only; otherwise
    /// hybrid fanout (graph or scoped query per row; graph miss with relation key is an error).
    #[allow(clippy::too_many_arguments)]
    async fn execute_chain_via_param(
        &self,
        source_result: &ExecutionResult,
        rel: &plasm_core::RelationSchema,
        capability: &plasm_core::CapabilityName,
        via_param: &CapabilityParamName,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        _chain_step: &plasm_core::ChainStep,
        _consume: StreamConsumeOpts,
        _opts: ExecuteOptions,
    ) -> Result<ExecutionResult, RuntimeError> {
        let target_entity = rel.target_resource.clone();
        let target_key = target_entity.as_str();
        let cap = cgs.get_capability(capability.as_str()).ok_or_else(|| {
            RuntimeError::ConfigurationError {
                message: format!(
                    "Chain materialize: unknown capability '{}' (target entity '{}')",
                    capability, target_key
                ),
            }
        })?;
        if cap.domain.as_str() != target_key {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "Chain materialize: capability '{}' domain '{}' does not match target '{}'",
                    capability, cap.domain, target_key
                ),
            });
        }
        let capability_name = cap.name.clone();

        if source_result.entities.is_empty() {
            return Ok(ExecutionResult {
                entities: vec![],
                count: 0,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats.clone(),
                request_fingerprints: source_result.request_fingerprints.clone(),
            });
        }

        let via = via_param.clone();
        let network_jobs = partition_scoped_query_fanout(&source_result.entities, |entity| {
            let id_field = cgs
                .get_entity(entity.reference.entity_type.as_str())
                .map(|def| def.id_field.as_str().to_string())
                .unwrap_or_default();
            let id = entity
                .get_field(id_field.as_str())
                .map(|tf| tf.to_value())
                .and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Integer(n) => Some(n.to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| entity.reference.primary_slot_str());
            let pred = plasm_core::Predicate::eq(via.as_str(), id);
            let mut q = QueryExpr::filtered(target_entity.clone(), pred);
            q.capability_name = Some(capability_name.clone());
            q
        });
        let per_parent = vec![Vec::new(); source_result.entities.len()];
        self.fanout_scoped_query_parallel(source_result, per_parent, network_jobs, cgs, mat, mode)
            .await
    }

    /// Multi-parameter scoped query fanout (`RelationMaterialization::QueryScopedBindings`).
    /// Always one scoped query per parent row (ignores decoded `relations` on the parent).
    #[allow(clippy::too_many_arguments)]
    async fn execute_chain_via_bindings(
        &self,
        source_result: &ExecutionResult,
        parent_entity_def: &plasm_core::EntityDef,
        rel: &plasm_core::RelationSchema,
        capability: &plasm_core::CapabilityName,
        bindings: &IndexMap<CapabilityParamName, EntityFieldName>,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        _chain_step: &plasm_core::ChainStep,
        _consume: StreamConsumeOpts,
        _opts: ExecuteOptions,
    ) -> Result<ExecutionResult, RuntimeError> {
        let target_entity = rel.target_resource.clone();
        let target_key = target_entity.as_str();
        let cap = cgs.get_capability(capability.as_str()).ok_or_else(|| {
            RuntimeError::ConfigurationError {
                message: format!(
                    "Chain materialize: unknown capability '{}' (target entity '{}')",
                    capability, target_key
                ),
            }
        })?;
        if cap.domain.as_str() != target_key {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "Chain materialize: capability '{}' domain '{}' does not match target '{}'",
                    capability, cap.domain, target_key
                ),
            });
        }

        if source_result.entities.is_empty() {
            return Ok(ExecutionResult {
                entities: vec![],
                count: 0,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats.clone(),
                request_fingerprints: source_result.request_fingerprints.clone(),
            });
        }

        let capability_name = cap.name.clone();
        let cap_params: Vec<_> = cap.object_params().map(|f| f.to_vec()).unwrap_or_default();
        let parent_def = parent_entity_def;
        let binds = bindings;

        let network_jobs = partition_scoped_query_fanout(&source_result.entities, |entity| {
            let preds: Vec<Predicate> = binds
                .iter()
                .map(|(cap_param, parent_field)| {
                    let raw = chain_binding_raw_json(entity, parent_def, parent_field);
                    let value =
                        chain_binding_plasm_value(&raw, cap_param.as_str(), &cap_params, cgs);
                    Predicate::eq(cap_param.as_str(), value)
                })
                .collect();
            let pred = if preds.len() == 1 {
                preds.into_iter().next().expect("non-empty preds")
            } else {
                Predicate::and(preds)
            };
            let mut q = QueryExpr::filtered(target_entity.clone(), pred);
            q.capability_name = Some(capability_name.clone());
            q
        });
        let per_parent = vec![Vec::new(); source_result.entities.len()];
        self.fanout_scoped_query_parallel(source_result, per_parent, network_jobs, cgs, mat, mode)
            .await
    }

    /// [`RelationMaterialization::PreferFromParentGet`]: typed per-row embed vs declared scoped fallback.
    #[allow(clippy::too_many_arguments)]
    async fn execute_chain_prefer_from_parent_get(
        &self,
        source_result: &ExecutionResult,
        parent_entity_def: &EntityDef,
        rel: &RelationSchema,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        chain_step: &ChainStep,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> Result<ExecutionResult, RuntimeError> {
        let Some(RelationMaterialization::PreferFromParentGet { fallback, .. }) =
            rel.materialize.as_ref()
        else {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "Relation '{}': expected PreferFromParentGet materialize",
                    rel.name
                ),
            });
        };

        if source_result.entities.is_empty() {
            return Ok(ExecutionResult {
                entities: vec![],
                count: 0,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats.clone(),
                request_fingerprints: source_result.request_fingerprints.clone(),
            });
        }

        let relation_key = rel.name.as_str();
        let expected_target = rel.target_resource.as_str();
        let target_entity = rel.target_resource.clone();
        let all_embed = source_result.entities.iter().all(|parent| {
            let parent_json = parent.payload_to_json();
            matches!(
                resolve_relation_row_resolution(
                    rel.materialize.as_ref().expect("prefer"),
                    relation_key,
                    expected_target,
                    &parent_json,
                    parent.relations.get(relation_key).map(|v| v.as_slice()),
                    |r| mat.get(r).is_some(),
                ),
                RelationRowResolution::EmbeddedRefs(_)
            )
        });
        if all_embed {
            return self
                .execute_chain_from_embedded_relations(
                    source_result,
                    rel,
                    cgs,
                    mat,
                    mode,
                    chain_step,
                    consume,
                    opts,
                )
                .await;
        }

        let (per_parent, network_jobs) = partition_prefer_from_parent_get(
            &source_result.entities,
            rel.materialize.as_ref().expect("prefer"),
            relation_key,
            expected_target,
            mat,
            parent_entity_def,
            cgs,
            &target_entity,
            fallback,
        )?;
        self.fanout_scoped_query_parallel(source_result, per_parent, network_jobs, cgs, mat, mode)
            .await
    }

    /// Chain on `get_scoped_bindings`: synthesize a [`GetExpr`] per parent row from binding keys.
    #[allow(clippy::too_many_arguments)]
    async fn execute_chain_via_get_bindings(
        &self,
        source_result: &ExecutionResult,
        parent_entity_def: &EntityDef,
        target_entity: EntityName,
        capability: &plasm_core::CapabilityName,
        bindings: &IndexMap<CapabilityParamName, EntityFieldName>,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        use futures_util::stream::{self, StreamExt};

        let target_key = target_entity.as_str();
        let cap = cgs.get_capability(capability.as_str()).ok_or_else(|| {
            RuntimeError::ConfigurationError {
                message: format!(
                    "Chain materialize: unknown capability '{}' (target entity '{}')",
                    capability, target_key
                ),
            }
        })?;
        if cap.domain.as_str() != target_key {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "Chain materialize: capability '{}' domain '{}' does not match target '{}'",
                    capability, cap.domain, target_key
                ),
            });
        }

        let target_ent =
            cgs.get_entity(target_key)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("Chain materialize: unknown target entity '{target_key}'"),
                })?;

        let mut gets: Vec<GetExpr> = Vec::new();
        for entity in &source_result.entities {
            let mut bound: IndexMap<String, String> = IndexMap::new();
            for (cap_param, parent_field) in bindings.iter() {
                bound.insert(
                    cap_param.to_string(),
                    chain_binding_value(entity, parent_entity_def, parent_field),
                );
            }
            let reference = ref_from_materialize_bindings_for_get_chain(target_ent, &bound)?;
            gets.push(GetExpr::from_ref(reference));
        }

        if gets.is_empty() {
            return Ok(ExecutionResult {
                entities: vec![],
                count: 0,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats.clone(),
                request_fingerprints: source_result.request_fingerprints.clone(),
            });
        }

        let concurrency = self.config.hydrate_concurrency.max(1);
        let mut all_entities: Vec<CachedEntity> = Vec::new();
        let total_network = source_result.stats.network_requests;
        let total_cache_hits = source_result.stats.cache_hits;
        let mut any_live = source_result.source == ExecutionSource::Live;

        let cap_named = capability.clone();
        let mut stream = stream::iter(gets.into_iter().map(move |get| {
            let c = cap_named.clone();
            async move {
                self.fetch_get_decoded(
                    &get,
                    cgs,
                    mode,
                    Some(c.as_str()),
                    false,
                    None,
                    &ViewAmbientContext::default(),
                )
                .await
            }
        }))
        .buffer_unordered(concurrency);

        let mut extra_network = 0usize;
        let mut extra_cache_hits = 0usize;
        while let Some(res) = stream.next().await {
            cooperative_cancel_check()?;
            let (entity, source) = res?;
            if source == ExecutionSource::Live {
                any_live = true;
                extra_network += 1;
            } else {
                extra_cache_hits += 1;
            }
            all_entities.push(entity);
        }

        mat.merge(all_entities.clone())?;
        let count = all_entities.len();
        Ok(ExecutionResult {
            entities: all_entities,
            count,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: if any_live {
                ExecutionSource::Live
            } else {
                ExecutionSource::Cache
            },
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: total_network + extra_network,
                cache_hits: total_cache_hits + extra_cache_hits,
                cache_misses: count,
                ..Default::default()
            },
            request_fingerprints: Vec::new(),
        })
    }

    /// Chain on `FromParentGet`: refs already on `CachedEntity.relations[relation.name]`.
    #[allow(clippy::too_many_arguments)] // cache + mode + step + consume mirror `execute_chain` helpers
    async fn execute_chain_from_embedded_relations(
        &self,
        source_result: &ExecutionResult,
        relation: &RelationSchema,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        chain_step: &ChainStep,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> Result<ExecutionResult, RuntimeError> {
        use futures_util::stream::{self, StreamExt};

        let relation_key = relation.name.as_str();
        let expected_target = &relation.target_resource;

        let mut ordered_refs: Vec<Ref> = Vec::new();
        for e in &source_result.entities {
            if let Some(refs) = e.relations.get(relation_key) {
                for r in refs {
                    if r.entity_type != *expected_target {
                        return Err(RuntimeError::ConfigurationError {
                            message: format!(
                                "Decoded relation '{}' expected Ref.entity_type {} (CGS target_resource), got {}",
                                relation.name, expected_target, r.entity_type
                            ),
                        });
                    }
                    ordered_refs.push(r.clone());
                }
            }
        }

        if ordered_refs.is_empty() {
            return Ok(ExecutionResult {
                entities: vec![],
                count: 0,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats.clone(),
                request_fingerprints: source_result.request_fingerprints.clone(),
            });
        }

        if matches!(chain_step, ChainStep::Explicit { .. }) {
            let mut resolved = Vec::new();
            let mut total_network = source_result.stats.network_requests;
            let mut total_cache_hits = source_result.stats.cache_hits;
            let mut any_live = source_result.source == ExecutionSource::Live;

            if let ChainStep::Explicit { expr } = chain_step {
                // One continuation eval per decoded ref (refs already validated against
                // `relation.target_resource` when collected above).
                let mut remaining = ordered_refs.len();
                while remaining > 0 {
                    remaining -= 1;
                    let res = self
                        .execute(expr, cgs, mat, Some(mode), consume.clone(), opts.clone())
                        .await?;
                    if res.source == ExecutionSource::Live {
                        any_live = true;
                    }
                    total_network += res.stats.network_requests;
                    total_cache_hits += res.stats.cache_hits;
                    resolved.extend(res.entities);
                }
            }

            let count = resolved.len();
            return Ok(ExecutionResult {
                entities: resolved,
                count,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: if any_live {
                    ExecutionSource::Live
                } else {
                    ExecutionSource::Cache
                },
                stats: ExecutionStats {
                    duration_ms: 0,
                    network_requests: total_network,
                    cache_hits: total_cache_hits,
                    cache_misses: count,
                    ..Default::default()
                },
                request_fingerprints: Vec::new(),
            });
        }

        let mut seen = HashSet::new();
        let unique_refs: Vec<Ref> = ordered_refs
            .iter()
            .filter(|r| seen.insert((*r).clone()))
            .cloned()
            .collect();

        let mut cached_hits = 0usize;
        let to_fetch: Vec<Ref> = unique_refs
            .iter()
            .filter(|r| {
                if matches!(
                    mat.get(r).map(|e| e.completeness),
                    Some(crate::EntityCompleteness::Complete)
                ) {
                    cached_hits += 1;
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect();

        let mut extra_network = 0usize;
        let mut any_live = source_result.source == ExecutionSource::Live;

        if !to_fetch.is_empty() {
            let concurrency = self.config.hydrate_concurrency.max(1);
            let mut stream = stream::iter(to_fetch.into_iter().map(|reference| {
                let get = GetExpr::from_ref(reference.clone());
                async move {
                    self.fetch_get_decoded(
                        &get,
                        cgs,
                        mode,
                        None,
                        false,
                        None,
                        &ViewAmbientContext::default(),
                    )
                    .await
                }
            }))
            .buffer_unordered(concurrency);

            while let Some(res) = stream.next().await {
                cooperative_cancel_check()?;
                let (entity, source) = res?;
                if source == ExecutionSource::Live {
                    any_live = true;
                    extra_network += 1;
                }
                mat.insert(entity)?;
            }
        }

        let mut resolved = Vec::with_capacity(ordered_refs.len());
        for r in &ordered_refs {
            if let Some(e) = mat.get(r) {
                resolved.push(e.clone());
            }
        }

        let count = resolved.len();
        Ok(ExecutionResult {
            entities: resolved,
            count,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: if any_live {
                ExecutionSource::Live
            } else {
                ExecutionSource::Cache
            },
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: source_result.stats.network_requests + extra_network,
                cache_hits: source_result.stats.cache_hits + cached_hits,
                cache_misses: count,
                ..Default::default()
            },
            request_fingerprints: Vec::new(),
        })
    }
}
