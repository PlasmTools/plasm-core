//! PreferFromParentGet relation materialization (mixed embed + scoped GET fan-out).

use super::*;

/// PreferFromParentGet: embed from graph when possible; otherwise per-row scoped GET via
/// graph branch fork/commit — session mutex not held during HTTP.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn materialize_prefer_from_parent_get_relation(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node_index: usize,
    node: &ValidatedPlanNode,
    relation: &ValidatedRelationTraversalNode,
    source_mat: &MaterializedNode,
    source_rows: &[plasm_core::ValueRow],
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    trace: Option<&PlasmTraceContext>,
    sink: Option<&McpPlasmTraceSink>,
    plan_shared: Option<Arc<crate::plan_execute_shared::PlanLineExecuteShared>>,
) -> Result<MaterializedNode, ExecutionFailure> {
    use super::plan_fanout_parallel::{self as fanout, RowFanoutPolicy};
    use plasm_core::collection_codec::{Demand, SharedRows, Transform};
    use plasm_runtime::execution::{ExecutionCollection, PayloadResidency};
    let RelationMaterialization::PreferFromParentGet { fallback, .. } =
        &relation.relation.materialize
    else {
        return Err(ExecutionFailure::new(
            plasm_runtime::FailureCause::Program,
            "relation_materialization_policy_mismatch",
            format!(
                "relation `{}` is not configured for PreferFromParentGet",
                relation.id
            ),
        ));
    };
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target)).map_err(
        |diagnostic| {
            ExecutionFailure::new(
                plasm_runtime::FailureCause::Program,
                "relation_catalog_scope_unavailable",
                diagnostic.to_string(),
            )
        },
    )?;
    let read_cap = crate::plan_read_bounds::effective_relation_read_cap(relation);
    let rel_name = relation.relation.relation.as_str();
    let target_entity = relation.relation.target.entity.as_str();
    let rehydrator = crate::graph_rehydrate::GraphSurfaceRehydrator::new(
        es,
        st,
        session_id,
        scoped_es.cgs.as_ref(),
    );
    let parents = source_mat
        .resolve_materialized_source_parents(&rehydrator)
        .await
        .map_err(|diagnostic| {
            ExecutionFailure::new(
                plasm_runtime::FailureCause::Runtime,
                "relation_parent_rehydration_failed",
                diagnostic.to_string(),
            )
        })?;
    if parents.len() != source_rows.len() || parents.len() != source_mat.result.count() {
        return Err(plasm_core::collection_codec::CollectionFault::Conservation.into());
    }
    let mut children: Vec<Option<ExecutionCollection>> = vec![None; parents.len()];
    let mut embedded = vec![false; parents.len()];
    let mut resident: Vec<SharedRows<plasm_runtime::CachedEntity>> =
        vec![SharedRows::default(); parents.len()];
    let mut jobs = Vec::new();
    let base_display = crate::plan_dry_display::render_executable_expr(
        &relation.relation.ir.expr,
        relation.relation.ir.projection.as_deref(),
        Some(es),
    );
    let snapshot =
        crate::graph_rehydrate::RelationEmbedSnapshot::capture(&scoped_es, &parents, rel_name)
            .await;
    for (index, (parent, cached)) in parents.iter().zip(snapshot.resident).enumerate() {
        if let Some(membership) = parent.relations.get(rel_name) {
            let all_present = cached.len() == membership.len();
            if all_present
                || matches!(
                    fallback,
                    plasm_core::RelationScopedFallback::HydrateFromEmbedPath { .. }
                )
            {
                embedded[index] = true;
                resident[index] = cached;
                children[index] = Some(ExecutionCollection::graph(membership.record().clone()));
                if let plasm_core::RelationScopedFallback::HydrateFromEmbedPath {
                    get_capability,
                    ..
                } = fallback
                {
                    let missing = membership
                        .iter()
                        .filter(|reference| {
                            !resident[index]
                                .iter()
                                .any(|row| &row.reference == *reference)
                        })
                        .cloned();
                    super::prefer_embed_hydrate::push_prefer_hydrate_get_jobs(
                        &mut jobs,
                        &scoped_es,
                        node_index,
                        index,
                        &base_display,
                        &relation.relation.target,
                        target_entity,
                        get_capability,
                        missing,
                    )?;
                }
                continue;
            }
        }
        if matches!(
            fallback,
            plasm_core::RelationScopedFallback::HydrateFromEmbedPath { .. }
        ) {
            return Err(ExecutionFailure::new(
                plasm_runtime::FailureCause::Runtime,
                "relation_membership_observation_missing",
                format!("relation `{rel_name}` lacks a decoded membership observation for parent {index}"),
            ));
        }
        let row_identity = source_mat
            .row_identities
            .get(index)
            .and_then(|i| i.as_ref())
            .cloned();
        let mut input_rows = materialized_result_use_inputs_with_source_row(
            materialized,
            &relation.uses_result,
            &relation.relation.source,
            &source_rows[index],
            row_identity,
        )?;
        let coercions =
            wire_coercion_by_alias_from_inputs(es, &mut input_rows).map_err(|diagnostic| {
                ExecutionFailure::new(
                    plasm_runtime::FailureCause::Program,
                    "relation_input_coercion_failed",
                    diagnostic.to_string(),
                )
            })?;
        let parsed = instantiate_parsed_expr_plan_inputs_with_rows(
            ParsedExpr {
                expr: relation.relation.ir.expr.clone(),
                projection: relation.relation.ir.projection.clone(),
                field_dot_extract: None,
            },
            &scoped_es.cgs,
            &input_rows,
            &coercions,
        )
        .map_err(ExecutionFailure::from)?;
        fanout::push_verified_row_job(
            &mut jobs,
            &scoped_es,
            node_index,
            index,
            format!("{base_display} [row {index}]"),
            parsed,
        )?;
    }
    let policy = RowFanoutPolicy::relation_scoped(read_cap);
    let batch = fanout::run_plan_line_jobs_parallel(
        st,
        &scoped_es,
        session_id,
        jobs,
        trace,
        sink,
        plan_shared.clone(),
        policy.preflight,
        policy.concurrency,
        policy.admission,
    )
    .await?;
    let mut source = ExecutionSource::Cache;
    let mut stats = ExecutionStats::default();
    let mut fingerprints = Vec::new();
    let mut operations = plasm_runtime::OperationLedger::empty();
    for job in &batch.completed {
        source = fanout::combine_execution_source(source, job.result.source);
        fanout::merge_execution_stats(&mut stats, &job.result.stats, policy.stats);
        operations.merge(&job.result.operations);
        fingerprints.extend(job.result.request_fingerprints.iter().cloned());
        let rows = job.result.collection.materialize(Demand::Observed)?;
        if embedded[job.index] {
            resident[job.index] = SharedRows::concat([&resident[job.index], rows]);
        } else if children[job.index]
            .replace(job.result.collection.clone())
            .is_some()
        {
            return Err(plasm_core::collection_codec::CollectionFault::Conservation.into());
        }
    }
    if let Some(failure) = batch.failures.first() {
        return Err(failure.message.clone().with_effects(&operations));
    }
    for index in 0..children.len() {
        if !embedded[index] {
            continue;
        }
        let child = children[index]
            .as_ref()
            .ok_or(plasm_core::collection_codec::CollectionFault::Arity)?;
        let positions = child
            .membership()
            .observed()
            .iter()
            .map(|reference| {
                resident[index]
                    .iter()
                    .position(|row| &row.reference == reference)
                    .ok_or(plasm_core::collection_codec::CollectionFault::NotResident)
            })
            .collect::<Result<Vec<_>, _>>()?;
        children[index] = Some(child.with_materialization(resident[index].select(positions)?)?);
    }
    let children = children
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or(plasm_core::collection_codec::CollectionFault::Arity)?;
    let mut collection = source_mat
        .result
        .collection
        .flat_map(&("prefer_relation", &relation.id), &children)?;
    if let Some(count) = read_cap {
        collection = ExecutionCollection::derive(
            collection
                .membership()
                .identity()
                .derived(&("take", count))?,
            &[&collection],
            Transform::Take(count),
            PayloadResidency::Materialized(
                collection
                    .resident_entities()
                    .select(0..count.min(collection.count()))?,
            ),
        )?;
    }
    let full_result = execution_result_from_relation_entities(
        collection,
        source,
        stats,
        fingerprints,
        operations,
    );
    archive_materialize_relation_result_hydrated(
        st,
        es,
        session_id,
        &scoped_es,
        relation,
        node,
        full_result,
        format!(
            "plan.relation({}) prefer_from_parent_get",
            relation.id.as_str()
        ),
        format!("plan.relation({}) recorded fanout", relation.id.as_str()),
        read_cap,
        trace,
        plan_shared,
    )
    .await
}
