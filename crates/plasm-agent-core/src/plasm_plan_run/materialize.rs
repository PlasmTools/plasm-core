//! Plan node and relation materialization.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn materialize_synthetic_node(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node: &ValidatedPlanNode,
    entry_id: &str,
    entity_override: Option<&str>,
    rows: Vec<serde_json::Value>,
    row_identities: Vec<Option<plasm_core::RowIdentity>>,
    trace: Option<&PlasmTraceContext>,
) -> Result<MaterializedNode, String> {
    let entity = entity_override
        .map(str::to_string)
        .unwrap_or_else(|| match node {
            ValidatedPlanNode::Compute(compute) => compute
                .compute
                .schema
                .entity
                .clone()
                .unwrap_or_else(|| format!("PlanComputed_{}", node.id().as_str())),
            _ => format!("PlanComputed_{}", node.id().as_str()),
        });
    let full_entities = json_rows_to_entities(&entity, &rows);
    let request_fingerprints = vec![compute_fingerprint(node, &rows)];
    let full_result = ExecutionResult {
        count: full_entities.len(),
        entities: full_entities.clone(),
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats: ExecutionStats {
            duration_ms: 0,
            network_requests: 0,
            cache_hits: 0,
            cache_misses: 0,
            ..Default::default()
        },
        request_fingerprints: request_fingerprints.clone(),
    };
    let parsed_preimage = evidence_plan::parsed_expr_for_plan_node(node);
    let artifact = archive_plasm_result_snapshot(
        st,
        es,
        session_id,
        Some(entry_id),
        vec![synthetic_node_display(node)],
        &parsed_preimage,
        &full_result,
        trace,
    )
    .await?;
    let page_size = match node {
        ValidatedPlanNode::Compute(compute) => compute.compute.page_size.unwrap_or(50),
        _ => 50,
    };
    let (entities, has_more, paging_handle) = if full_entities.len() > page_size {
        let first = full_entities[..page_size].to_vec();
        let handle = es.register_synthetic_paging_continuation(
            crate::execute_session::SyntheticPageCursor {
                node_id: node.id().as_str().to_string(),
                entity_type: entity.clone(),
                rows: full_entities,
                offset: page_size,
                page_size,
                request_fingerprints: request_fingerprints.clone(),
            },
            trace.and_then(|t| t.logical_session_ref.as_deref()),
        );
        (first, true, Some(handle))
    } else {
        (full_result.entities.clone(), false, None)
    };
    Ok(MaterializedNode {
        entry_id: entry_id.to_string(),
        entity: entity.clone(),
        display: synthetic_node_display(node),
        projection: synthetic_projection(node),
        row_source: inline_row_source_owned(rows),
        row_identities,
        result: Arc::new(ExecutionResult {
            count: entities.len(),
            entities,
            has_more,
            pagination_resume: None,
            paging_handle,
            source: ExecutionSource::Cache,
            stats: full_result.stats,
            request_fingerprints,
        }),
        artifact: Some(artifact),
    })
}

