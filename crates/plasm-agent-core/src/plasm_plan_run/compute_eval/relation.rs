//! Relation traversal materialization during plan execute.

#![allow(clippy::too_many_arguments)]

use super::super::*;

#[derive(Debug, thiserror::Error)]
pub(crate) enum EntityRowConversionError {
    #[error(transparent)]
    Decode(#[from] plasm_core::row_contract::RowDecodeError),
}

impl From<EntityRowConversionError> for ExecutionFailure {
    fn from(error: EntityRowConversionError) -> Self {
        Self::new(
            plasm_runtime::FailureCause::Runtime,
            "row_decode",
            error.to_string(),
        )
    }
}
use super::compute_ops::compute_fingerprint;
use super::eval::{
    instantiate_parsed_expr_plan_inputs, instantiate_parsed_expr_plan_inputs_with_rows,
    wire_coercion_by_alias_from_inputs,
};
use super::materialized_result_use_inputs_with_source_row;
use super::relation_coverage::embedded_collection;

fn relation_failure(code: &'static str, diagnostic: impl Into<String>) -> ExecutionFailure {
    ExecutionFailure::new(
        plasm_runtime::FailureCause::Program,
        code,
        diagnostic.into(),
    )
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RelationParentError {
    #[error(transparent)]
    CatalogOwnership(#[from] crate::catalog_ownership::CatalogOwnershipError),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ParentGetWireRowsError {
    #[error("parent-get wire-row extraction requires a parent-get relation materialization")]
    UnexpectedMaterialization,
    #[error("source entity `{entity}` is absent from its catalog")]
    SourceEntityMissing { entity: String },
    #[error("entity `{entity}` has no relation `{relation}`")]
    RelationMissing { entity: String, relation: String },
}

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
) -> Result<MaterializedNode, ExecutionFailure> {
    if relation_parent_row_missing(materialized, relation, es).map_err(|diagnostic| {
        relation_failure("relation_parent_validation_failed", diagnostic.to_string())
    })? {
        return finalize_empty_relation_materialized_node(
            st,
            es,
            session_id,
            &ValidatedPlanNode::RelationTraversal(relation.clone()),
            relation,
            trace,
            crate::plan_read_bounds::effective_relation_read_cap(relation),
            materialized
                .get(&relation.relation.source)
                .ok_or_else(|| {
                    relation_failure(
                        "relation_parent_not_materialized",
                        "relation parent was not materialized",
                    )
                })?
                .result
                .collection
                .flat_map(&"empty_relation", &[])?,
        )
        .await;
    }
    let pe = ParsedExpr {
        expr: relation.relation.ir.expr.clone(),
        projection: relation.relation.ir.projection.clone(),
        field_dot_extract: None,
    };
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target)).map_err(
        |diagnostic| relation_failure("relation_catalog_scope_unavailable", diagnostic.to_string()),
    )?;
    let parsed = instantiate_parsed_expr_plan_inputs(
        pe,
        &scoped_es.cgs,
        &relation.uses_result,
        materialized,
    )
    .map_err(ExecutionFailure::from)?;
    let expr_label = &crate::plan_dry_display::render_executable_expr(
        &relation.relation.ir.expr,
        relation.relation.ir.projection.as_deref(),
        Some(es),
    );
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
            value_shapes: Vec::new(),
            optional_fields: Default::default(),
            qualified_entity: relation.relation.target.clone(),
            display: crate::expr_display::expr_display(&parsed.expr),
            projection: parsed.projection,
            row_source: inline_row_source(&[]),
            row_identities: row_identities_from_entities(
                &scoped_es,
                relation.relation.target.entity.as_str(),
                result.entities(),
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

/// True when the relation source produced no rows or a row without a usable identity (avoid null-id GET).
fn relation_parent_row_missing(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    relation: &ValidatedRelationTraversalNode,
    es: &ExecuteSession,
) -> Result<bool, RelationParentError> {
    let Some(source_mat) = materialized.get(&relation.relation.source) else {
        return Ok(false);
    };
    if source_mat.result.count() == 0 {
        return Ok(true);
    }
    let Some(rows) = source_mat.row_source.inline_rows() else {
        return Ok(false);
    };
    if rows.is_empty() {
        return Ok(true);
    }
    let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
        es,
        source_mat.qualified_entity.entry_id.as_str(),
        source_mat.qualified_entity.entity.as_str(),
    )?;
    let Some(ent) = cgs.get_entity(source_mat.qualified_entity.entity.as_str()) else {
        return Ok(false);
    };
    let id_field = ent.id_field.as_str();
    Ok(rows.iter().all(|row| {
        row.get(id_field)
            .map(|v| v.is_null() || v.as_str().is_some_and(str::is_empty))
            .unwrap_or(true)
    }))
}

/// Zero-row relation result when the parent binding is empty (no HTTP).
pub(crate) async fn finalize_empty_relation_materialized_node(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node: &ValidatedPlanNode,
    relation: &ValidatedRelationTraversalNode,
    trace: Option<&PlasmTraceContext>,
    read_cap: Option<usize>,
    collection: plasm_runtime::execution::ExecutionCollection,
) -> Result<MaterializedNode, ExecutionFailure> {
    let display = crate::plan_dry_display::render_executable_expr(
        &relation.relation.ir.expr,
        relation.relation.ir.projection.as_deref(),
        Some(es),
    );
    finalize_typed_relation_materialized_node(
        st,
        es,
        session_id,
        &relation.relation.target,
        MaterializedNode {
            value_shapes: Vec::new(),
            optional_fields: Default::default(),
            qualified_entity: relation.relation.target.clone(),
            display,
            projection: relation.relation.ir.projection.clone(),
            row_source: inline_row_source(&[]),
            row_identities: vec![],
            result: Arc::new(ExecutionResult {
                collection: collection.with_materialization(vec![].into())?,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Cache,
                stats: ExecutionStats::default(),
                request_fingerprints: vec![compute_fingerprint(node, &[], &[]).map_err(
                    |diagnostic| {
                        relation_failure("relation_fingerprint_failed", diagnostic.to_string())
                    },
                )?],
                operations: plasm_runtime::OperationLedger::empty(),
            }),
            artifact: None,
        },
        trace,
        read_cap,
        None,
    )
    .await
}

/// Wire JSON rows extracted along a `from_parent_get` path — preserves nested embeds for chained hops.
pub(crate) fn parent_get_wire_rows(
    source_rows: &[plasm_core::ValueRow],
    relation: &ValidatedRelationTraversalNode,
    source_entity: &str,
    cgs: &CGS,
    target_entity: &str,
) -> Result<Vec<plasm_core::ValueRow>, ParentGetWireRowsError> {
    let rel_name = relation.relation.relation.as_str();
    let path = match &relation.relation.materialize {
        RelationMaterialization::FromParentGet { path, .. }
        | RelationMaterialization::PreferFromParentGet { path, .. } => path,
        _ => return Err(ParentGetWireRowsError::UnexpectedMaterialization),
    };
    let rel_schema = cgs
        .get_entity(source_entity)
        .ok_or_else(|| ParentGetWireRowsError::SourceEntityMissing {
            entity: source_entity.to_owned(),
        })?
        .relations
        .get(rel_name)
        .ok_or_else(|| ParentGetWireRowsError::RelationMissing {
            entity: source_entity.to_owned(),
            relation: rel_name.to_owned(),
        })?;
    Ok(normalize_parent_get_target_rows(
        flatten_from_parent_get_source_rows(source_rows, path, rel_schema.cardinality),
        path,
        Some(cgs),
        target_entity,
    ))
}

pub(crate) struct EmbedRelationGraphSnapshot {
    pub(crate) entities: Vec<CachedEntity>,
    pub(crate) wire_rows: Vec<plasm_core::ValueRow>,
    pub(crate) read_cap: Option<usize>,
}

/// One graph lock: resolve embed targets + wire rows; no further `.await` (CEP-4).
pub(crate) async fn snapshot_embed_relation_under_graph_lock(
    scoped_es: &ExecuteSession,
    relation: &ValidatedRelationTraversalNode,
    rel_name: &str,
    target_entity: &str,
    parents: &plasm_core::collection_codec::SharedRows<CachedEntity>,
    wire_fallback_rows: Option<&[plasm_core::ValueRow]>,
) -> Result<EmbedRelationGraphSnapshot, ExecutionFailure> {
    let guard = scoped_es.lock_graph_cache().await;
    let mat = guard.materialization();
    let mut entities = resolve_embed_target_entities(
        rel_name,
        target_entity,
        parents,
        mat,
        wire_fallback_rows,
        scoped_es.cgs.as_ref(),
    )?;
    let read_cap = crate::plan_read_bounds::effective_relation_read_cap(relation);
    crate::plan_read_bounds::truncate_to_read_cap(&mut entities, read_cap);
    let wire_rows = crate::graph_rehydrate::wire_rows_for_embed_entities(
        &entities,
        scoped_es.cgs.as_ref(),
        mat,
    );
    Ok(EmbedRelationGraphSnapshot {
        entities,
        wire_rows,
        read_cap,
    })
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
) -> Result<Option<MaterializedNode>, ExecutionFailure> {
    let rel_name = relation.relation.relation.as_str();
    let target_entity = relation.relation.target.entity.as_str();
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target)).map_err(
        |diagnostic| relation_failure("relation_catalog_scope_unavailable", diagnostic.to_string()),
    )?;
    let source_cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
        es,
        source_mat.qualified_entity.entry_id.as_str(),
        source_mat.qualified_entity.entity.as_str(),
    )
    .map_err(|error| relation_failure("relation_source_catalog_unavailable", error.to_string()))?;
    let rehydrator =
        crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, source_cgs);
    let parents = source_mat
        .resolve_materialized_source_parents(&rehydrator)
        .await
        .map_err(|diagnostic| {
            relation_failure("relation_parent_rehydration_failed", diagnostic.to_string())
        })?;
    if parents.is_empty() {
        return Ok(None);
    }
    let source_rows: Vec<plasm_core::ValueRow> = rehydrator
        .resolve_row_source_rows(&source_mat.row_source, None)
        .await
        .map_err(|diagnostic| {
            relation_failure("relation_parent_rows_unavailable", diagnostic.to_string())
        })?;
    let wire_extracted = parent_get_wire_rows(
        &source_rows,
        relation,
        source_mat.qualified_entity.entity.as_str(),
        source_cgs,
        target_entity,
    )
    .ok()
    .filter(|rows| !rows.is_empty());
    let relations_on_parents = parents.iter().all(|p| p.relations.contains_key(rel_name));
    if !relations_on_parents && wire_extracted.is_none() {
        return Ok(None);
    }
    let snapshot = snapshot_embed_relation_under_graph_lock(
        &scoped_es,
        relation,
        rel_name,
        target_entity,
        &parents,
        wire_extracted.as_deref(),
    )
    .await?;
    let coverage = embedded_collection(
        &source_mat.result,
        &parents,
        rel_name,
        target_entity,
        snapshot.entities.iter().map(|row| &row.reference),
        snapshot.read_cap,
    )?;
    if snapshot.entities.is_empty() {
        if relations_on_parents {
            return finalize_empty_relation_materialized_node(
                st,
                es,
                session_id,
                node,
                relation,
                trace,
                crate::plan_read_bounds::effective_relation_read_cap(relation),
                coverage,
            )
            .await
            .map(Some);
        }
        return Ok(None);
    }
    let count = snapshot.entities.len();
    let display = format!("plan.relation({}) cached_embed", relation.id.as_str());
    finalize_embed_relation_materialized_node(
        st,
        es,
        session_id,
        node,
        relation,
        &scoped_es,
        target_entity,
        snapshot.entities,
        snapshot.wire_rows,
        Some(&source_rows),
        display.clone(),
        vec![display],
        trace,
        snapshot.read_cap,
        count,
        coverage,
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
    source_rows: &[plasm_core::ValueRow],
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    trace: Option<&PlasmTraceContext>,
    sink: Option<&McpPlasmTraceSink>,
    plan_shared: Option<Arc<crate::plan_execute_shared::PlanLineExecuteShared>>,
) -> Result<MaterializedNode, ExecutionFailure> {
    let pe = ParsedExpr {
        expr: relation.relation.ir.expr.clone(),
        projection: relation.relation.ir.projection.clone(),
        field_dot_extract: None,
    };
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target)).map_err(
        |diagnostic| relation_failure("relation_catalog_scope_unavailable", diagnostic.to_string()),
    )?;
    let source_node = &relation.relation.source;
    let base_display = crate::plan_dry_display::render_executable_expr(
        &relation.relation.ir.expr,
        relation.relation.ir.projection.as_deref(),
        Some(es),
    );

    let read_cap = crate::plan_read_bounds::effective_relation_read_cap(relation);
    let parent_row_cap = source_rows.len();
    let mut jobs = Vec::new();
    for (row_index, source_row) in source_rows.iter().enumerate().take(parent_row_cap) {
        let row_identity = source_mat
            .row_identities
            .get(row_index)
            .and_then(|i| i.as_ref())
            .cloned();
        let mut input_rows = materialized_result_use_inputs_with_source_row(
            materialized,
            &relation.uses_result,
            source_node,
            source_row,
            row_identity,
        )?;
        let wire_coercion_by_alias = wire_coercion_by_alias_from_inputs(es, &mut input_rows)
            .map_err(|diagnostic| {
                relation_failure("relation_input_coercion_failed", diagnostic.to_string())
            })?;
        let parsed = instantiate_parsed_expr_plan_inputs_with_rows(
            pe.clone(),
            &scoped_es.cgs,
            &input_rows,
            &wire_coercion_by_alias,
        )
        .map_err(ExecutionFailure::from)?;
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
        &source_mat.result.collection,
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
pub(crate) fn rows_to_entities(
    entity: &str,
    rows: &[plasm_core::ValueRow],
) -> Result<Vec<CachedEntity>, EntityRowConversionError> {
    rows_to_entities_with_refs(entity, rows, None)
}

/// Hoist nested parent-get embed rows (e.g. `{ "pokemon": { "name": "x" } }`) to target entity shape.
pub(crate) fn normalize_parent_get_target_rows(
    rows: Vec<plasm_core::Value>,
    path: &[plasm_core::JsonPathSegment],
    cgs: Option<&CGS>,
    entity: &str,
) -> Vec<plasm_core::ValueRow> {
    let embed_key = path.iter().rev().find_map(|seg| match seg {
        plasm_core::JsonPathSegment::Key { key } => Some(key.as_str()),
        _ => None,
    });
    rows.into_iter()
        .map(|row| {
            plasm_core::ValueRow::from_output(normalize_parent_get_target_row(
                row, embed_key, cgs, entity,
            ))
        })
        .collect()
}

fn normalize_parent_get_target_row(
    row: plasm_core::Value,
    embed_key: Option<&str>,
    cgs: Option<&CGS>,
    entity: &str,
) -> plasm_core::Value {
    let mut v = row;
    if let Some(key) = embed_key {
        v = hoist_embed_key_object(v, key);
    }
    if let Some(ent) = cgs.and_then(|c| c.get_entity(entity)) {
        let id_field = ent.id_field.as_str();
        if wire_id_from_row(&v, id_field, ent.id_from.as_deref()).is_none() {
            if let Some(path) = ent.id_from.as_deref() {
                if let Some(extracted) = super::super::row_values::value_at_segments(&v, path) {
                    if let Some(id) = json_value_to_wire_id(extracted) {
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert(id_field.to_string(), plasm_core::Value::String(id));
                        }
                    }
                }
            }
        }
    }
    v
}

