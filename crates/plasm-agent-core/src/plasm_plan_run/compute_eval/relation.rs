//! Relation traversal materialization during plan execute.

#![allow(clippy::too_many_arguments)]

use super::super::*;
use super::compute_ops::compute_fingerprint;
use super::eval::{
    instantiate_parsed_expr_plan_inputs, instantiate_parsed_expr_plan_inputs_with_rows,
    json_to_plasm_value, materialized_result_use_inputs_with_source_row,
    wire_coercion_ctx_for_source_entity,
};

pub(crate) async fn materialize_relation_singleton_chain(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node_index: usize,
    relation: &ValidatedRelationTraversalNode,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    trace: Option<&PlasmTraceContext>,
    sink: Option<&McpPlasmTraceSink>,
    plan_shared: Option<Arc<crate::plan_execute_shared::PlanLineExecuteShared>>,
) -> Result<MaterializedNode, String> {
    let pe = ParsedExpr {
        expr: relation.relation.ir.expr.clone(),
        projection: relation.relation.ir.projection.clone(),
    };
    let parsed = instantiate_parsed_expr_plan_inputs(pe, &relation.uses_result, materialized)?;
    let expr_label = relation
        .relation
        .ir
        .display_expr
        .as_deref()
        .unwrap_or("<ir>");
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target))?;
    let (parsed, result, artifact) = execute_plasm_parsed_expr(
        st,
        &scoped_es,
        session_id,
        expr_label,
        parsed,
        trace,
        node_index as i64,
        None,
        None,
        None,
        plan_shared.as_deref(),
    )
    .await?;
    if let Some(sink) = sink {
        trace_record_plasm_line(sink, node_index, expr_label, &parsed, &result, &scoped_es).await;
    }
    let read_cap = crate::plan_read_bounds::effective_relation_read_cap(relation);
    finalize_typed_relation_materialized_node(
        st,
        es,
        session_id,
        &relation.relation.target,
        MaterializedNode {
            entry_id: relation.relation.target.entry_id.clone(),
            entity: relation.relation.target.entity.clone(),
            display: crate::expr_display::expr_display(&parsed.expr),
            projection: parsed.projection,
            row_source: inline_row_source(&[]),
            row_identities: row_identities_from_entities(
                &scoped_es,
                relation.relation.target.entity.as_str(),
                &result.entities,
            ),
            result: Arc::new(result),
            artifact,
        },
        trace,
        read_cap,
        plan_shared.clone(),
    )
    .await
}

/// Wire JSON rows extracted along a `from_parent_get` path — preserves nested embeds for chained hops.
pub(crate) fn parent_get_wire_rows(
    source_rows: &[serde_json::Value],
    relation: &ValidatedRelationTraversalNode,
    source_entity: &str,
    cgs: &CGS,
    target_entity: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let rel_name = relation.relation.relation.as_str();
    let path = match &relation.relation.materialize {
        RelationMaterialization::FromParentGet { path }
        | RelationMaterialization::PreferFromParentGet { path, .. } => path,
        other => {
            return Err(format!(
                "parent_get_wire_rows expected from_parent_get materialize, got {other:?}"
            ));
        }
    };
    let rel_schema = cgs
        .get_entity(source_entity)
        .ok_or_else(|| format!("unknown source entity `{source_entity}`"))?
        .relations
        .get(rel_name)
        .ok_or_else(|| format!("entity `{source_entity}` has no relation `{rel_name}`"))?;
    Ok(normalize_parent_get_target_rows(
        flatten_from_parent_get_source_rows(source_rows, path, rel_schema.cardinality),
        path,
        Some(cgs),
        target_entity,
    ))
}

