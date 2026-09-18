//! Ensure relation traversal rows are typed target-entity rows (decode + GET hydrate).

use std::sync::Arc;

use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{Expr, GetExpr, Ref, CGS};
use plasm_runtime::{entity_to_agent_row_json, CachedEntity, ExecutionResult};

use crate::execute_session::ExecuteSession;
use crate::http_execute::execute_plasm_parsed_expr;
use crate::plan_execute_shared::PlanLineExecuteShared;
use crate::plan_read_bounds::truncate_to_read_cap;
use crate::plasm_plan::QualifiedEntityKey;
use crate::plasm_plan_run::plan_bounded_parallel::{bounded_parallel_map, BoundedParallelConfig};
use crate::server_state::PlasmHostState;
use crate::trace_sink_emit::PlasmTraceContext;

use super::{entry_scoped_execute_session, MaterializedNode};

/// True when a cached entity is missing any field declared on the target CGS entity.
pub(crate) fn entity_row_schema_incomplete(
    cgs: &CGS,
    entity_type: &str,
    entity: &CachedEntity,
) -> bool {
    if entity.completeness == plasm_runtime::EntityCompleteness::Summary && entity.fields.is_empty()
    {
        return true;
    }
    let Some(def) = cgs.get_entity(entity_type) else {
        return false;
    };
    for field_name in def.fields.keys() {
        if !entity.fields.contains_key(field_name.as_str()) {
            return true;
        }
    }
    false
}

pub(crate) fn relation_entities_need_hydration(
    cgs: &CGS,
    entity_type: &str,
    entities: &[CachedEntity],
) -> bool {
    entities
        .iter()
        .any(|e| entity_row_schema_incomplete(cgs, entity_type, e))
}

#[allow(clippy::too_many_arguments)]
async fn fetch_entity_get_by_ref(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    target: &QualifiedEntityKey,
    reference: &Ref,
    get_capability: Option<&str>,
    trace: Option<&PlasmTraceContext>,
    plan_shared: Option<&PlanLineExecuteShared>,
) -> Result<
    (
        CachedEntity,
        plasm_runtime::ExecutionStats,
        Vec<String>,
        plasm_runtime::ExecutionSource,
    ),
    String,
> {
    let scoped = entry_scoped_execute_session(es, Some(target))?;
    if reference.primary_slot_str().is_empty() {
        return Err(format!(
            "relation hydrate GET: empty identity for `{}`",
            reference
        ));
    }
    let mut get_expr = GetExpr::from_ref(reference.clone());
    if let Some(cap) = get_capability {
        get_expr = get_expr.with_capability(cap);
    }
    get_expr.catalog_entry_id = plasm_core::CatalogEntryStamp::some(
        plasm_core::RegistryEntryId::from(target.entry_id.as_str()),
    );
    let parsed = ParsedExpr::from_expr(Expr::Get(get_expr));
    let (_, result, _) = execute_plasm_parsed_expr(
        st,
        &scoped,
        session_id,
        "relation hydrate get",
        parsed,
        trace,
        0,
        None,
        None,
        None,
        plan_shared,
    )
    .await?;
    let entity = result.entities.into_iter().next().ok_or_else(|| {
        format!(
            "relation hydrate GET returned no `{}` row",
            reference.entity_type
        )
    })?;
    Ok((
        entity,
        result.stats,
        result.request_fingerprints,
        result.source,
    ))
}

#[derive(Default)]
struct RelationHydration {
    entities: Vec<CachedEntity>,
    stats: plasm_runtime::ExecutionStats,
    request_fingerprints: Vec<String>,
    source: Option<plasm_runtime::ExecutionSource>,
}

struct HydrateWork {
    index: usize,
    reference: Ref,
}