fn hoist_embed_key_object(row: plasm_core::Value, embed_key: &str) -> plasm_core::Value {
    if let Some(obj) = row.as_object() {
        if let Some(inner) = obj.get(embed_key) {
            if inner.is_object() {
                return inner.clone();
            }
        }
    }
    row
}

fn json_value_to_wire_id(v: &plasm_core::Value) -> Option<String> {
    if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    if let Some(n) = v.as_integer() {
        return Some(n.to_string());
    }
    if let Some(n) = v.as_unsigned() {
        return Some(n.to_string());
    }
    None
}

fn wire_id_from_row(
    row: &plasm_core::Value,
    id_field: &str,
    id_from: Option<&[String]>,
) -> Option<String> {
    if let Some(id) = row.get(id_field).and_then(json_value_to_wire_id) {
        return Some(id);
    }
    id_from
        .and_then(|path| super::super::row_values::value_at_segments(row, path))
        .and_then(json_value_to_wire_id)
        .or_else(|| row.get("id").and_then(json_value_to_wire_id))
        .or_else(|| json_value_to_wire_id(row))
}

pub(crate) fn rows_to_entities_with_refs(
    entity: &str,
    rows: &[plasm_core::ValueRow],
    cgs: Option<&CGS>,
) -> Result<Vec<CachedEntity>, EntityRowConversionError> {
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
            if row.get("_ref").is_some() {
                let mut wire = row.clone();
                {
                    let object = wire.fields_mut();
                    for key in ["_version", "_last_updated", "_completeness"] {
                        object.shift_remove(key);
                    }
                }
                let semantic =
                    plasm_core::row_contract::RowCodec::new(cgs).decode_values(entity, &wire)?;
                return Ok(CachedEntity::from_row(
                    &semantic,
                    0,
                    EntityCompleteness::Complete,
                ));
            }
            // Unidentified computed rows and raw API embeds are a distinct input lane.
            let fields = row
                .iter()
                .map(|(key, value)| (key.clone(), TypedFieldValue::from(value.clone())))
                .collect();
            let reference = Ref::new(
                EntityName::new(entity.to_string()),
                wire_id_from_row(row, id_field, id_from)
                    .unwrap_or_else(|| format!("synthetic-{}", idx + 1)),
            );
            Ok(CachedEntity {
                reference,
                fields,
                relations: IndexMap::new(),
                last_updated: 0,
                version: 1,
                completeness: EntityCompleteness::Complete,
                unavailable_fields: Default::default(),
            })
        })
        .collect()
}