/// When view `relation_outputs` (or other embed paths) populated `CachedEntity.relations`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn try_materialize_from_cached_relation_refs(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node: &ValidatedPlanNode,
    relation: &ValidatedRelationTraversalNode,
    source_mat: &MaterializedNode,
    trace: Option<&PlasmTraceContext>,
) -> Result<Option<MaterializedNode>, String> {
    let rel_name = relation.relation.relation.as_str();
    let target_entity = relation.relation.target.entity.as_str();
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target))?;
    let rehydrator = crate::graph_rehydrate::GraphSurfaceRehydrator::new(
        es,
        st,
        session_id,
        scoped_es.cgs.as_ref(),
    );
    let parents = source_mat
        .resolve_materialized_source_parents(&rehydrator)
        .await;
    if parents.is_empty() {
        return Ok(None);
    }
    let source_rows: Vec<serde_json::Value> = rehydrator
        .resolve_row_source_rows(&source_mat.row_source, None)
        .await
        .unwrap_or_default();
    let wire_extracted = parent_get_wire_rows(
        &source_rows,
        relation,
        source_mat.entity.as_str(),
        scoped_es.cgs.as_ref(),
        target_entity,
    )
    .ok()
    .filter(|rows| !rows.is_empty());
    let relations_on_parents = parents.iter().all(|p| p.relations.contains_key(rel_name));
    if !relations_on_parents && wire_extracted.is_none() {
        return Ok(None);
    }
    let guard = scoped_es.lock_graph_cache().await;
    let mat = guard.materialization();
    let mut entities = resolve_embed_target_entities(
        rel_name,
        target_entity,
        &parents,
        mat,
        wire_extracted.as_deref(),
        scoped_es.cgs.as_ref(),
    );
    if entities.is_empty() {
        return Ok(None);
    }
    let read_cap = crate::plan_read_bounds::effective_relation_read_cap(relation);
    crate::plan_read_bounds::truncate_to_read_cap(&mut entities, read_cap);
    let count = entities.len();
    let wire_rows = crate::graph_rehydrate::wire_rows_for_embed_entities(
        &entities,
        scoped_es.cgs.as_ref(),
        mat,
    );
    drop(guard);
    let display = format!("plan.relation({}) cached_embed", relation.id.as_str());
    finalize_embed_relation_materialized_node(
        st,
        es,
        session_id,
        node,
        relation,
        &scoped_es,
        target_entity,
        entities,
        wire_rows,
        Some(&source_rows),
        display.clone(),
        vec![display],
        trace,
        read_cap,
        count,
    )
    .await
    .map(Some)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn materialize_relation_scoped_fanout(
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
    let pe = ParsedExpr {
        expr: relation.relation.ir.expr.clone(),
        projection: relation.relation.ir.projection.clone(),
    };
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target))?;
    let source_node = &relation.relation.source;
    let base_display = relation
        .relation
        .ir
        .display_expr
        .clone()
        .unwrap_or_else(|| format!("plan.relation({})", relation.id.as_str()));

    let read_cap = crate::plan_read_bounds::effective_relation_read_cap(relation);
    let parent_row_cap = read_cap.unwrap_or(source_rows.len());
    let mut jobs = Vec::new();
    for (row_index, source_row) in source_rows.iter().enumerate().take(parent_row_cap) {
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
        super::super::plan_fanout_parallel::push_verified_row_job(
            &mut jobs, &scoped_es, node_index, row_index, expr_label, parsed,
        )?;
    }
    let fold = super::super::plan_fanout_parallel::execute_row_fanout(
        st,
        &scoped_es,
        session_id,
        jobs,
        trace,
        sink,
        plan_shared,
        super::super::plan_fanout_parallel::RowFanoutPolicy::relation_scoped(read_cap),
    )
    .await?;
    super::super::materialize::archive_materialize_relation_fanout(
        st,
        es,
        session_id,
        &scoped_es,
        relation,
        node,
        fold,
        format!(
            "plan.relation({}) fanout ({} source rows)",
            relation.id.as_str(),
            source_rows.len()
        ),
        format!(
            "plan.relation({}) fanout {} rows",
            relation.id.as_str(),
            source_rows.len()
        ),
        trace,
    )
    .await
}
pub(crate) fn json_rows_to_entities(entity: &str, rows: &[serde_json::Value]) -> Vec<CachedEntity> {
    json_rows_to_entities_with_refs(entity, rows, None)
}

/// Hoist nested parent-get embed rows (e.g. `{ "pokemon": { "name": "x" } }`) to target entity shape.
pub(crate) fn normalize_parent_get_target_rows(
    rows: Vec<serde_json::Value>,
    path: &[plasm_core::JsonPathSegment],
    cgs: Option<&CGS>,
    entity: &str,
) -> Vec<serde_json::Value> {
    let embed_key = path.iter().rev().find_map(|seg| match seg {
        plasm_core::JsonPathSegment::Key { key } => Some(key.as_str()),
        _ => None,
    });
    rows.into_iter()
        .map(|row| normalize_parent_get_target_row(row, embed_key, cgs, entity))
        .collect()
}

fn normalize_parent_get_target_row(
    row: serde_json::Value,
    embed_key: Option<&str>,
    cgs: Option<&CGS>,
    entity: &str,
) -> serde_json::Value {
    let mut v = row;
    if let Some(key) = embed_key {
        v = hoist_embed_key_object(v, key);
    }
    if let Some(ent) = cgs.and_then(|c| c.get_entity(entity)) {
        let id_field = ent.id_field.as_str();
        if wire_id_from_row(&v, id_field, ent.id_from.as_deref()).is_none() {
            if let Some(path) = ent.id_from.as_deref() {
                if let Some(extracted) = super::super::row_json::value_at_segments(&v, path) {
                    if let Some(id) = json_value_to_wire_id(extracted) {
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert(id_field.to_string(), serde_json::Value::String(id));
                        }
                    }
                }
            }
        }
    }
    v
}

fn hoist_embed_key_object(row: serde_json::Value, embed_key: &str) -> serde_json::Value {
    if let Some(obj) = row.as_object() {
        if let Some(inner) = obj.get(embed_key) {
            if inner.is_object() {
                return inner.clone();
            }
        }
    }
    row
}

fn json_value_to_wire_id(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    if let Some(n) = v.as_i64() {
        return Some(n.to_string());
    }
    if let Some(n) = v.as_u64() {
        return Some(n.to_string());
    }
    None
}

