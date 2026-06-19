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
    source_rows: &[serde_json::Value],
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    trace: Option<&PlasmTraceContext>,
    sink: Option<&McpPlasmTraceSink>,
    plan_shared: Option<Arc<crate::plan_execute_shared::PlanLineExecuteShared>>,
) -> Result<MaterializedNode, String> {
    let RelationMaterialization::PreferFromParentGet { path, .. } = &relation.relation.materialize
    else {
        return Err(format!(
            "relation `{}` expected PreferFromParentGet materialize",
            relation.relation.relation
        ));
    };
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target))?;
    let read_cap = crate::plan_read_bounds::effective_relation_read_cap(relation);
    let rel_name = relation.relation.relation.as_str();
    let target_entity = relation.relation.target.entity.as_str();
    let source_cgs = crate::catalog_ownership::resolve_cgs_for_entity(
        es,
        source_mat.entity.as_str(),
        None,
    )?;
    let rel_schema = source_cgs
        .get_entity(source_mat.entity.as_str())
        .ok_or_else(|| format!("unknown source entity `{}`", source_mat.entity))?
        .relations
        .get(rel_name)
        .ok_or_else(|| {
            format!(
                "entity `{}` has no relation `{rel_name}`",
                source_mat.entity
            )
        })?;
    // Plan-only fast path: wire JSON already contains path payloads (no graph resolution).
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
    let rehydrator = crate::graph_rehydrate::GraphSurfaceRehydrator::new(
        es,
        st,
        session_id,
        scoped_es.cgs.as_ref(),
    );
    let parents = source_mat
        .resolve_materialized_source_parents(&rehydrator)
        .await;
    let snapshot = crate::graph_rehydrate::plan_prefer_from_parent_get(
        &scoped_es,
        &relation.relation.materialize,
        rel_name,
        target_entity,
        &parents,
        source_rows,
    )
    .await?;
    if let Some(mut entities) = snapshot.all_embedded {
        crate::plan_read_bounds::truncate_to_read_cap(&mut entities, read_cap);
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
            read_cap,
            plan_shared,
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
    let resolutions = snapshot.resolutions;
    let mut per_row = snapshot.embedded_per_row;
    let mut request_fingerprints = Vec::new();
    let mut stats = ExecutionStats {
        duration_ms: 0,
        network_requests: 0,
        cache_hits: 0,
        cache_misses: 0,
        ..Default::default()
    };
    let mut source = ExecutionSource::Cache;
    let mut scoped_jobs = Vec::new();
    for (row_index, resolution) in resolutions.iter().enumerate() {
        // EmbeddedRefs rows were captured in the snapshot; only ScopedQuery rows fan out to HTTP.
        let RelationRowResolution::ScopedQuery = resolution else {
            continue;
        };
        // ScopedQuery rows may still use wire JSON without HTTP when parent payload embeds targets.
        let source_row = &source_rows[row_index];
        let wire_rows = normalize_parent_get_target_rows(
            flatten_from_parent_get_source_rows(
                std::slice::from_ref(source_row),
                path,
                rel_schema.cardinality,
            ),
            path,
            Some(scoped_es.cgs.as_ref()),
            target_entity,
        );
        if !wire_rows.is_empty() {
            let wire_entities = json_rows_to_entities_with_refs(
                target_entity,
                &wire_rows,
                Some(scoped_es.cgs.as_ref()),
            );
            per_row[row_index].extend(wire_entities);
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
        let wire_coercion =
            wire_coercion_ctx_for_source_entity(scoped_es.cgs.as_ref(), source_mat.entity.as_str());
        let parsed =
            instantiate_parsed_expr_plan_inputs_with_rows(pe.clone(), &input_rows, wire_coercion)?;
        let expr_label = format!("{base_display} [row {row_index}]");
        super::plan_fanout_parallel::push_verified_row_job(
            &mut scoped_jobs,
            &scoped_es,
            node_index,
            row_index,
            expr_label,
            parsed,
        )?;
    }
    if !scoped_jobs.is_empty() {
        let policy = super::plan_fanout_parallel::RowFanoutPolicy::relation_scoped(read_cap);
        let results = super::plan_fanout_parallel::run_plan_line_jobs_parallel(
            st,
            &scoped_es,
            session_id,
            scoped_jobs,
            trace,
            sink,
            plan_shared.clone(),
            policy.preflight,
            policy.concurrency,
        )
        .await?;
        super::plan_fanout_parallel::merge_fanout_job_results(
            &mut source,
            &mut stats,
            &mut request_fingerprints,
            &mut per_row,
            &results,
            policy.stats,
        );
    }
    let mut entities = super::plan_fanout_parallel::flatten_per_row_entities(per_row);
    crate::plan_read_bounds::truncate_to_read_cap(&mut entities, read_cap);
    let full_result =
        execution_result_from_relation_entities(entities, source, stats, request_fingerprints);
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
        format!(
            "plan.relation({}) prefer_from_parent_get (mixed)",
            relation.id.as_str()
        ),
        read_cap,
        trace,
        plan_shared.clone(),
    )
    .await
}
