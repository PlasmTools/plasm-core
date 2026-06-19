//! Ensure relation traversal rows are typed target-entity rows (decode + GET hydrate).

use std::sync::Arc;

use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{EntityName, Expr, GetExpr, Ref, CGS};
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
    trace: Option<&PlasmTraceContext>,
    plan_shared: Option<&PlanLineExecuteShared>,
) -> Result<CachedEntity, String> {
    let scoped = entry_scoped_execute_session(es, Some(target))?;
    let slot = reference.primary_slot_str();
    if slot.is_empty() {
        return Err(format!(
            "relation hydrate GET: empty identity for `{}`",
            reference
        ));
    }
    let mut get_expr = GetExpr::new(
        EntityName::new(reference.entity_type.as_str().to_string()),
        slot,
    );
    get_expr.catalog_entry_id = Some(target.entry_id.clone());
    let parsed = ParsedExpr {
        expr: Expr::Get(get_expr),
        projection: None,
    };
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
    result.entities.into_iter().next().ok_or_else(|| {
        format!(
            "relation hydrate GET returned no `{}` row",
            reference.entity_type
        )
    })
}

struct HydrateWork {
    index: usize,
    reference: Ref,
}

/// GET-hydrate any relation targets whose cached/embed rows omit declared CGS fields.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn hydrate_relation_entities_if_needed(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    target: &QualifiedEntityKey,
    entities: Vec<CachedEntity>,
    trace: Option<&PlasmTraceContext>,
    max_hydrate: Option<usize>,
    plan_shared: Option<Arc<PlanLineExecuteShared>>,
) -> Result<Vec<CachedEntity>, String> {
    let scoped = entry_scoped_execute_session(es, Some(target))?;
    let cgs = scoped.cgs.as_ref();
    let entity_type = target.entity.as_str();
    let mut entities = entities;
    truncate_to_read_cap(&mut entities, max_hydrate);
    if !relation_entities_need_hydration(cgs, entity_type, &entities) {
        return Ok(entities);
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
        return Ok(entities);
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
                trace_ctx.as_ref(),
                plan_shared.as_deref(),
            )
            .await?;
            Ok((item.index, entity))
        }
    })
    .await?;
    for (index, entity) in hydrated {
        out[index] = Some(entity);
    }

    out.into_iter()
        .enumerate()
        .map(|(i, slot)| slot.ok_or_else(|| format!("relation hydrate missing row at index {i}")))
        .collect()
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
    let hydrated = hydrate_relation_entities_if_needed(
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
    let rows = relation_rows_from_entities(&hydrated, cgs);
    let count = hydrated.len();
    mat.result = Arc::new(ExecutionResult {
        count,
        entities: hydrated,
        has_more: mat.result.has_more,
        pagination_resume: mat.result.pagination_resume.clone(),
        paging_handle: mat.result.paging_handle.clone(),
        source: mat.result.source,
        stats: mat.result.stats.clone(),
        request_fingerprints: mat.result.request_fingerprints.clone(),
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
        }
    }

    #[test]
    fn incomplete_entity_detected_when_cgs_field_missing() {
        let cgs = langmatrix_cgs();
        let entity = stub_langitem_entity();
        assert!(entity_row_schema_incomplete(&cgs, "LangItem", &entity));
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