pub(crate) fn synthetic_node_display(node: &ValidatedPlanNode) -> String {
    match node {
        ValidatedPlanNode::Data(_) => format!("plan.data({})", node.id().as_str()),
        ValidatedPlanNode::Derive(_) => format!("plan.derive({})", node.id().as_str()),
        ValidatedPlanNode::Compute(_) => format!("plan.compute({})", node.id().as_str()),
        ValidatedPlanNode::RelationTraversal(_) => {
            format!("plan.relation({})", node.id().as_str())
        }
        _ => format!("plan.stage({})", node.id().as_str()),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn materialize_validated_relation_traversal(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    idx: usize,
    node: &ValidatedPlanNode,
    relation: &ValidatedRelationTraversalNode,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    trace: Option<&PlasmTraceContext>,
    sink: Option<&McpPlasmTraceSink>,
    plan_shared: Option<&crate::plan_execute_shared::PlanLineExecuteShared>,
) -> Result<MaterializedNode, String> {
    let source_mat = materialized.get(&relation.relation.source).ok_or_else(|| {
        format!(
            "relation source node {:?} has not been materialized",
            relation.relation.source.as_str()
        )
    })?;
    let source_rows =
        crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, es.cgs.as_ref())
            .resolve_row_source_rows(&source_mat.row_source)
            .await?;
    match &relation.relation.materialize {
        RelationMaterialization::FromParentGet { .. } => try_materialize_from_parent_get_relation(
            st,
            es,
            session_id,
            node,
            relation,
            source_mat,
            &source_rows,
            trace,
        )
        .await?
        .ok_or_else(|| {
            format!(
                "relation `{}` FromParentGet materialize failed",
                relation.relation.relation
            )
        }),
        RelationMaterialization::PreferFromParentGet { .. } => {
            materialize_prefer_from_parent_get_relation(
                st,
                es,
                session_id,
                idx,
                node,
                relation,
                source_mat,
                &source_rows,
                materialized,
                trace,
                sink,
                plan_shared,
            )
            .await
        }
        RelationMaterialization::QueryScoped { .. }
        | RelationMaterialization::QueryScopedBindings { .. } => {
            if matches!(
                relation.relation.source_cardinality,
                RelationSourceCardinality::Many
            ) {
                materialize_relation_scoped_fanout(
                    st,
                    es,
                    session_id,
                    idx,
                    node,
                    relation,
                    source_mat,
                    &source_rows,
                    materialized,
                    trace,
                    sink,
                    plan_shared,
                )
                .await
            } else {
                if matches!(
                    relation.relation.source_cardinality,
                    RelationSourceCardinality::RuntimeCheckedSingleton
                ) && source_rows.len() != 1
                {
                    return Err(singleton_input_row_count_error(
                        relation.relation.source.as_str(),
                        "source",
                        source_rows.len(),
                        "relation traversal",
                    ));
                }
                materialize_relation_singleton_chain(
                    st,
                    es,
                    session_id,
                    idx,
                    relation,
                    materialized,
                    trace,
                    sink,
                    plan_shared,
                )
                .await
            }
        }
        RelationMaterialization::GetScopedBindings { .. } => {
            if matches!(
                relation.relation.source_cardinality,
                RelationSourceCardinality::RuntimeCheckedSingleton
            ) && source_rows.len() != 1
            {
                return Err(singleton_input_row_count_error(
                    relation.relation.source.as_str(),
                    "source",
                    source_rows.len(),
                    "relation traversal",
                ));
            }
            materialize_relation_singleton_chain(
                st,
                es,
                session_id,
                idx,
                relation,
                materialized,
                trace,
                sink,
                plan_shared,
            )
            .await
        }
        RelationMaterialization::Unavailable => {
            if let Some(mat) = try_materialize_from_cached_relation_refs(
                st, es, session_id, node, relation, source_mat, trace,
            )
            .await?
            {
                finalize_typed_relation_materialized_node(
                    st,
                    es,
                    session_id,
                    &relation.relation.target,
                    mat,
                    trace,
                )
                .await
            } else if matches!(
                relation.relation.cardinality,
                crate::plasm_plan::RelationCardinality::One
            ) {
                materialize_relation_singleton_chain(
                    st,
                    es,
                    session_id,
                    idx,
                    relation,
                    materialized,
                    trace,
                    sink,
                    plan_shared,
                )
                .await
            } else {
                Err(format!(
                    "relation `{}` on `{}` has no materialize strategy (Unavailable)",
                    relation.relation.relation, source_mat.entity
                ))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn try_materialize_from_parent_get_relation(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node: &ValidatedPlanNode,
    relation: &ValidatedRelationTraversalNode,
    source_mat: &MaterializedNode,
    source_rows: &[serde_json::Value],
    trace: Option<&PlasmTraceContext>,
) -> Result<Option<MaterializedNode>, String> {
    let cgs = match crate::catalog_ownership::resolve_cgs_for_entity(
        es,
        source_mat.entity.as_str(),
        None,
    ) {
        Ok(cgs) => cgs,
        Err(_) => return Ok(None),
    };
    let ent = cgs
        .get_entity(source_mat.entity.as_str())
        .ok_or_else(|| format!("unknown source entity `{}`", source_mat.entity))?;
    let rel_schema = ent
        .relations
        .get(relation.relation.relation.as_str())
        .ok_or_else(|| {
            format!(
                "entity `{}` has no relation `{}`",
                source_mat.entity, relation.relation.relation
            )
        })?;
    let path = match &relation.relation.materialize {
        RelationMaterialization::FromParentGet { path }
        | RelationMaterialization::PreferFromParentGet { path, .. } => path,
        _ => return Ok(None),
    };
    if path.is_empty() {
        return Err(format!(
            "relation `{}` on `{}` declares from_parent_get with an empty path",
            relation.relation.relation, source_mat.entity
        ));
    }
    if let Some(mat) = try_materialize_from_cached_relation_refs(
        st, es, session_id, node, relation, source_mat, trace,
    )
    .await?
    {
        return Ok(Some(mat));
    }
    let rows = flatten_from_parent_get_source_rows(source_rows, path, rel_schema.cardinality);
    let target = relation.relation.target.entity.as_str();
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target))?;
    let entities = json_rows_to_entities_with_refs(target, &rows, Some(scoped_es.cgs.as_ref()));
    let request_fingerprints = vec![compute_fingerprint(node, &rows)];
    let full_result = ExecutionResult {
        count: entities.len(),
        entities: entities.clone(),
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats: ExecutionStats {
            duration_ms: 0,
            network_requests: 0,
            cache_hits: 0,
            cache_misses: 0,
            ..Default::default()
        },
        request_fingerprints: request_fingerprints.clone(),
    };
    let parsed_preimage = evidence_plan::parsed_expr_for_plan_node(node);
    let artifact = archive_plasm_result_snapshot(
        st,
        es,
        session_id,
        Some(relation.relation.target.entry_id.as_str()),
        vec![synthetic_node_display(node)],
        &parsed_preimage,
        &full_result,
        trace,
    )
    .await?;
    let row_identities = row_identities_from_entities(&scoped_es, target, &full_result.entities);
    let display = relation
        .relation
        .ir
        .display_expr
        .clone()
        .unwrap_or_else(|| format!("plan.relation({})", node.id().as_str()));
    finalize_typed_relation_materialized_node(
        st,
        es,
        session_id,
        &relation.relation.target,
        MaterializedNode {
            entry_id: relation.relation.target.entry_id.clone(),
            entity: relation.relation.target.entity.clone(),
            display,
            projection: relation.relation.ir.projection.clone(),
            row_source: inline_row_source(&[]),
            row_identities,
            result: Arc::new(full_result),
            artifact: Some(artifact),
        },
        trace,
    )
    .await
    .map(Some)
}

/// PreferFromParentGet: embed from graph when possible; otherwise per-row scoped GET via
/// [`run_parsed_plasm_line`] (graph branch fork/commit — session mutex not held during HTTP).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn materialize_prefer_from_parent_get_relation(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node_index: usize,
    node: &ValidatedPlanNode,
    relation: &ValidatedRelationTraversalNode,
    source_mat: &MaterializedNode,
    source_rows: &[serde_json::Value],
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    trace: Option<&PlasmTraceContext>,
    sink: Option<&McpPlasmTraceSink>,
    plan_shared: Option<&crate::plan_execute_shared::PlanLineExecuteShared>,
) -> Result<MaterializedNode, String> {
    let RelationMaterialization::PreferFromParentGet { path, .. } = &relation.relation.materialize
    else {
        return Err(format!(
            "relation `{}` expected PreferFromParentGet materialize",
            relation.relation.relation
        ));
    };
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target))?;
    let rel_name = relation.relation.relation.as_str();
    let target_entity = relation.relation.target.entity.as_str();
    let source_entity_def = scoped_es
        .cgs
        .entities
        .values()
        .find(|ent| ent.relations.contains_key(rel_name))
        .ok_or_else(|| {
            format!("no catalog entity declares relation `{rel_name}` for prefer embed")
        })?;
    let rel_schema = source_entity_def
        .relations
        .get(rel_name)
        .ok_or_else(|| format!("relation `{rel_name}` missing on catalog entity"))?;
    let all_wire_embedded = !source_rows.is_empty()
        && source_rows.iter().all(|row| {
            !flatten_from_parent_get_source_rows(
                std::slice::from_ref(row),
                path,
                rel_schema.cardinality,
            )
            .is_empty()
        });
    if all_wire_embedded {
        if let Some(node) = try_materialize_from_parent_get_relation(
            st,
            es,
            session_id,
            node,
            relation,
            source_mat,
            source_rows,
            trace,
        )
        .await?
        {
            return Ok(node);
        }
    }
    let parents = &source_mat.result.entities;
    let all_embedded_entities: Option<Vec<CachedEntity>> = {
        let graph = scoped_es.lock_graph_cache().await;
        let all_embed = parents.len() == source_rows.len()
            && parents.iter().zip(source_rows).all(|(parent, row)| {
                matches!(
                    resolve_relation_row_resolution(
                        &relation.relation.materialize,
                        rel_name,
                        target_entity,
                        row,
                        parent.relations.get(rel_name).map(|v| v.as_slice()),
                        |r| graph.get(r).is_some(),
                    ),
                    RelationRowResolution::EmbeddedRefs(_)
                )
            });
        if all_embed {
            collect_all_embedded_relation_targets(rel_name, target_entity, parents, &graph)
        } else {
            None
        }
    };
    if let Some(entities) = all_embedded_entities {
        let count = entities.len();
        let full_result = ExecutionResult {
            count,
            entities: entities.clone(),
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Cache,
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: 0,
                cache_hits: count,
                cache_misses: 0,
                ..Default::default()
            },
            request_fingerprints: vec![compute_fingerprint(node, source_rows)],
        };
        let parsed_preimage = evidence_plan::parsed_expr_for_plan_node(node);
        let artifact = archive_plasm_result_snapshot(
            st,
            es,
            session_id,
            Some(relation.relation.target.entry_id.as_str()),
            vec![format!(
                "plan.relation({}) prefer_embed_all",
                relation.id.as_str()
            )],
            &parsed_preimage,
            &full_result,
            trace,
        )
        .await?;
        let rows: Vec<_> = full_result
            .entities
            .iter()
            .map(|e| cached_entity_row_json(e, scoped_es.cgs.as_ref()))
            .collect();
        return finalize_typed_relation_materialized_node(
            st,
            es,
            session_id,
            &relation.relation.target,
            MaterializedNode {
                entry_id: relation.relation.target.entry_id.clone(),
                entity: relation.relation.target.entity.clone(),
                display: format!(
                    "plan.relation({}) prefer_from_parent_get (all embedded)",
                    relation.id.as_str()
                ),
                projection: relation.relation.ir.projection.clone(),
                row_source: inline_row_source_owned(rows),
                row_identities: row_identities_from_entities(
                    &scoped_es,
                    target_entity,
                    &full_result.entities,
                ),
                result: Arc::new(full_result),
                artifact: Some(artifact),
            },
            trace,
        )
        .await;
    }
    let pe = ParsedExpr {
        expr: relation.relation.ir.expr.clone(),
        projection: relation.relation.ir.projection.clone(),
    };
    let source_node = &relation.relation.source;
    let base_display = relation
        .relation
        .ir
        .display_expr
        .clone()
        .unwrap_or_else(|| format!("plan.relation({})", relation.id.as_str()));
    let resolutions: Vec<RelationRowResolution> = {
        let graph = scoped_es.lock_graph_cache().await;
        source_rows
            .iter()
            .enumerate()
            .map(|(row_index, source_row)| {
                let parent = parents.get(row_index);
                resolve_relation_row_resolution(
                    &relation.relation.materialize,
                    rel_name,
                    target_entity,
                    source_row,
                    parent
                        .and_then(|p| p.relations.get(rel_name))
                        .map(|v| v.as_slice()),
                    |r| graph.get(r).is_some(),
                )
            })
            .collect()
    };
    let mut entities = Vec::new();
    let mut request_fingerprints = Vec::new();
    let mut stats = ExecutionStats {
        duration_ms: 0,
        network_requests: 0,
        cache_hits: 0,
        cache_misses: 0,
        ..Default::default()
    };
    let mut source = ExecutionSource::Cache;
    for (row_index, resolution) in resolutions.iter().enumerate() {
        let source_row = &source_rows[row_index];
        match resolution {
            RelationRowResolution::EmbeddedRefs(refs) => {
                let graph = scoped_es.lock_graph_cache().await;
                for r in refs {
                    let e = graph
                        .get(r)
                        .ok_or_else(|| format!("prefer embed: missing graph target {r}"))?
                        .clone();
                    entities.push(e);
                }
            }
            RelationRowResolution::ScopedQuery => {
                let wire_rows = flatten_from_parent_get_source_rows(
                    std::slice::from_ref(source_row),
                    path,
                    rel_schema.cardinality,
                );
                if !wire_rows.is_empty() {
                    let wire_entities = json_rows_to_entities_with_refs(
                        target_entity,
                        &wire_rows,
                        Some(scoped_es.cgs.as_ref()),
                    );
                    entities.extend(wire_entities);
                    continue;
                }
                let row_identity = source_mat
                    .row_identities
                    .get(row_index)
                    .and_then(|i| i.as_ref())
                    .cloned();
                let input_rows = materialized_result_use_inputs_with_source_row(
                    materialized,
                    &relation.uses_result,
                    source_node,
                    source_row,
                    row_identity,
                )?;
                let wire_coercion = wire_coercion_ctx_for_source_entity(
                    scoped_es.cgs.as_ref(),
                    source_mat.entity.as_str(),
                );
                let parsed = instantiate_parsed_expr_plan_inputs_with_rows(
                    pe.clone(),
                    &input_rows,
                    wire_coercion,
                )?;
                let trace_line_index = node_index
                    .checked_mul(1000)
                    .and_then(|base| base.checked_add(row_index))
                    .unwrap_or(node_index);
                let expr_label = format!("{base_display} [row {row_index}]");
                crate::execute_pipeline::PlasmPreflight::preflight_parsed_line(
                    &scoped_es,
                    &expr_label,
                    &parsed,
                )
                .map_err(|e| e.to_string())?;
                let (parsed, result, _artifact) = run_parsed_plasm_line(
                    &expr_label,
                    &scoped_es,
                    st,
                    session_id,
                    parsed,
                    trace,
                    trace_line_index as i64,
                    None,
                    None,
                    None,
                    Some(plasm_core::PreflightToken::VERIFIED),
                    plan_shared,
                )
                .await
                .map_err(|e| match e {
                    crate::http_execute::RunLineError::Parse(d)
                    | crate::http_execute::RunLineError::Normalize(d)
                    | crate::http_execute::RunLineError::Projection(d) => d,
                    crate::http_execute::RunLineError::Operation(_) => {
                        "operation continuation is not valid inside a plan surface node".to_string()
                    }
                    crate::http_execute::RunLineError::Runtime(e, src) => {
                        format!("{e}\nsource expression: {src}")
                    }
                    crate::http_execute::RunLineError::ArtifactSerialization(e) => {
                        format!("artifact serialization failed: {e}")
                    }
                    crate::http_execute::RunLineError::ArtifactPersist(d) => {
                        format!("run artifact persist failed: {d}")
                    }
                    crate::http_execute::RunLineError::StaleGraphEpoch { .. } => {
                        "session graph changed during concurrent execute; retry the request"
                            .to_string()
                    }
                })?;
                if let Some(sink) = sink {
                    trace_record_plasm_line(
                        sink,
                        trace_line_index,
                        &expr_label,
                        &parsed,
                        &result,
                        &scoped_es,
                    )
                    .await;
                }
                source = combine_execution_source(source, result.source);
                stats.duration_ms = stats.duration_ms.saturating_add(result.stats.duration_ms);
                stats.network_requests = stats
                    .network_requests
                    .saturating_add(result.stats.network_requests);
                stats.merge_telemetry(&result.stats.cache);
                stats.cache_hits = stats.cache.legacy_cache_hits();
                stats.cache_misses = stats.cache.legacy_cache_misses();
                request_fingerprints.extend(result.request_fingerprints);
                entities.extend(result.entities);
            }
        }
    }
    let count = entities.len();
    let full_result = ExecutionResult {
        count,
        entities: entities.clone(),
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source,
        stats,
        request_fingerprints: request_fingerprints.clone(),
    };
    let parsed_preimage = evidence_plan::parsed_expr_for_plan_node(node);
    let artifact = archive_plasm_result_snapshot(
        st,
        es,
        session_id,
        Some(relation.relation.target.entry_id.as_str()),
        vec![format!(
            "plan.relation({}) prefer_from_parent_get (mixed)",
            relation.id.as_str()
        )],
        &parsed_preimage,
        &full_result,
        trace,
    )
    .await?;
    let rows: Vec<_> = full_result
        .entities
        .iter()
        .map(|e| cached_entity_row_json(e, scoped_es.cgs.as_ref()))
        .collect();
    finalize_typed_relation_materialized_node(
        st,
        es,
        session_id,
        &relation.relation.target,
        MaterializedNode {
            entry_id: relation.relation.target.entry_id.clone(),
            entity: relation.relation.target.entity.clone(),
            display: format!(
                "plan.relation({}) prefer_from_parent_get",
                relation.id.as_str()
            ),
            projection: relation.relation.ir.projection.clone(),
            row_source: inline_row_source_owned(rows),
            row_identities: row_identities_from_entities(
                &scoped_es,
                target_entity,
                &full_result.entities,
            ),
            result: Arc::new(full_result),
            artifact: Some(artifact),
        },
        trace,
    )
    .await
}

pub(crate) async fn materialized_rows(
    es: &ExecuteSession,
    st: &PlasmHostState,
    session_id: &str,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    source: &PlanNodeId,
) -> Result<Vec<serde_json::Value>, String> {
    let mat = materialized.get(source).ok_or_else(|| {
        format!(
            "source node {:?} has not been materialized",
            source.as_str()
        )
    })?;
    crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, es.cgs.as_ref())
        .resolve_row_source_rows(&mat.row_source)
        .await
}

pub(crate) fn compute_needs_full_materialize(op: &ComputeOp) -> bool {
    matches!(
        op,
        ComputeOp::Sort { .. }
            | ComputeOp::GroupBy { .. }
            | ComputeOp::Aggregate { .. }
            | ComputeOp::DedupeBy { .. }
            | ComputeOp::Render { .. }
    )
}
