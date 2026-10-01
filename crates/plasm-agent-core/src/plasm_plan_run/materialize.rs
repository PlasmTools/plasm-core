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
    rows: Vec<plasm_core::ValueRow>,
    value_shapes: Vec<MaterializedValueShape>,
    row_identities: Vec<Option<plasm_core::RowIdentity>>,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    trace: Option<&PlasmTraceContext>,
) -> Result<MaterializedNode, ExecutionFailure> {
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
    let full_entities = rows_to_entities(&entity, &rows)?;
    let request_fingerprints = vec![compute_fingerprint(node, &rows, &value_shapes)?];
    let dependencies = node
        .depends_on()
        .iter()
        .map(|id| {
            materialized
                .get(id)
                .map(|input| &input.result.collection)
                .ok_or_else(|| format!("declared dependency {} is not materialized", id.as_str()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let identity = plasm_core::collection_codec::CollectionIdentity::for_expression(
        es.cgs.as_ref(),
        &(entry_id, &request_fingerprints),
        0,
    )?;
    use plasm_core::collection_codec::{SharedRows, Transform};
    use plasm_runtime::execution::{ExecutionCollection, PayloadResidency};
    let full_entities: SharedRows<_> = full_entities.into();
    let collection = if let ValidatedPlanNode::Compute(compute) = node {
        let source = &materialized
            .get(&compute.source_node)
            .ok_or_else(|| {
                format!(
                    "compute source {} is not materialized",
                    compute.source_node.as_str()
                )
            })?
            .result
            .collection;
        match &compute.compute.op {
            ComputeOp::Limit { count } => {
                let taken = ExecutionCollection::derive(
                    identity.derived(&("take", count))?,
                    &[source],
                    Transform::Take(*count),
                    PayloadResidency::Graph,
                )?;
                ExecutionCollection::derive(
                    identity,
                    &[&taken],
                    Transform::Map {
                        rows: full_entities
                            .iter()
                            .map(|r| r.reference.clone())
                            .collect::<Vec<_>>()
                            .into(),
                    },
                    PayloadResidency::Materialized(full_entities),
                )?
            }
            ComputeOp::Project { .. }
            | ComputeOp::With { .. }
            | ComputeOp::Sort { .. }
            | ComputeOp::Python { per_row: true, .. } => {
                let mut inputs = vec![source];
                inputs.extend(
                    node.depends_on()
                        .iter()
                        .filter(|id| *id != &compute.source_node)
                        .map(|id| &materialized[id].result.collection),
                );
                ExecutionCollection::derive(
                    identity,
                    &inputs,
                    Transform::Map {
                        rows: full_entities
                            .iter()
                            .map(|r| r.reference.clone())
                            .collect::<Vec<_>>()
                            .into(),
                    },
                    PayloadResidency::Materialized(full_entities),
                )?
            }
            _ => ExecutionCollection::evaluate(identity, &dependencies, full_entities)?,
        }
    } else {
        ExecutionCollection::evaluate(identity, &dependencies, full_entities)?
    };
    let full_result = ExecutionResult {
        collection,
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
        operations: plasm_runtime::OperationLedger::empty(),
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
            .unwrap_or(full_result.count().max(1)),
        _ => full_result.count().max(1),
    };
    let (collection, has_more, paging_handle) = if full_result.count() > page_size {
        let first = full_result.collection.delivery(0..page_size)?;
        let handle = es.register_synthetic_paging_continuation(
            crate::execute_session::SyntheticPageCursor {
                node_id: node.id().as_str().to_string(),
                qualified_entity: crate::plasm_plan::QualifiedEntityKey {
                    entry_id: entry_id.to_string(),
                    entity: entity.clone(),
                },
                collection: full_result.collection.clone(),
                offset: page_size,
                page_size,
                request_fingerprints: request_fingerprints.clone(),
            },
            trace.and_then(|t| t.logical_session_ref.as_deref()),
        );
        (first, true, Some(handle))
    } else {
        (full_result.collection.clone(), false, None)
    };
    Ok(MaterializedNode {
        value_shapes,
        optional_fields: match node {
            ValidatedPlanNode::Compute(c) => c.compute.schema.optional_fields.clone(),
            _ => Default::default(),
        },
        qualified_entity: crate::plasm_plan::QualifiedEntityKey {
            entry_id: entry_id.to_string(),
            entity: entity.clone(),
        },
        display: synthetic_node_display(node),
        projection: synthetic_projection(node),
        row_source: inline_row_source_owned(rows),
        row_identities,
        result: Arc::new(ExecutionResult {
            collection,
            has_more,
            pagination_resume: None,
            paging_handle,
            source: ExecutionSource::Cache,
            stats: full_result.stats,
            request_fingerprints,
            operations: full_result.operations,
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
) -> Result<MaterializedNode, ExecutionFailure> {
    let source_mat = materialized.get(&relation.relation.source).ok_or_else(|| {
        format!(
            "relation source node {:?} has not been materialized",
            relation.relation.source.as_str()
        )
    })?;
    let read_cap = crate::plan_read_bounds::effective_relation_read_cap(relation);
    let source_cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
        es,
        source_mat.qualified_entity.entry_id.as_str(),
        source_mat.qualified_entity.entity.as_str(),
    )
    .map_err(|e| format!("relation source catalog: {e}"))?;
    let source_rows =
        crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, source_cgs)
            .resolve_row_source_rows(&source_mat.row_source, read_cap)
            .await?;
    if source_rows.is_empty() {
        if matches!(
            relation.relation.source_cardinality,
            RelationSourceCardinality::RuntimeCheckedSingleton
        ) {
            return Err(singleton_input_row_count_error(
                relation.relation.source.as_str(),
                "source",
                0,
                "relation traversal",
            )
            .into());
        }
        return super::compute_eval::finalize_empty_relation_materialized_node(
            st,
            es,
            session_id,
            node,
            relation,
            trace,
            read_cap,
            source_mat
                .result
                .collection
                .flat_map(&"empty_relation", &[])?,
        )
        .await;
    }
    match &relation.relation.materialize {
        RelationMaterialization::FromParentGet { .. } => Box::pin(try_materialize_from_parent_get_relation(
            st,
            es,
            session_id,
            node,
            relation,
            source_mat,
            &source_rows,
            trace,
        ))
        .await?
        .ok_or_else(|| {
            format!(
                "relation `{}` could not resolve parent rows on `{}` — ensure the source binding returned at least one row before navigating `.{}`",
                relation.relation.relation,
                source_mat.qualified_entity.entity,
                relation.relation.relation
            ).into()
        }),
        RelationMaterialization::PreferFromParentGet { .. } => {
            Box::pin(materialize_prefer_from_parent_get_relation(
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
            ))
            .await
        }
        RelationMaterialization::QueryScoped { .. }
        | RelationMaterialization::QueryScopedBindings { .. } => {
            if matches!(
                relation.relation.source_cardinality,
                RelationSourceCardinality::Many
            ) {
                Box::pin(materialize_relation_scoped_fanout(
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
                ))
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
                    ).into());
                }
                Box::pin(materialize_relation_singleton_chain(
                    st,
                    es,
                    session_id,
                    idx,
                    relation,
                    materialized,
                    trace,
                    sink,
                    plan_shared,
                ))
                .await
            }
        }
        RelationMaterialization::GetScopedBindings { .. } => {
            // One-from-many flat-map: a plural source fans the scoped get out per parent row
            // (one target per parent), exactly like QueryScopedBindings.
            if matches!(
                relation.relation.source_cardinality,
                RelationSourceCardinality::Many
            ) {
                return Box::pin(materialize_relation_scoped_fanout(
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
                ))
                .await;
            }
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
                ).into());
            }
            Box::pin(materialize_relation_singleton_chain(
                st,
                es,
                session_id,
                idx,
                relation,
                materialized,
                trace,
                sink,
                plan_shared,
            ))
            .await
        }
        RelationMaterialization::ViewEmbed { .. } => {
            materialize_cached_embed_or_error(
                st,
                es,
                session_id,
                node,
                relation,
                source_mat,
                trace,
                read_cap,
                plan_shared.clone(),
                || {
                    format!(
                        "relation `{}` on `{}` requires view-produced parent rows (view_embed); execute the view root before navigating `.{}`",
                        relation.relation.relation,
                        source_mat.qualified_entity.entity,
                        relation.relation.relation
                    )
                },
            )
            .await
        }
        RelationMaterialization::Unavailable => {
            if matches!(
                relation.relation.source_cardinality,
                RelationSourceCardinality::Many
            ) {
                // Plural source → per-row fanout (covers one-from-many and many-from-many).
                Box::pin(materialize_relation_scoped_fanout(
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
                ))
                .await
            } else if matches!(
                relation.relation.cardinality,
                crate::plasm_plan::RelationCardinality::One
            ) {
                Box::pin(materialize_relation_singleton_chain(
                    st,
                    es,
                    session_id,
                    idx,
                    relation,
                    materialized,
                    trace,
                    sink,
                    plan_shared,
                ))
                .await
            } else {
                Err(format!(
                    "relation `{}` on `{}` has no materialize strategy (Unavailable)",
                    relation.relation.relation, source_mat.qualified_entity.entity
                ).into())
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
    source_rows: &[plasm_core::ValueRow],
    trace: Option<&PlasmTraceContext>,
) -> Result<Option<MaterializedNode>, ExecutionFailure> {
    let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
        es,
        source_mat.qualified_entity.entry_id.as_str(),
        source_mat.qualified_entity.entity.as_str(),
    )
    .map_err(|e| {
        format!(
            "relation `{}` FromParentGet source catalog: {e}",
            relation.relation.relation
        )
    })?;
    let ent = cgs
        .get_entity(source_mat.qualified_entity.entity.as_str())
        .ok_or_else(|| {
            format!(
                "unknown source entity `{}`",
                source_mat.qualified_entity.entity
            )
        })?;
    if !ent
        .relations
        .contains_key(relation.relation.relation.as_str())
    {
        return Err(format!(
            "entity `{}` has no relation `{}`",
            source_mat.qualified_entity.entity, relation.relation.relation
        )
        .into());
    }
    let path = match &relation.relation.materialize {
        RelationMaterialization::FromParentGet { path, .. }
        | RelationMaterialization::PreferFromParentGet { path, .. } => path,
        _ => return Ok(None),
    };
    if path.is_empty() {
        return Err(format!(
            "relation `{}` on `{}` declares from_parent_get with an empty path",
            relation.relation.relation, source_mat.qualified_entity.entity
        )
        .into());
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
    let rehydrator = crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, cgs);
    let parents = source_mat
        .resolve_materialized_source_parents(&rehydrator)
        .await?;
    let wire_fallback = super::compute_eval::parent_get_wire_rows(
        source_rows,
        relation,
        source_mat.qualified_entity.entity.as_str(),
        cgs,
        target,
    )
    .ok()
    .filter(|rows| !rows.is_empty());
    let snapshot = super::compute_eval::snapshot_embed_relation_under_graph_lock(
        &scoped_es,
        relation,
        rel_name,
        target,
        &parents,
        wire_fallback.as_deref(),
    )
    .await?;
    let coverage = super::compute_eval::embedded_collection(
        &source_mat.result,
        &parents,
        rel_name,
        target,
        snapshot.entities.iter().map(|row| &row.reference),
        snapshot.read_cap,
    )?;
    let display = crate::plan_dry_display::render_executable_expr(
        &relation.relation.ir.expr,
        relation.relation.ir.projection.as_deref(),
        Some(es),
    );
    super::compute_eval::finalize_embed_relation_materialized_node(
        st,
        es,
        session_id,
        node,
        relation,
        &scoped_es,
        target,
        snapshot.entities,
        snapshot.wire_rows,
        None,
        display,
        vec![synthetic_node_display(node)],
        trace,
        snapshot.read_cap,
        0,
        coverage,
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
) -> Result<Vec<plasm_core::ValueRow>, ExecutionFailure> {
    let mat = materialized.get(source).ok_or_else(|| {
        format!(
            "source node {:?} has not been materialized",
            source.as_str()
        )
    })?;
    crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, es.cgs.as_ref())
        .resolve_row_source_rows(&mat.row_source, None)
        .await
        .map_err(ExecutionFailure::from)
}

#[must_use]
pub(crate) fn execution_result_from_fanout_fold(
    fold: super::plan_fanout_parallel::PlanLineExecutionFold,
    identity: plasm_core::collection_codec::CollectionIdentity,
    parent: Option<&plasm_runtime::execution::ExecutionCollection>,
) -> Result<ExecutionResult, ExecutionFailure> {
    Ok(ExecutionResult {
        collection: fold.collection(identity, parent)?,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: fold.source,
        stats: fold.stats,
        request_fingerprints: fold.request_fingerprints,
        operations: fold.operations,
    })
}

#[must_use]
pub(crate) fn execution_result_from_relation_entities(
    collection: plasm_runtime::execution::ExecutionCollection,
    source: ExecutionSource,
    stats: ExecutionStats,
    request_fingerprints: Vec<String>,
    operations: plasm_runtime::OperationLedger,
) -> ExecutionResult {
    ExecutionResult {
        collection,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source,
        stats,
        request_fingerprints,
        operations,
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
        .entities()
        .iter()
        .map(|e| cached_entity_row_values(e, scoped_es.cgs.as_ref()))
        .collect();
    MaterializedNode {
        value_shapes: Vec::new(),
        optional_fields: Default::default(),
        qualified_entity: relation.relation.target.clone(),
        display,
        projection: relation.relation.ir.projection.clone(),
        row_source: inline_row_source_owned(rows),
        row_identities: row_identities_from_entities(
            scoped_es,
            relation.relation.target.entity.as_str(),
            full_result.entities(),
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
) -> Result<MaterializedNode, ExecutionFailure> {
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
    parent: &plasm_runtime::execution::ExecutionCollection,
    fold: super::plan_fanout_parallel::PlanLineExecutionFold,
    display: String,
    snapshot_label: String,
    trace: Option<&PlasmTraceContext>,
) -> Result<MaterializedNode, ExecutionFailure> {
    archive_materialize_relation(
        st,
        es,
        session_id,
        scoped_es,
        relation,
        node,
        execution_result_from_fanout_fold(
            fold,
            plasm_core::collection_codec::CollectionIdentity::for_expression(
                scoped_es.cgs.as_ref(),
                &relation.id,
                0,
            )?,
            Some(parent),
        )?,
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
) -> Result<MaterializedNode, ExecutionFailure> {
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

#[allow(clippy::too_many_arguments)]
async fn materialize_cached_embed_or_error(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node: &ValidatedPlanNode,
    relation: &ValidatedRelationTraversalNode,
    source_mat: &MaterializedNode,
    trace: Option<&PlasmTraceContext>,
    read_cap: Option<usize>,
    plan_shared: Option<Arc<crate::plan_execute_shared::PlanLineExecuteShared>>,
    on_miss: impl FnOnce() -> String,
) -> Result<MaterializedNode, ExecutionFailure> {
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
            plan_shared,
        )
        .await
    } else {
        Err(on_miss().into())
    }
}

/// Archive snapshot + build a for_each `MaterializedNode`.
#[allow(clippy::too_many_arguments)]
fn for_each_execution_result(
    fold: super::plan_fanout_parallel::PlanLineExecutionFold,
    parent: &plasm_runtime::execution::ExecutionCollection,
) -> Result<plasm_runtime::ExecutionResult, ExecutionFailure> {
    execution_result_from_fanout_fold(
        fold,
        parent.membership().identity().derived(&"for_each")?,
        Some(parent),
    )
}

// Archive context and fanout evidence are passed together, as in adjacent helpers.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn archive_materialize_for_each_fanout(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    scoped_es: &ExecuteSession,
    for_each: &ValidatedForEachNode,
    fold: super::plan_fanout_parallel::PlanLineExecutionFold,
    source_row_count: usize,
    parent: &plasm_runtime::execution::ExecutionCollection,
    snapshot_expressions: Vec<String>,
    trace: Option<&PlasmTraceContext>,
) -> Result<MaterializedNode, ExecutionFailure> {
    let mut result = for_each_execution_result(fold.clone(), parent)?;
    stamp_for_each_operations(&mut result, scoped_es.cgs.as_ref(), for_each);
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
        .entities()
        .iter()
        .map(|e| cached_entity_row_values(e, scoped_es.cgs.as_ref()))
        .collect();
    Ok(MaterializedNode {
        value_shapes: Vec::new(),
        optional_fields: Default::default(),
        qualified_entity: for_each.effect_template.qualified_entity.clone(),
        row_source: inline_row_source_owned(rows),
        row_identities: row_identities_from_entities(
            scoped_es,
            for_each.effect_template.qualified_entity.entity.as_str(),
            result.entities(),
        ),
        result: Arc::new(result),
        artifact: Some(artifact),
        display,
        projection: Some(for_each.projection.clone()).filter(|p| !p.is_empty()),
    })
}

/// Archive snapshot + build an iterate/until `MaterializedNode`.
///
/// HTTP-2: rematerialized seed rows live on `result.entities` with `count == entities.len()`.
/// Step acks are the merged fanout ledger on `result.operations`. Zero-step keeps that
/// ledger empty. This constructor does not mint acks and does not use `inline_cache`.
///
/// Coverage is the folded seed (+ retained step) coverages — never a hard Complete literal.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn archive_materialize_iterate_until(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    scoped_es: &ExecuteSession,
    it: &crate::plasm_plan::ValidatedIterateUntilNode,
    seed_qe: crate::plasm_plan::QualifiedEntityKey,
    rows: Vec<plasm_core::ValueRow>,
    observed: &MaterializedNode,
    operations: plasm_runtime::OperationLedger,
    request_fingerprints: Vec<String>,
    stats: ExecutionStats,
    source: ExecutionSource,
    steps_taken: u32,
    dependencies: &[plasm_runtime::execution::ExecutionCollection],
    trace: Option<&PlasmTraceContext>,
) -> Result<MaterializedNode, ExecutionFailure> {
    if steps_taken > 0 && operations.is_empty() {
        return Err(
            "iterate_until steps completed but fold operations ledger is empty (HTTP-2)".into(),
        );
    }
    // Inline observations already own semantic entities. Only graph-backed
    // observations need rehydration; public value rows cannot restore evidence.
    let entities = if observed.result.entities().is_empty() {
        let rehydrator = crate::graph_rehydrate::GraphSurfaceRehydrator::new(
            scoped_es,
            st,
            session_id,
            &scoped_es.cgs,
        );
        observed
            .resolve_materialized_source_parents(&rehydrator)
            .await?
            .into()
    } else {
        observed.result.entities().clone()
    };
    if entities.len() != rows.len() {
        return Err("iterate_until final observation lost semantic row occurrences".into());
    }
    let mut inputs: Vec<_> = dependencies.iter().collect();
    inputs.push(&observed.result.collection);
    let collection = plasm_runtime::execution::ExecutionCollection::evaluate(
        observed
            .result
            .collection
            .membership()
            .identity()
            .derived(&("iterate_until", &it.id, steps_taken))?,
        &inputs,
        entities,
    )?;
    let result = ExecutionResult {
        collection,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source,
        stats,
        request_fingerprints,
        operations,
    };
    let iterate_node = ValidatedPlanNode::IterateUntil(it.clone());
    let parsed_preimage = evidence_plan::parsed_expr_for_plan_node(&iterate_node);
    let display = format!("iterate_until {} take {}", it.id.as_str(), it.take);
    let artifact = archive_plasm_result_snapshot(
        st,
        es,
        session_id,
        Some(seed_qe.entry_id.as_str()),
        vec![display.clone()],
        &parsed_preimage,
        &result,
        trace,
    )
    .await?;
    let row_identities =
        row_identities_from_entities(scoped_es, seed_qe.entity.as_str(), result.entities());
    Ok(MaterializedNode {
        value_shapes: Vec::new(),
        optional_fields: Default::default(),
        qualified_entity: seed_qe,
        row_source: inline_row_source_owned(rows),
        row_identities,
        result: Arc::new(result),
        artifact: Some(artifact),
        display,
        projection: Some(it.effect_template.projection.clone()).filter(|p| !p.is_empty()),
    })
}

fn stamp_for_each_operations(
    result: &mut ExecutionResult,
    cgs: &CGS,
    for_each: &ValidatedForEachNode,
) {
    if !result.operations.is_empty() {
        return;
    }
    let template = &for_each.effect_template;
    let is_operation = matches!(
        template.result_shape,
        crate::plasm_plan::ResultShape::SideEffectAck
    ) || matches!(
        template.effect_class,
        EffectClass::SideEffect | EffectClass::Write
    );
    if !is_operation {
        return;
    }
    let Some(ack) = plasm_runtime::OperationAck::try_from_mutating_expr(
        &template.ir_template.expr,
        Some(cgs),
        result.source,
        0,
        0,
    ) else {
        return;
    };
    result.operations =
        plasm_runtime::OperationLedger::from_ack(plasm_runtime::OperationAck::empty_iteration(
            ack.entry_id,
            ack.entity,
            ack.capability,
            ack.description,
            result.source,
        ));
}

#[cfg(test)]
mod for_each_coverage_tests {
    use super::*;
    use plasm_runtime::ResultCoverage::{Complete, Partial, Unknown};
    #[test]
    fn successful_children_do_not_complete_partial_source() {
        for coverage in [Complete, Partial, Unknown] {
            for count in [0, 3] {
                let rows = (0..count)
                    .map(|i| {
                        plasm_runtime::CachedEntity::new(
                            plasm_core::Ref::new("Parent", i.to_string()),
                            0,
                        )
                    })
                    .collect();
                let parent = crate::test_support::execution_fixtures::collection(rows, coverage);
                let mut fold = super::super::plan_fanout_parallel::empty_execution_fold();
                fold.collections = (0..count)
                    .map(|_| crate::test_support::execution_fixtures::collection(vec![], Complete))
                    .collect();
                fold.stats.network_requests = count;
                let result = for_each_execution_result(fold, &parent).unwrap();
                assert_eq!(
                    result.coverage(),
                    if coverage == Complete {
                        Complete
                    } else {
                        Unknown
                    }
                );
                assert_eq!(result.stats.network_requests, count);
            }
        }
    }
}