/// GET-hydrate any relation targets whose cached/embed rows omit declared CGS fields.
#[allow(clippy::too_many_arguments)]
async fn hydrate_relation_entities_if_needed(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    target: &QualifiedEntityKey,
    entities: Vec<CachedEntity>,
    trace: Option<&PlasmTraceContext>,
    max_hydrate: Option<usize>,
    plan_shared: Option<Arc<PlanLineExecuteShared>>,
) -> Result<RelationHydration, String> {
    let scoped = entry_scoped_execute_session(es, Some(target))?;
    let cgs = scoped.cgs.as_ref();
    let entity_type = target.entity.as_str();
    let mut entities = entities;
    truncate_to_read_cap(&mut entities, max_hydrate);
    if !relation_entities_need_hydration(cgs, entity_type, &entities) {
        return Ok(RelationHydration {
            entities,
            ..Default::default()
        });
    }

    let mut out: Vec<Option<CachedEntity>> = entities.iter().cloned().map(Some).collect();
    let mut work = Vec::new();
    for (index, entity) in entities.iter().enumerate() {
        if entity_row_schema_incomplete(cgs, entity_type, entity) {
            work.push(HydrateWork {
                index,
                reference: entity.reference.clone(),
            });
        }
    }
    if work.is_empty() {
        return Ok(RelationHydration {
            entities,
            ..Default::default()
        });
    }
    {
        let mut cache = scoped.lock_graph_cache().await;
        for item in &work {
            cache.remove(&item.reference);
        }
    }

    let st = st.clone();
    let es = es.clone();
    let session_id = session_id.to_string();
    let target = target.clone();
    let trace_ctx = trace.cloned();
    let plan_shared = plan_shared.clone();
    // Preserve the plan's batch bound. The shared outbound limiter enforces
    // conditional backend caps across this batch and independent branches.
    let cfg = BoundedParallelConfig::for_plan_http(None);
    let hydrated = bounded_parallel_map(work, cfg, move |item| {
        let st = st.clone();
        let es = es.clone();
        let session_id = session_id.clone();
        let target = target.clone();
        let trace_ctx = trace_ctx.clone();
        let plan_shared = plan_shared.clone();
        async move {
            let entity = fetch_entity_get_by_ref(
                &st,
                &es,
                session_id.as_str(),
                &target,
                &item.reference,
                None,
                trace_ctx.as_ref(),
                plan_shared.as_deref(),
            )
            .await?;
            Ok((item.index, entity))
        }
    })
    .await?;
    let mut summary = RelationHydration::default();
    for (index, (entity, stats, fingerprints, source)) in hydrated {
        out[index] = Some(entity);
        super::plan_fanout_parallel::merge_execution_stats(
            &mut summary.stats,
            &stats,
            super::plan_fanout_parallel::ExecutionStatsFold::Telemetry,
        );
        summary.request_fingerprints.extend(fingerprints);
        summary.source = Some(match summary.source {
            Some(prior) => super::plan_fanout_parallel::combine_execution_source(prior, source),
            None => source,
        });
    }
    summary.entities = out
        .into_iter()
        .enumerate()
        .map(|(i, slot)| slot.ok_or_else(|| format!("relation hydrate missing row at index {i}")))
        .collect::<Result<_, _>>()?;
    Ok(summary)
}

/// Rebuild agent row JSON from typed entities after hydration.
pub(crate) fn relation_rows_from_entities(
    entities: &[CachedEntity],
    cgs: &CGS,
) -> Vec<serde_json::Value> {
    entities
        .iter()
        .map(|e| entity_to_agent_row_json(e, Some(cgs)))
        .collect()
}

/// Preserve nested wire embed keys (e.g. `detail` on LangSummary) not declared on the target
/// entity so chained `from_parent_get` hops can read the next path segment.
fn merge_wire_embed_superset_rows(
    prior_wire: &[serde_json::Value],
    entity_rows: &[serde_json::Value],
    cgs: &CGS,
    entity_type: &str,
) -> Vec<serde_json::Value> {
    use std::collections::HashSet;

    let declared: HashSet<&str> = cgs
        .get_entity(entity_type)
        .map(|e| e.fields.keys().map(|k| k.as_str()).collect())
        .unwrap_or_default();
    entity_rows
        .iter()
        .enumerate()
        .map(|(i, entity_row)| {
            prior_wire
                .get(i)
                .map(|wire| merge_wire_embed_superset_row(wire, entity_row, &declared))
                .unwrap_or_else(|| entity_row.clone())
        })
        .collect()
}

fn merge_wire_embed_superset_row(
    wire: &serde_json::Value,
    entity_row: &serde_json::Value,
    declared_fields: &std::collections::HashSet<&str>,
) -> serde_json::Value {
    let mut merged = entity_row.clone();
    let (Some(wire_obj), Some(merged_obj)) = (wire.as_object(), merged.as_object_mut()) else {
        return merged;
    };
    for (k, v) in wire_obj {
        if !declared_fields.contains(k.as_str()) {
            merged_obj.insert(k.clone(), v.clone());
        }
    }
    merged
}

