//! Chain (Kleisli) navigation and relation fanout.

use super::*;
use plasm_core::collection_codec::{CollectionIdentity, Demand, Observation, SharedRows};

pub(crate) struct RelationGetBindings<'a> {
    pub(crate) mat: &'a SessionMaterialization,
    pub(crate) inherited: &'a CapabilityParamEnv,
}

impl GetBindings for RelationGetBindings<'_> {
    fn bindings_for(&self, get: &GetExpr, cgs: &CGS) -> IndexMap<String, Value> {
        if self.inherited.bindings().is_empty() {
            self.mat.bindings_for(get, cgs)
        } else {
            self.inherited.bindings().clone()
        }
    }
}

/// A Get-backed relation uses the same scope and freshness proof as an authored Get.
/// A relation without a Get retains its observed row without promoting completeness.
pub(crate) enum PreparedRelationGet<'a> {
    Observed(CachedEntity),
    Fetch(ResolvedGet<'a>),
}

pub(crate) fn prepare_relation_get<'a>(
    get: &'a GetExpr,
    inherited: &CapabilityParamEnv,
    cgs: &'a CGS,
    mat: &SessionMaterialization,
    ambient: &ViewAmbientContext,
) -> Result<PreparedRelationGet<'a>, RuntimeError> {
    if get.capability_name.is_none()
        && cgs
            .find_capability(
                get.reference.entity_type.as_str(),
                plasm_core::CapabilityKind::Get,
            )
            .is_none()
    {
        if let Some(row) = mat.get(&get.reference) {
            return Ok(PreparedRelationGet::Observed(row.clone()));
        }
    }
    let ambient = ambient
        .clone()
        .with_capability_params(inherited.bindings().clone());
    let bindings = RelationGetBindings { mat, inherited };
    let resolved = ResolvedGet::resolve(
        get,
        cgs,
        get.capability_name.as_deref(),
        &bindings,
        &ambient,
        GetPurpose::Hydration,
    )?;
    if (get.capability_name.is_none() && inherited.bindings().is_empty())
        || mat.get_observations.contains_key(&get.reference)
    {
        if let Some(row) = resolved.cached_row(mat, cgs) {
            return Ok(PreparedRelationGet::Observed(row.clone()));
        }
    }
    Ok(PreparedRelationGet::Fetch(resolved))
}

#[derive(Clone, PartialEq)]
struct RelationGet {
    get: GetExpr,
    inherited: CapabilityParamEnv,
}

impl ExecutionEngine {
    /// Resolve each parent occurrence before coalescing equivalent detail requests.
    async fn resolve_relation_occurrences(
        &self,
        occurrences: Vec<RelationGet>,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
    ) -> Result<(Vec<CachedEntity>, usize, usize, bool), RuntimeError> {
        use futures_util::stream::{self, StreamExt};

        let mut unique: Vec<RelationGet> = Vec::new();
        let mut by_reference: std::collections::HashMap<Ref, Vec<usize>> = Default::default();
        let positions: Vec<usize> = occurrences
            .into_iter()
            .map(|occurrence| {
                let candidates = by_reference
                    .entry(occurrence.get.reference.clone())
                    .or_default();
                if let Some(index) = candidates
                    .iter()
                    .copied()
                    .find(|index| unique[*index] == occurrence)
                {
                    index
                } else {
                    let index = unique.len();
                    unique.push(occurrence);
                    candidates.push(index);
                    index
                }
            })
            .collect();
        let concurrency = self
            .config
            .effective_hydrate_concurrency(cgs.entry_id.as_deref());
        let (seed, _) = crate::BranchMaterializationBase::fork_from(mat);
        let mut stream = stream::iter(unique.into_iter().map(|RelationGet { get, inherited }| {
            let mut branch = seed.clone();
            async move {
                let reference = get.reference.clone();
                let ambient = ViewAmbientContext::default()
                    .with_capability_params(inherited.bindings().clone());
                let request = match prepare_relation_get(&get, &inherited, cgs, &branch, &ambient)?
                {
                    PreparedRelationGet::Observed(row) => {
                        return Ok::<_, RuntimeError>((row, ExecutionSource::Cache, None))
                    }
                    PreparedRelationGet::Fetch(request) => request,
                };
                let (row, source) = self
                    .fetch_resolved_get_decoded(&request, cgs, mode, &mut branch)
                    .await
                    .map_err(|error| {
                        wrap_synthesized_get_error(
                            request.capability.name.as_str(),
                            reference.entity_type.as_str(),
                            error,
                        )
                    })?;
                if row.reference != reference {
                    return Err(wrap_synthesized_get_error(
                        request.capability.name.as_str(),
                        reference.entity_type.as_str(),
                        RuntimeError::GetIdentityMismatch {
                            expected: Box::new(reference.clone()),
                            actual: Box::new(row.reference.clone()),
                        },
                    ));
                }
                request.record(&mut branch, cgs, row.clone())?;
                Ok((row, source, Some(branch)))
            }
        }))
        .buffered(concurrency);
        let mut rows = Vec::new();
        let mut network = 0;
        let mut hits = 0;
        let mut live = false;
        while let Some(result) = stream.next().await {
            cooperative_cancel_check()?;
            let (row, source, branch) = result?;
            if let Some(branch) = branch {
                mat.absorb_branch(branch)?;
                if source == ExecutionSource::Live {
                    network += 1;
                    live = true;
                } else {
                    hits += 1;
                }
            } else {
                hits += 1;
            }
            rows.push(row);
        }
        Ok((
            positions
                .into_iter()
                .map(|index| rows[index].clone())
                .collect(),
            network,
            hits,
            live,
        ))
    }

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