/// Resolve embed-target entities from session-graph refs, else synthesize from wire JSON rows.
pub(crate) fn resolve_embed_target_entities(
    rel_name: &str,
    target_entity: &str,
    parents: &plasm_core::collection_codec::SharedRows<CachedEntity>,
    mat: &plasm_runtime::SessionMaterialization,
    wire_fallback_rows: Option<&[plasm_core::ValueRow]>,
    cgs: &CGS,
) -> Result<Vec<CachedEntity>, EntityRowConversionError> {
    match crate::graph_rehydrate::collect_all_embedded_relation_targets(
        rel_name,
        target_entity,
        parents,
        mat,
    ) {
        Some(mut entities) => {
            // Prefer an already-observed full wire row for a missing cache child.
            // Match its identity; unrelated fallback rows cannot enter the relation.
            if let Some(rows) = wire_fallback_rows {
                let fallback = rows_to_entities_with_refs(target_entity, rows, Some(cgs))?;
                for entity in &mut entities {
                    if entity.completeness == plasm_runtime::EntityCompleteness::Summary
                        && entity.fields.is_empty()
                    {
                        if let Some(observed) = fallback
                            .iter()
                            .find(|row| row.reference == entity.reference)
                        {
                            *entity = observed.clone();
                        }
                    }
                }
            }
            Ok(entities)
        }
        None => wire_fallback_rows
            .map(|rows| rows_to_entities_with_refs(target_entity, rows, Some(cgs)))
            .transpose()
            .map(|rows| rows.unwrap_or_default()),
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
    wire_rows: Vec<plasm_core::ValueRow>,
    fingerprint_rows: Option<&[plasm_core::ValueRow]>,
    display: String,
    artifact_labels: Vec<String>,
    trace: Option<&PlasmTraceContext>,
    read_cap: Option<usize>,
    cache_hits: usize,
    collection: plasm_runtime::execution::ExecutionCollection,
) -> Result<MaterializedNode, ExecutionFailure> {
    let full_result = ExecutionResult {
        collection: collection.with_materialization(entities.into())?,
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
            &[],
        )
        .map_err(|diagnostic| {
            relation_failure("relation_fingerprint_failed", diagnostic.to_string())
        })?],
        operations: plasm_runtime::OperationLedger::empty(),
    };
    let parsed_preimage = crate::plasm_plan_run::evidence_plan::parsed_expr_for_plan_node(node);
    let mut materialized = finalize_typed_relation_materialized_node(
        st,
        es,
        session_id,
        &relation.relation.target,
        MaterializedNode {
            value_shapes: Vec::new(),
            optional_fields: Default::default(),
            qualified_entity: relation.relation.target.clone(),
            display,
            projection: relation.relation.ir.projection.clone(),
            row_source: inline_row_source_owned(wire_rows),
            row_identities: row_identities_from_entities(
                scoped_es,
                target_entity,
                full_result.entities(),
            ),
            result: Arc::new(full_result),
            artifact: None,
        },
        trace,
        read_cap,
        None,
    )
    .await?;
    let artifact = archive_plasm_result_snapshot(
        st,
        es,
        session_id,
        Some(relation.relation.target.entry_id.as_str()),
        artifact_labels,
        &parsed_preimage,
        &materialized.result,
        trace,
    )
    .await
    .map_err(|diagnostic| {
        relation_failure(
            "relation_artifact_persistence_failed",
            diagnostic.to_string(),
        )
    })?;
    materialized.artifact = Some(artifact);
    Ok(materialized)
}

