//! Plan node and relation materialization.
//!
//! **CEP-5:** relation paths resolve parent rows via [`GraphSurfaceRehydrator::resolve_source_parents`],
//! not `result.entities` alone when the source node is GraphBacked.

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
        ValidatedPlanNode::Compute(compute) => compute
            .compute
            .page_size
            .unwrap_or(crate::plan_read_bounds::DEFAULT_HOST_PAGE_SIZE),
        _ => crate::plan_read_bounds::DEFAULT_HOST_PAGE_SIZE,
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
    plan_shared: Option<Arc<crate::plan_execute_shared::PlanLineExecuteShared>>,
) -> Result<MaterializedNode, String> {
    let source_mat = materialized.get(&relation.relation.source).ok_or_else(|| {
        format!(
            "relation source node {:?} has not been materialized",
            relation.relation.source.as_str()
        )
    })?;
    let read_cap = crate::plan_read_bounds::effective_relation_read_cap(relation);
    let source_rows =
        crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, es.cgs.as_ref())
            .resolve_row_source_rows(&source_mat.row_source, read_cap)
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
                    read_cap,
                    plan_shared.clone(),
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
    if !ent
        .relations
        .contains_key(relation.relation.relation.as_str())
    {
        return Err(format!(
            "entity `{}` has no relation `{}`",
            source_mat.entity, relation.relation.relation
        ));
    }
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
    let target = relation.relation.target.entity.as_str();
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target))?;
    let rel_name = relation.relation.relation.as_str();
    let rehydrator = crate::graph_rehydrate::GraphSurfaceRehydrator::new(
        es,
        st,
        session_id,
        scoped_es.cgs.as_ref(),
    );
    let parents = source_mat
        .resolve_materialized_source_parents(&rehydrator)
        .await;
    let guard = scoped_es.lock_graph_cache().await;
    let mat = guard.materialization();
    let wire_fallback = super::compute_eval::parent_get_wire_rows(
        source_rows,
        relation,
        source_mat.entity.as_str(),
        scoped_es.cgs.as_ref(),
        target,
    )
    .ok()
    .filter(|rows| !rows.is_empty());
    let mut entities = super::compute_eval::resolve_embed_target_entities(
        rel_name,
        target,
        &parents,
        &mat,
        wire_fallback.as_deref(),
        scoped_es.cgs.as_ref(),
    );
    let read_cap = crate::plan_read_bounds::effective_relation_read_cap(relation);
    crate::plan_read_bounds::truncate_to_read_cap(&mut entities, read_cap);
    let wire_rows =
        crate::graph_rehydrate::wire_rows_for_embed_entities(&entities, scoped_es.cgs.as_ref(), &mat);
    drop(guard);
    let display = relation
        .relation
        .ir
        .display_expr
        .clone()
        .unwrap_or_else(|| format!("plan.relation({})", node.id().as_str()));
    super::compute_eval::finalize_embed_relation_materialized_node(
        st,
        es,
        session_id,
        node,
        relation,
        &scoped_es,
        target,
        entities,
        wire_rows,
        None,
        display,
        vec![synthetic_node_display(node)],
        trace,
        read_cap,
        0,
    )
    .await
    .map(Some)
}

pub(crate) use materialize_prefer::materialize_prefer_from_parent_get_relation;
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
        .resolve_row_source_rows(&mat.row_source, None)
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

#[must_use]
pub(crate) fn execution_result_from_fanout_fold(
    fold: super::plan_fanout_parallel::PlanLineExecutionFold,
) -> ExecutionResult {
    ExecutionResult {
        count: fold.entities.len(),
        entities: fold.entities,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: fold.source,
        stats: fold.stats,
        request_fingerprints: fold.request_fingerprints,
    }
}

#[must_use]
pub(crate) fn execution_result_from_relation_entities(
    entities: Vec<CachedEntity>,
    source: ExecutionSource,
    stats: ExecutionStats,
    request_fingerprints: Vec<String>,
) -> ExecutionResult {
    let count = entities.len();
    ExecutionResult {
        count,
        entities,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source,
        stats,
        request_fingerprints,
    }
}

fn relation_materialized_node_from_result(
    scoped_es: &ExecuteSession,
    relation: &ValidatedRelationTraversalNode,
    full_result: ExecutionResult,
    display: String,
    artifact: crate::run_artifacts::RunArtifactHandle,
) -> MaterializedNode {
    let rows: Vec<_> = full_result
        .entities
        .iter()
        .map(|e| cached_entity_row_json(e, scoped_es.cgs.as_ref()))
        .collect();
    MaterializedNode {
        entry_id: relation.relation.target.entry_id.clone(),
        entity: relation.relation.target.entity.clone(),
        display,
        projection: relation.relation.ir.projection.clone(),
        row_source: inline_row_source_owned(rows),
        row_identities: row_identities_from_entities(
            scoped_es,
            relation.relation.target.entity.as_str(),
            &full_result.entities,
        ),
        result: Arc::new(full_result),
        artifact: Some(artifact),
    }
}