fn wire_id_from_row(
    row: &serde_json::Value,
    id_field: &str,
    id_from: Option<&[String]>,
) -> Option<String> {
    if let Some(id) = row.get(id_field).and_then(json_value_to_wire_id) {
        return Some(id);
    }
    id_from
        .and_then(|path| super::super::row_json::value_at_segments(row, path))
        .and_then(json_value_to_wire_id)
}

pub(crate) fn json_rows_to_entities_with_refs(
    entity: &str,
    rows: &[serde_json::Value],
    cgs: Option<&CGS>,
) -> Vec<CachedEntity> {
    let id_field = cgs
        .and_then(|c| c.get_entity(entity))
        .map(|e| e.id_field.as_str())
        .unwrap_or("id");
    let id_from = cgs
        .and_then(|c| c.get_entity(entity))
        .and_then(|e| e.id_from.as_deref());
    rows.iter()
        .enumerate()
        .map(|(idx, row)| {
            let mut fields = IndexMap::new();
            match row {
                serde_json::Value::Object(obj) => {
                    for (k, v) in obj {
                        fields.insert(k.clone(), TypedFieldValue::from(json_to_plasm_value(v)));
                    }
                }
                other => {
                    fields.insert(
                        "value".to_string(),
                        TypedFieldValue::from(json_to_plasm_value(other)),
                    );
                }
            }
            let reference = Ref::new(
                EntityName::new(entity.to_string()),
                wire_id_from_row(row, id_field, id_from)
                    .unwrap_or_else(|| format!("synthetic-{}", idx + 1)),
            );
            CachedEntity {
                reference,
                fields,
                relations: IndexMap::new(),
                last_updated: 0,
                version: 1,
                completeness: EntityCompleteness::Complete,
            }
        })
        .collect()
}

/// Resolve embed-target entities from session-graph refs, else synthesize from wire JSON rows.
pub(crate) fn resolve_embed_target_entities(
    rel_name: &str,
    target_entity: &str,
    parents: &[CachedEntity],
    mat: &plasm_runtime::SessionMaterialization,
    wire_fallback_rows: Option<&[serde_json::Value]>,
    cgs: &CGS,
) -> Vec<CachedEntity> {
    match crate::graph_rehydrate::collect_all_embedded_relation_targets(
        rel_name,
        target_entity,
        parents,
        mat,
    ) {
        Some(entities) if !entities.is_empty() => entities,
        _ => wire_fallback_rows
            .map(|rows| json_rows_to_entities_with_refs(target_entity, rows, Some(cgs)))
            .unwrap_or_default(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn finalize_embed_relation_materialized_node(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node: &ValidatedPlanNode,
    relation: &ValidatedRelationTraversalNode,
    scoped_es: &ExecuteSession,
    target_entity: &str,
    entities: Vec<CachedEntity>,
    wire_rows: Vec<serde_json::Value>,
    fingerprint_rows: Option<&[serde_json::Value]>,
    display: String,
    artifact_labels: Vec<String>,
    trace: Option<&PlasmTraceContext>,
    read_cap: Option<usize>,
    cache_hits: usize,
) -> Result<MaterializedNode, String> {
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
            cache_hits,
            cache_misses: 0,
            ..Default::default()
        },
        request_fingerprints: vec![compute_fingerprint(
            node,
            fingerprint_rows.unwrap_or(&wire_rows),
        )],
    };
    let parsed_preimage = crate::plasm_plan_run::evidence_plan::parsed_expr_for_plan_node(node);
    let artifact = archive_plasm_result_snapshot(
        st,
        es,
        session_id,
        Some(relation.relation.target.entry_id.as_str()),
        artifact_labels,
        &parsed_preimage,
        &full_result,
        trace,
    )
    .await?;
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
            row_source: inline_row_source_owned(wire_rows),
            row_identities: row_identities_from_entities(
                scoped_es,
                target_entity,
                &full_result.entities,
            ),
            result: Arc::new(full_result),
            artifact: Some(artifact),
        },
        trace,
        read_cap,
        None,
    )
    .await
}

#[cfg(test)]
mod parent_get_row_tests {
    use super::*;
    use plasm_core::JsonPathSegment;

    #[test]
    fn normalize_hoists_nested_pokemon_embed() {
        let path = vec![JsonPathSegment::Key {
            key: "pokemon".into(),
        }];
        let rows = normalize_parent_get_target_rows(
            vec![serde_json::json!({
                "pokemon": { "name": "jolteon", "url": "https://pokeapi.co/api/v2/pokemon/135/" }
            })],
            &path,
            None,
            "Pokemon",
        );
        assert_eq!(rows[0]["name"], "jolteon");
    }

    #[test]
    fn json_rows_avoids_synthetic_when_id_present() {
        let rows = vec![serde_json::json!({ "name": "pikachu", "id": 25 })];
        let entities = json_rows_to_entities_with_refs("Pokemon", &rows, None);
        assert_eq!(entities[0].reference.primary_slot_str(), "25");
    }
}