#[cfg(test)]
mod parent_get_row_tests {
    use super::*;
    use plasm_core::JsonPathSegment;

    #[test]
    fn embedded_wire_identity_matches_detail_decoder() {
        for row in [
            crate::fixture_value!({"id": 8, "name": "Eight"}),
            crate::fixture_value!(8),
        ] {
            assert_eq!(
                wire_id_from_row(&row, "resource_id", None),
                Some("8".into())
            );
        }
        assert_eq!(
            wire_id_from_row(
                &crate::fixture_value!({"resource_id": 9, "id": 8}),
                "resource_id",
                None
            ),
            Some("9".into())
        );
    }

    #[test]
    fn missing_cached_child_uses_only_matching_observed_wire_row() {
        let cgs = plasm_core::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .unwrap();
        let parent = CachedEntity {
            reference: plasm_core::Ref::new("LangItem", "i1"),
            fields: Default::default(),
            relations: indexmap::IndexMap::from([(
                "lines".into(),
                vec![plasm_core::Ref::new("LangLine", "l1")],
            )])
            .into_iter()
            .map(|(key, refs)| {
                (
                    key,
                    plasm_core::row_contract::RelationMembership::observe(
                        None,
                        &"relation_fixture",
                        refs,
                        None,
                    )
                    .unwrap(),
                )
            })
            .collect(),
            last_updated: 0,
            version: 0,
            completeness: plasm_runtime::EntityCompleteness::Complete,
            unavailable_fields: Default::default(),
        };
        let wire = [
            crate::fixture_row!({"id":"l99","item_id":"i1","note":"unrelated"}),
            crate::fixture_row!({"id":"l1","item_id":"i1","note":"observed"}),
        ];
        let rows = resolve_embed_target_entities(
            "lines",
            "LangLine",
            &vec![parent].into(),
            &plasm_runtime::SessionMaterialization::new(),
            Some(&wire),
            &cgs,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reference, plasm_core::Ref::new("LangLine", "l1"));
        assert_eq!(rows[0].payload_to_json()["note"], "observed");
    }