/// Archive snapshot + build a relation `MaterializedNode`, optionally GET-hydrating rows.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn archive_materialize_relation(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    scoped_es: &ExecuteSession,
    relation: &ValidatedRelationTraversalNode,
    node: &ValidatedPlanNode,
    full_result: ExecutionResult,
    display: String,
    snapshot_label: String,
    trace: Option<&PlasmTraceContext>,
    hydrate: Option<(
        Option<usize>,
        Option<Arc<crate::plan_execute_shared::PlanLineExecuteShared>>,
    )>,
) -> Result<MaterializedNode, String> {
    let parsed_preimage = evidence_plan::parsed_expr_for_plan_node(node);
    let artifact = archive_plasm_result_snapshot(
        st,
        es,
        session_id,
        Some(relation.relation.target.entry_id.as_str()),
        vec![snapshot_label],
        &parsed_preimage,
        &full_result,
        trace,
    )
    .await?;
    let mat =
        relation_materialized_node_from_result(scoped_es, relation, full_result, display, artifact);
    let Some((read_cap, plan_shared)) = hydrate else {
        return Ok(mat);
    };
    finalize_typed_relation_materialized_node(
        st,
        es,
        session_id,
        &relation.relation.target,
        mat,
        trace,
        read_cap,
        plan_shared,
    )
    .await
}

/// Archive snapshot + build a relation `MaterializedNode` (no GET-hydrate pass).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn archive_materialize_relation_fanout(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    scoped_es: &ExecuteSession,
    relation: &ValidatedRelationTraversalNode,
    node: &ValidatedPlanNode,
    fold: super::plan_fanout_parallel::PlanLineExecutionFold,
    display: String,
    snapshot_label: String,
    trace: Option<&PlasmTraceContext>,
) -> Result<MaterializedNode, String> {
    archive_materialize_relation(
        st,
        es,
        session_id,
        scoped_es,
        relation,
        node,
        execution_result_from_fanout_fold(fold),
        display,
        snapshot_label,
        trace,
        None,
    )
    .await
}

/// Archive snapshot + hydrate from a fully assembled relation execution result.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn archive_materialize_relation_result_hydrated(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    scoped_es: &ExecuteSession,
    relation: &ValidatedRelationTraversalNode,
    node: &ValidatedPlanNode,
    full_result: ExecutionResult,
    display: String,
    snapshot_label: String,
    read_cap: Option<usize>,
    trace: Option<&PlasmTraceContext>,
    plan_shared: Option<Arc<crate::plan_execute_shared::PlanLineExecuteShared>>,
) -> Result<MaterializedNode, String> {
    archive_materialize_relation(
        st,
        es,
        session_id,
        scoped_es,
        relation,
        node,
        full_result,
        display,
        snapshot_label,
        trace,
        Some((read_cap, plan_shared)),
    )
    .await
}

/// Archive snapshot + build a for_each `MaterializedNode`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn archive_materialize_for_each_fanout(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    scoped_es: &ExecuteSession,
    for_each: &ValidatedForEachNode,
    fold: super::plan_fanout_parallel::PlanLineExecutionFold,
    source_row_count: usize,
    snapshot_expressions: Vec<String>,
    trace: Option<&PlasmTraceContext>,
) -> Result<MaterializedNode, String> {
    let result = execution_result_from_fanout_fold(fold.clone());
    let for_each_node = ValidatedPlanNode::ForEach(for_each.clone());
    let parsed_preimage = evidence_plan::parsed_expr_for_plan_node(&for_each_node);
    let display = if fold.displays.len() == 1 {
        fold.displays
            .into_iter()
            .next()
            .unwrap_or_else(|| format!("for_each {}", for_each.id.as_str()))
    } else {
        format!(
            "for_each {} ({} calls)",
            for_each.id.as_str(),
            source_row_count
        )
    };
    let artifact = archive_plasm_result_snapshot(
        st,
        es,
        session_id,
        Some(for_each.effect_template.qualified_entity.entry_id.as_str()),
        snapshot_expressions,
        &parsed_preimage,
        &result,
        trace,
    )
    .await?;
    let rows: Vec<_> = result
        .entities
        .iter()
        .map(|e| cached_entity_row_json(e, scoped_es.cgs.as_ref()))
        .collect();
    Ok(MaterializedNode {
        entry_id: for_each.effect_template.qualified_entity.entry_id.clone(),
        entity: for_each.effect_template.qualified_entity.entity.clone(),
        row_source: inline_row_source_owned(rows),
        row_identities: row_identities_from_entities(
            scoped_es,
            for_each.effect_template.qualified_entity.entity.as_str(),
            &result.entities,
        ),
        result: Arc::new(result),
        artifact: Some(artifact),
        display,
        projection: Some(for_each.projection.clone()).filter(|p| !p.is_empty()),
    })
}