        self.execute_chain_materialized(chain, source_result, cgs, mat, mode, consume, opts)
            .await
    }

    /// Navigate an already observed rowset without repeating its source request.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_chain_materialized(
        &self,
        chain: &plasm_core::ChainExpr,
        source_result: ExecutionResult,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> Result<ExecutionResult, RuntimeError> {
        source_result.collection.materialize(Demand::Observed)?;
        if source_result.count() == 0 {
            return Ok(ExecutionResult {
                collection: source_result.collection.flat_map(&"empty_chain", &[])?,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats,
                request_fingerprints: source_result.request_fingerprints.clone(),
                operations: source_result.operations.clone(),
            });
        }

        let source_entity_name_owned: String = if matches!(chain.source.as_ref(), Expr::Chain(_)) {
            source_result
                .entities()
                .first()
                .map(|e| e.reference.entity_type.to_string())
                .unwrap_or_else(|| chain.source.primary_entity().to_string())
        } else {
            chain.source.primary_entity().to_string()
        };
        let source_entity_name = source_entity_name_owned.as_str();
        let source_entity =
            cgs.get_entity(source_entity_name)
                .ok_or_else(|| RuntimeError::EntityUnknown {
                    entity: source_entity_name.to_string(),
                })?;

        // Resolve the target entity type — either from an EntityRef field or a
        // declared relation (cardinality-one decode, scoped query, embedded GET refs).
        let target_entity_name: String =
            if let Some(field_schema) = source_entity.fields.get(chain.selector.as_str()) {
                let nv = cgs
                    .named_value_for_slot(field_schema)
                    .map_err(RuntimeError::from)?;
                match &nv.field_type {
                    FieldType::EntityRef { target, .. } => target.to_string(),
                    _ => {
                        return Err(RuntimeError::Chain(crate::ChainError::FieldNotEntityRef {
                            entity: source_entity_name.to_string(),
                            field: chain.selector.to_string(),
                            actual: nv.field_type.clone(),
                        }));
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
                            RelationMaterialization::QueryScoped {
                                capability, param, ..
                            } => {
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
                                ..
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
                                return Err(RuntimeError::Chain(
                                    crate::ChainError::MaterializationMissing {
                                        entity: source_entity_name.to_string(),
                                        relation: chain.selector.to_string(),
                                    },
                                ));
                            }
                            RelationMaterialization::GetScopedBindings { .. } => {
                                return Err(RuntimeError::Chain(
                                    crate::ChainError::GetBindingsCardinality {
                                        entity: source_entity_name.to_string(),
                                        relation: chain.selector.to_string(),
                                    },
                                ));
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
                                return Err(RuntimeError::Chain(
                                    crate::ChainError::QueryBindingsCardinality {
                                        entity: source_entity_name.to_string(),
                                        relation: chain.selector.to_string(),
                                    },
                                ));
                            }
                            Some(RelationMaterialization::PreferFromParentGet { .. })
                            | Some(RelationMaterialization::ViewEmbed { .. }) => {
                                return Err(RuntimeError::Chain(
                                    crate::ChainError::ParentEmbeddingCardinality {
                                        entity: source_entity_name.to_string(),
                                        relation: chain.selector.to_string(),
                                    },
                                ));
                            }
                            Some(RelationMaterialization::Unavailable) | None => {}
                        }
                        rel.target_resource.to_string()
                    }
                }
            } else {
                return Err(RuntimeError::Chain(crate::ChainError::SelectorMissing {
                    entity: source_entity_name.to_string(),
                    selector: chain.selector.to_string(),
                }));
            };

        // ── Extract ref IDs from source entities ─────────────────────────
        let ref_ids: Vec<Option<String>> = source_result
            .entities()
            .iter()
            .map(|e| extract_ref_id(e, &chain.selector, cgs))
            .collect();

        // ── Explicit continuation: no batching, dispatch per-entity ──────
        if matches!(chain.step, ChainStep::Explicit { .. }) {
            let mut children = Vec::new();
            let mut operations = source_result.operations.clone();
            let mut total_network = source_result.stats.network_requests;
            let mut total_cache_hits = source_result.stats.cache_hits;
            let mut any_live = source_result.source == ExecutionSource::Live;

            if let ChainStep::Explicit { expr } = &chain.step {
                for (position, id_opt) in ref_ids.iter().enumerate() {
                    if id_opt.is_none() {
                        children.push(ExecutionCollection::observe(
                            source_result.collection.membership().identity().derived(&(
                                "absent_reference",
                                &chain.selector,
                                position,
                            ))?,
                            vec![],
                            Observation::ExactOutput { decoded: 0 },
                        )?);
                        continue;
                    }
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
                    operations.merge(&r.operations);
                    children.push(r.collection);
                }
            }

            let collection = source_result.collection.flat_map(chain, &children)?;
            let count = collection.count();
            return Ok(ExecutionResult {
                collection,
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
                operations,
            });
        }

        let occurrences = source_result
            .entities()
            .iter()
            .zip(&ref_ids)
            .filter_map(|(parent, id)| {
                id.as_ref().map(|id| RelationGet {
                    get: GetExpr::from_ref(Ref::new(&target_entity_name, id.as_str())),
                    inherited: CapabilityParamEnv::from_source_row(
                        cgs,
                        &target_entity_name,
                        mat,
                        parent,
                    ),
                })
            })
            .collect();
        let (resolved, extra_network, cached_hits, child_live) = self
            .resolve_relation_occurrences(occurrences, cgs, mat, mode)
            .await?;
        let any_live = source_result.source == ExecutionSource::Live || child_live;

        let count = resolved.len();
        Ok(ExecutionResult {
            collection: ExecutionCollection::evaluate(
                crate::execution::RowMaterialization::Catalog,
                source_result
                    .collection
                    .membership()
                    .identity()
                    .derived(chain)?,
                &[&source_result.collection],
                resolved.into(),
            )?,
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
            operations: source_result.operations.clone(),
        })
    }

    /// Parallel scoped-query fanout; `network_jobs` must already list `(parent_index, query)`.
    #[allow(clippy::too_many_arguments)] // Mirrors the scoped-query execution context below.
    async fn fanout_scoped_query_parallel(
        &self,
        source_result: &ExecutionResult,
        mut per_parent: Vec<Option<ExecutionCollection>>,
        network_jobs: Vec<(usize, QueryExpr)>,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        select_entity: Option<&plasm_core::EntityName>,
    ) -> Result<ExecutionResult, RuntimeError> {
        use futures_util::stream::{self, StreamExt};

        let mut total_network = source_result.stats.network_requests;
        let mut merged_stats = ExecutionStats::default();
        merged_stats.merge_telemetry(&source_result.stats.cache);
        let mut any_live = source_result.source == ExecutionSource::Live;
        let graph_hits = per_parent
            .iter()
            .flatten()
            .map(ExecutionCollection::count)
            .sum::<usize>();

        if !network_jobs.is_empty() {
            let concurrency = self
                .config
                .effective_hydrate_concurrency(cgs.entry_id.as_deref());
            let branch_seed = {
                let snap = mat.snapshot();
                SessionMaterialization::seed_read_branch(mat, snap.into_graph())
            };

            let mut stream = stream::iter(network_jobs.into_iter().map(|(parent_idx, q)| {
                let mut branch = branch_seed.clone();
                Box::pin(async move {
                    let result = Box::pin(self.execute_query(
                        &q,
                        cgs,
                        &mut branch,
                        mode,
                        StreamConsumeOpts {
                            fetch_all: select_entity.is_some(),
                            ..Default::default()
                        },
                        &ViewAmbientContext::default(),
                    ))
                    .await?;
                    Ok::<_, RuntimeError>((parent_idx, result, branch))
                })
            }))
            .buffer_unordered(concurrency);

            while let Some(res) = stream.next().await {
                cooperative_cancel_check()?;
                let (parent_idx, mut result, branch) = res?;
                if let Some(selected) = select_entity {
                    if result.has_more || result.pagination_resume.is_some() {
                        return Err(crate::ChainError::SelectedRelationIncomplete {
                            entity: selected.to_string(),
                        }
                        .into());
                    }
                    result.collection.materialize(Demand::Observed)?;
                    let retained: Vec<_> = result
                        .entities()
                        .iter()
                        .enumerate()
                        .filter_map(|(index, entity)| {
                            (entity.reference.entity_type == *selected).then_some(index)
                        })
                        .collect();
                    result.collection = result.collection.filter(
                        &("relation_select_entity", selected),
                        &retained,
                        &[],
                    )?;
                }
                mat.absorb_branch(branch)?;
                if result.source == ExecutionSource::Live {
                    any_live = true;
                }
                total_network += result.stats.network_requests;
                merged_stats.merge_telemetry(&result.stats.cache);
                // At most one scoped query per parent today; order within parent = query row order.
                if per_parent[parent_idx].replace(result.collection).is_some() {
                    return Err(plasm_core::collection_codec::CollectionFault::Conservation.into());
                }
            }
        }

        let children = per_parent
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or(plasm_core::collection_codec::CollectionFault::Conservation)?;
        let collection = source_result
            .collection
            .flat_map(&"scoped_query_fanout", &children)?;
        let count = collection.count();
        merged_stats.cache_hits = merged_stats
            .cache
            .legacy_cache_hits()
            .saturating_add(graph_hits);
        merged_stats.cache_misses = merged_stats.cache.legacy_cache_misses();
        merged_stats.network_requests = total_network;
        merged_stats.record_rows_materialized(count);
        Ok(ExecutionResult {
            collection,
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
            operations: source_result.operations.clone(),
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
            RuntimeError::CapabilityNotFound {
                capability: capability.to_string(),
                entity: target_key.to_string(),
            }
        })?;
        if rel
            .materialize
            .as_ref()
            .and_then(|mat| mat.selected_entity())
            .is_none()
            && cap.domain.as_str() != target_key
        {
            return Err(RuntimeError::Chain(
                crate::ChainError::CapabilityDomainMismatch {
                    capability: capability.to_string(),
                    expected: target_key.to_string(),
                    actual: cap.domain.to_string(),
                },
            ));
        }
        let capability_name = cap.name.clone();

        source_result.collection.materialize(Demand::Observed)?;
        if source_result.count() == 0 {
            return Ok(ExecutionResult {
                collection: source_result.collection.flat_map(&"empty_chain", &[])?,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats.clone(),
                request_fingerprints: source_result.request_fingerprints.clone(),
                operations: source_result.operations.clone(),
            });
        }

        let via = via_param.clone();
        let mut network_jobs = Vec::new();
        for (i, entity) in source_result.entities().iter().enumerate() {
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
            let mut q = QueryExpr::filtered(cap.domain.clone(), pred);
            q.capability_name = Some(capability_name.clone());
            relation_inherit_for_scoped_query(cgs, mat, entity, cap, &q)?
                .apply_to_scoped_query(&mut q);
            network_jobs.push((i, q));
        }
        let per_parent = vec![None; source_result.count()];
        self.fanout_scoped_query_parallel(
            source_result,
            per_parent,
            network_jobs,
            cgs,
            mat,
            mode,
            rel.materialize
                .as_ref()
                .and_then(|mat| mat.selected_entity()),
        )
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
            RuntimeError::CapabilityNotFound {
                capability: capability.to_string(),
                entity: target_key.to_string(),
            }
        })?;
        if rel
            .materialize
            .as_ref()
            .and_then(|mat| mat.selected_entity())
            .is_none()
            && cap.domain.as_str() != target_key
        {
            return Err(RuntimeError::Chain(
                crate::ChainError::CapabilityDomainMismatch {
                    capability: capability.to_string(),
                    expected: target_key.to_string(),
                    actual: cap.domain.to_string(),
                },
            ));
        }

        source_result.collection.materialize(Demand::Observed)?;
        if source_result.count() == 0 {
            return Ok(ExecutionResult {
                collection: source_result.collection.flat_map(&"empty_chain", &[])?,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats.clone(),
                request_fingerprints: source_result.request_fingerprints.clone(),
                operations: source_result.operations.clone(),
            });
        }

        let capability_name = cap.name.clone();
        let cap_params: Vec<_> = cap.query_surface_fields().cloned().collect();
        let parent_def = parent_entity_def;
        let binds = bindings;

        let mut network_jobs = Vec::new();
        for (i, entity) in source_result.entities().iter().enumerate() {
            let preds: Vec<Predicate> = binds
                .iter()
                .map(|(cap_param, parent_field)| {
                    let raw = chain_binding_raw_json(entity, parent_def, parent_field, cgs)?;
                    let value =
                        chain_binding_plasm_value(&raw, cap_param.as_str(), &cap_params, cgs)?;
                    Ok(Predicate::eq(cap_param.as_str(), value))
                })
                .collect::<Result<_, RuntimeError>>()?;
            let pred = if preds.len() == 1 {
                preds.into_iter().next().expect("non-empty preds")
            } else {
                Predicate::and(preds)
            };
            let mut q = QueryExpr::filtered(cap.domain.clone(), pred);
            q.capability_name = Some(capability_name.clone());
            relation_inherit_for_scoped_query(cgs, mat, entity, cap, &q)?
                .apply_to_scoped_query(&mut q);
            network_jobs.push((i, q));
        }
        let per_parent = vec![None; source_result.count()];
        self.fanout_scoped_query_parallel(
            source_result,
            per_parent,
            network_jobs,
            cgs,
            mat,
            mode,
            rel.materialize
                .as_ref()
                .and_then(|mat| mat.selected_entity()),
        )
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
            return Err(RuntimeError::Chain(
                crate::ChainError::PreferParentGetRequired {
                    relation: rel.name.to_string(),
                },
            ));
        };

        source_result.collection.materialize(Demand::Observed)?;
        if source_result.count() == 0 {
            return Ok(ExecutionResult {
                collection: source_result.collection.flat_map(&"empty_chain", &[])?,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats.clone(),
                request_fingerprints: source_result.request_fingerprints.clone(),
                operations: source_result.operations.clone(),
            });
        }

        let relation_key = rel.name.as_str();
        let expected_target = rel.target_resource.as_str();
        let target_entity = rel.target_resource.clone();
        let all_embed = source_result
            .entities()
            .iter()
            .map(|parent| {
                let parent_json = parent.to_row_values(None)?;
                Ok(matches!(
                    resolve_relation_row_resolution(
                        rel.materialize.as_ref().expect("prefer"),
                        rel.cardinality,
                        relation_key,
                        expected_target,
                        &parent_json,
                        parent
                            .relations
                            .get(relation_key)
                            .map(|v| v.iter().cloned().collect::<Vec<_>>())
                            .as_deref(),
                        |r| mat.get(r).is_some(),
                    ),
                    RelationRowResolution::EmbeddedRefs(_)
                ))
            })
            .collect::<Result<Vec<_>, plasm_core::ValueCodecError>>()?
            .into_iter()
            .all(|embedded| embedded);
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
            source_result.entities(),
            rel.materialize.as_ref().expect("prefer"),
            relation_key,
            expected_target,
            mat,
            parent_entity_def,
            cgs,
            &target_entity,
            fallback,
        )?;
        self.fanout_scoped_query_parallel(
            source_result,
            per_parent,
            network_jobs,
            cgs,
            mat,
            mode,
            None,
        )
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
        let target_key = target_entity.as_str();
        let cap = cgs.get_capability(capability.as_str()).ok_or_else(|| {
            RuntimeError::CapabilityNotFound {
                capability: capability.to_string(),
                entity: target_key.to_string(),
            }
        })?;
        if cap.domain.as_str() != target_key {
            return Err(RuntimeError::Chain(
                crate::ChainError::CapabilityDomainMismatch {
                    capability: capability.to_string(),
                    expected: target_key.to_string(),
                    actual: cap.domain.to_string(),
                },
            ));
        }

        let target_ent = cgs
            .get_entity(target_key)
            .ok_or_else(|| RuntimeError::EntityUnknown {
                entity: target_key.to_string(),
            })?;

        let mut gets = Vec::new();
        for entity in source_result.entities() {
            let mut bound: IndexMap<String, String> = IndexMap::new();
            for (cap_param, parent_field) in bindings.iter() {
                bound.insert(
                    cap_param.to_string(),
                    chain_binding_value(entity, parent_entity_def, parent_field, cgs)?,
                );
            }
            let reference = ref_from_materialize_bindings_for_get_chain(target_ent, &bound)?;
            let inherit = CapabilityParamEnv::from_source_row_for_cap(cgs, mat, entity, cap);
            let mut get = GetExpr::from_ref(reference);
            get.capability_name = Some(capability.clone());
            gets.push(RelationGet {
                get,
                inherited: inherit,
            });
        }

        if gets.is_empty() {
            return Ok(ExecutionResult {
                collection: source_result.collection.flat_map(&"empty_chain", &[])?,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats.clone(),
                request_fingerprints: source_result.request_fingerprints.clone(),
                operations: source_result.operations.clone(),
            });
        }

        let total_network = source_result.stats.network_requests;
        let total_cache_hits = source_result.stats.cache_hits;
        let (all_entities, extra_network, extra_cache_hits, child_live) = self
            .resolve_relation_occurrences(gets, cgs, mat, mode)
            .await?;
        let any_live = source_result.source == ExecutionSource::Live || child_live;
        let count = all_entities.len();
        Ok(ExecutionResult {
            collection: ExecutionCollection::evaluate(
                crate::execution::RowMaterialization::Catalog,
                source_result
                    .collection
                    .membership()
                    .identity()
                    .derived(&(capability, bindings))?,
                &[&source_result.collection],
                all_entities.into(),
            )?,
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
            operations: source_result.operations.clone(),
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
        let relation_key = relation.name.as_str();
        let expected_target = &relation.target_resource;

        let mut embedded_children = Vec::new();
        for parent in source_result.entities().iter() {
            let refs = parent.relations.get(relation_key).ok_or_else(|| {
                crate::CacheError::RelationUnobserved {
                    reference: parent.reference.clone(),
                    relation: relation_key.to_owned(),
                }
            })?;
            let record = refs.record().clone();
            embedded_children.push(ExecutionCollection::graph(record));
        }
        let embedded = source_result
            .collection
            .flat_map(relation, &embedded_children)?;
        let mut ordered_refs: Vec<Ref> = Vec::new();
        for e in source_result.entities() {
            if let Some(refs) = e.relations.get(relation_key) {
                for r in refs {
                    if r.entity_type != *expected_target {
                        return Err(RuntimeError::TraversalParentTypeMismatch {
                            expected: expected_target.to_string(),
                            actual: r.entity_type.to_string(),
                        });
                    }
                    ordered_refs.push(r.clone());
                }
            }
        }

        if ordered_refs.is_empty() {
            return Ok(ExecutionResult {
                collection: embedded.with_materialization(vec![].into())?,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats.clone(),
                request_fingerprints: source_result.request_fingerprints.clone(),
                operations: source_result.operations.clone(),
            });
        }

        if matches!(chain_step, ChainStep::Explicit { .. }) {
            let mut children = Vec::new();
            let mut operations = source_result.operations.clone();
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
                    operations.merge(&res.operations);
                    children.push(res.collection);
                }
            }

            let collection = embedded.flat_map(chain_step, &children)?;
            let count = collection.count();
            return Ok(ExecutionResult {
                collection,
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
                operations,
            });
        }

        let mut occurrences = Vec::with_capacity(ordered_refs.len());
        for parent in source_result.entities() {
            if let Some(refs) = parent.relations.get(relation_key) {
                let inherited =
                    CapabilityParamEnv::from_source_row(cgs, expected_target.as_str(), mat, parent);
                occurrences.extend(refs.iter().cloned().map(|reference| RelationGet {
                    get: GetExpr::from_ref(reference),
                    inherited: inherited.clone(),
                }));
            }
        }
        let (resolved, extra_network, cached_hits, child_live) = self
            .resolve_relation_occurrences(occurrences, cgs, mat, mode)
            .await?;
        let any_live = source_result.source == ExecutionSource::Live || child_live;

        let count = resolved.len();
        Ok(ExecutionResult {
            collection: embedded.with_materialization(resolved.into())?,
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
            operations: source_result.operations.clone(),
        })
    }
}

#[cfg(test)]
#[path = "chain_boundary_tests.rs"]
mod boundary_tests;