/// Hydrate incomplete relation targets and normalize materialized row JSON.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn finalize_typed_relation_materialized_node(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    target: &QualifiedEntityKey,
    mut mat: MaterializedNode,
    trace: Option<&PlasmTraceContext>,
    max_hydrate: Option<usize>,
    plan_shared: Option<Arc<PlanLineExecuteShared>>,
) -> Result<MaterializedNode, String> {
    let scoped = entry_scoped_execute_session(es, Some(target))?;
    let cgs = scoped.cgs.as_ref();
    let entity_type = target.entity.as_str();
    let hydration = hydrate_relation_entities_if_needed(
        st,
        es,
        session_id,
        target,
        mat.result.entities.clone(),
        trace,
        max_hydrate,
        plan_shared.clone(),
    )
    .await?;
    let hydrated = hydration.entities;
    let mut stats = mat.result.stats.clone();
    if hydration.source.is_some() {
        super::plan_fanout_parallel::merge_execution_stats(
            &mut stats,
            &hydration.stats,
            super::plan_fanout_parallel::ExecutionStatsFold::Telemetry,
        );
    }
    let mut fingerprints = mat.result.request_fingerprints.clone();
    fingerprints.extend(hydration.request_fingerprints);
    let source = hydration
        .source
        .map(|source| {
            super::plan_fanout_parallel::combine_execution_source(mat.result.source, source)
        })
        .unwrap_or(mat.result.source);
    let entity_rows = relation_rows_from_entities(&hydrated, cgs);
    let rows = match mat.row_source.inline_rows() {
        Some(prior) if !prior.is_empty() && prior.len() == hydrated.len() => {
            merge_wire_embed_superset_rows(prior, &entity_rows, cgs, entity_type)
        }
        _ => entity_rows,
    };
    let count = hydrated.len();
    mat.result = Arc::new(ExecutionResult {
        count,
        entities: hydrated,
        has_more: mat.result.has_more,
        coverage: mat.result.coverage,
        pagination_resume: mat.result.pagination_resume.clone(),
        paging_handle: mat.result.paging_handle.clone(),
        source,
        stats,
        request_fingerprints: fingerprints,
        operations: mat.result.operations.clone(),
    });
    mat.row_source = super::inline_row_source(&rows);
    mat.row_identities =
        super::row_identities_from_entities(&scoped, entity_type, &mat.result.entities);
    Ok(mat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use plasm_core::loader::load_schema_dir;
    use plasm_core::Ref;
    use std::path::PathBuf;

    fn langmatrix_cgs() -> CGS {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        load_schema_dir(&dir).expect("plasm_language_matrix")
    }

    fn stub_langitem_entity() -> CachedEntity {
        let mut fields = IndexMap::new();
        fields.insert(
            "id".into(),
            plasm_core::TypedFieldValue::from(plasm_core::Value::String("item-1".into())),
        );
        fields.insert(
            "title".into(),
            plasm_core::TypedFieldValue::from(plasm_core::Value::String("matrix item".into())),
        );
        CachedEntity {
            reference: Ref::new("LangItem", "item-1"),
            fields,
            relations: IndexMap::new(),
            last_updated: 0,
            version: 0,
            completeness: plasm_runtime::EntityCompleteness::Summary,
            unavailable_fields: Default::default(),
        }
    }

    #[test]
    fn incomplete_entity_detected_when_cgs_field_missing() {
        let cgs = langmatrix_cgs();
        let entity = stub_langitem_entity();
        assert!(entity_row_schema_incomplete(&cgs, "LangItem", &entity));
    }

    #[test]
    fn merge_wire_embed_superset_preserves_nested_detail_for_chained_hop() {
        use plasm_core::JsonPathSegment;
        use plasm_core::{flatten_from_parent_get_source_rows, Cardinality};

        let cgs = langmatrix_cgs();
        let item_row = serde_json::json!({
            "id": "i1",
            "title": "Alpha",
            "summary": {
                "id": "sum-i1",
                "headline": "Alpha summary",
                "detail": { "id": "det-i1", "body": "nested detail" }
            }
        });
        let summary_path = [JsonPathSegment::Key {
            key: "summary".into(),
        }];
        let wire_summary = flatten_from_parent_get_source_rows(
            std::slice::from_ref(&item_row),
            &summary_path,
            Cardinality::One,
        );
        assert_eq!(wire_summary.len(), 1);
        let summary_entity_row = serde_json::json!({
            "id": "sum-i1",
            "headline": "Alpha summary"
        });
        let merged = merge_wire_embed_superset_rows(
            &wire_summary,
            std::slice::from_ref(&summary_entity_row),
            &cgs,
            "LangSummary",
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].pointer("/detail/body").and_then(|v| v.as_str()),
            Some("nested detail"),
            "chained hop must retain nested detail embed on summary row_source"
        );

        let detail_path = [JsonPathSegment::Key {
            key: "detail".into(),
        }];
        let detail_rows =
            flatten_from_parent_get_source_rows(&merged, &detail_path, Cardinality::One);
        assert_eq!(detail_rows.len(), 1);
        assert_eq!(
            detail_rows[0].pointer("/body").and_then(|v| v.as_str()),
            Some("nested detail")
        );
    }

    #[test]
    fn truncate_to_read_cap_limits_hydrate_input() {
        let mut entities: Vec<CachedEntity> = (0..10)
            .map(|i| {
                let mut e = stub_langitem_entity();
                e.reference = Ref::new("LangItem", format!("item-{i}"));
                e
            })
            .collect();
        truncate_to_read_cap(&mut entities, Some(3));
        assert_eq!(entities.len(), 3);
    }
}