    #[test]
    fn normalize_hoists_nested_pokemon_embed() {
        let path = vec![JsonPathSegment::Key {
            key: "pokemon".into(),
        }];
        let rows = normalize_parent_get_target_rows(
            vec![crate::fixture_value!({
                "pokemon": { "name": "jolteon", "url": "https://pokeapi.co/api/v2/pokemon/135/" }
            })],
            &path,
            None,
            "Pokemon",
        );
        assert_eq!(rows[0]["name"].as_str(), Some("jolteon"));
    }

    #[test]
    fn execution_rows_reject_display_identity_and_keep_metadata_out_of_fields() {
        let cgs = plasm_core::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/hydration_boundary_matrix"),
        )
        .unwrap();
        for malformed in [
            crate::fixture_value!("Owner:3"),
            plasm_core::RefWire::from_ref(&Ref::new("Note", "3")).to_value(),
        ] {
            let row = crate::fixture_row!({"_ref":malformed,"owner_id":3});
            assert!(rows_to_entities_with_refs("Owner", &[row], Some(&cgs)).is_err());
        }
        let mut entity = CachedEntity::new(Ref::new("Owner", "3"), 44);
        entity.fields.insert(
            "_tag".into(),
            TypedFieldValue::from(plasm_core::Value::String("public".into())),
        );
        let row = plasm_runtime::entity_to_row_values(&entity, Some(&cgs));
        let rows = rows_to_entities_with_refs("Owner", &[row], Some(&cgs)).unwrap();
        assert_eq!(rows[0].reference, entity.reference);
        assert!(rows[0].fields.contains_key("_tag"));
        for key in ["_ref", "_version", "_last_updated", "_completeness"] {
            assert!(!rows[0].fields.contains_key(key));
        }
    }

    #[test]
    fn json_rows_avoids_synthetic_when_id_present() {
        let rows = vec![crate::fixture_row!({ "name": "pikachu", "id": 25 })];
        let entities = rows_to_entities_with_refs("Pokemon", &rows, None).unwrap();
        assert_eq!(entities[0].reference.primary_slot_str(), "25");
    }
}
