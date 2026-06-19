//! PreferFromParentGet hydrate fallback: wire/path ref extraction and GET job planning.

use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{
    prefer_hydrate_embed_path, CapabilityName, Cardinality, EntityName, Expr, GetExpr, Ref,
    RelationScopedFallback, CGS,
};
use plasm_runtime::CachedEntity;

use crate::execute_session::ExecuteSession;
use crate::plasm_plan_run::compute_eval::{
    json_rows_to_entities_with_refs, normalize_parent_get_target_rows,
};
use crate::plasm_plan_run::plan_fanout_parallel::{push_verified_row_job, PlanLineJob};
use crate::plasm_plan_run::relation_hydrate::entity_row_schema_incomplete;

use super::flatten_from_parent_get_source_rows;

/// Normalize wire rows for a prefer embed path (shared by wire hit + hydrate fallback).
pub(crate) fn prefer_embed_wire_rows(
    source_row: &serde_json::Value,
    embed_path: &[plasm_core::JsonPathSegment],
    cardinality: Cardinality,
    cgs: &CGS,
    target_entity: &str,
) -> Vec<serde_json::Value> {
    normalize_parent_get_target_rows(
        flatten_from_parent_get_source_rows(
            std::slice::from_ref(source_row),
            embed_path,
            cardinality,
        ),
        embed_path,
        Some(cgs),
        target_entity,
    )
}

/// Collect target refs from wire embed path and/or decoded parent relation refs.
pub(crate) fn prefer_hydrate_target_refs(
    source_row: &serde_json::Value,
    embed_path: &[plasm_core::JsonPathSegment],
    parent: Option<&CachedEntity>,
    rel_name: &str,
    target_entity: &str,
    cgs: &CGS,
    cardinality: Cardinality,
) -> Vec<Ref> {
    let wire_rows = prefer_embed_wire_rows(source_row, embed_path, cardinality, cgs, target_entity);
    if !wire_rows.is_empty() {
        return json_rows_to_entities_with_refs(target_entity, &wire_rows, Some(cgs))
            .into_iter()
            .map(|e| e.reference)
            .collect();
    }
    parent
        .and_then(|p| p.relations.get(rel_name))
        .map(|rs| rs.to_vec())
        .unwrap_or_default()
}

/// Split refs into graph-resident rows vs identities needing GET hydrate.
pub(crate) async fn partition_prefer_hydrate_refs(
    scoped_es: &ExecuteSession,
    target_entity: &str,
    refs: Vec<Ref>,
) -> (Vec<CachedEntity>, Vec<Ref>) {
    let guard = scoped_es.lock_graph_cache().await;
    let mat = guard.materialization();
    let mut cached = Vec::new();
    let mut need_get = Vec::new();
    for r in refs {
        match mat.get(&r) {
            Some(e) if !entity_row_schema_incomplete(scoped_es.cgs.as_ref(), target_entity, e) => {
                cached.push(e.clone());
            }
            _ => need_get.push(r),
        }
    }
    (cached, need_get)
}

/// Build verified plan-line GET jobs for hydrate fallback rows.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_prefer_hydrate_get_jobs(
    scoped_jobs: &mut Vec<PlanLineJob>,
    scoped_es: &ExecuteSession,
    node_index: usize,
    row_index: usize,
    base_display: &str,
    target: &crate::plasm_plan::QualifiedEntityKey,
    target_entity: &str,
    get_capability: &CapabilityName,
    refs: impl IntoIterator<Item = Ref>,
) -> Result<(), String> {
    for (sub_index, reference) in refs.into_iter().enumerate() {
        let slot = reference.primary_slot_str();
        if slot.is_empty() {
            continue;
        }
        let mut get_expr = GetExpr::new(EntityName::new(target_entity.to_string()), slot)
            .with_capability(get_capability.clone());
        get_expr.catalog_entry_id = Some(target.entry_id.to_string());
        let parsed = ParsedExpr {
            expr: Expr::Get(get_expr),
            projection: None,
        };
        let expr_label = format!("{base_display} [row {row_index} hydrate {sub_index}]");
        push_verified_row_job(
            scoped_jobs,
            scoped_es,
            node_index,
            row_index,
            expr_label,
            parsed,
        )?;
    }
    Ok(())
}

/// Plan hydrate fallback for one parent row when wire embed did not materialize targets.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn plan_prefer_hydrate_fallback_row(
    scoped_jobs: &mut Vec<PlanLineJob>,
    per_row: &mut [Vec<CachedEntity>],
    scoped_es: &ExecuteSession,
    fallback: &RelationScopedFallback,
    prefer_path: &[plasm_core::JsonPathSegment],
    rel_name: &str,
    target: &crate::plasm_plan::QualifiedEntityKey,
    target_entity: &str,
    cardinality: Cardinality,
    node_index: usize,
    row_index: usize,
    base_display: &str,
    source_row: &serde_json::Value,
    parent: Option<&CachedEntity>,
) -> Result<bool, String> {
    let RelationScopedFallback::HydrateFromEmbedPath { get_capability, .. } = fallback else {
        return Ok(false);
    };
    let Some(embed_path) = prefer_hydrate_embed_path(prefer_path, fallback) else {
        return Ok(false);
    };
    let refs = prefer_hydrate_target_refs(
        source_row,
        embed_path,
        parent,
        rel_name,
        target_entity,
        scoped_es.cgs.as_ref(),
        cardinality,
    );
    if refs.is_empty() {
        return Err(format!(
            "relation `{rel_name}` hydrate_from_embed_path: no embed identities on parent row {row_index}"
        ));
    }
    let (cached, need_get) = partition_prefer_hydrate_refs(scoped_es, target_entity, refs).await;
    per_row[row_index].extend(cached);
    if !need_get.is_empty() {
        push_prefer_hydrate_get_jobs(
            scoped_jobs,
            scoped_es,
            node_index,
            row_index,
            base_display,
            target,
            target_entity,
            get_capability,
            need_get,
        )?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::{EmbedOnMissPolicy, JsonPathSegment, RelationMaterialization};

    fn type_pokemon_prefer_mat() -> RelationMaterialization {
        RelationMaterialization::PreferFromParentGet {
            path: vec![
                JsonPathSegment::Key {
                    key: "pokemon".into(),
                },
                JsonPathSegment::Wildcard { wildcard: true },
                JsonPathSegment::Key {
                    key: "pokemon".into(),
                },
            ],
            on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::HydrateFromEmbedPath {
                path: Vec::new(),
                get_capability: "pokemon_get".into(),
            },
        }
    }

    #[test]
    fn wire_embed_rows_from_type_get_payload() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        let cgs = plasm_core::loader::load_schema(&dir).expect("pokeapi");
        let mat = type_pokemon_prefer_mat();
        let RelationMaterialization::PreferFromParentGet { path, .. } = &mat else {
            panic!("expected prefer");
        };
        let row = serde_json::json!({
            "name": "electric",
            "pokemon": [
                { "pokemon": { "name": "pikachu", "url": "https://pokeapi.co/api/v2/pokemon/25/" } }
            ]
        });
        let wire = prefer_embed_wire_rows(&row, path, Cardinality::Many, &cgs, "Pokemon");
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0]["name"], "pikachu");
    }

    #[test]
    fn hydrate_refs_from_parent_relation_when_wire_empty() {
        use indexmap::IndexMap;
        use plasm_runtime::EntityCompleteness;

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        let cgs = plasm_core::loader::load_schema(&dir).expect("pokeapi");
        let mat = type_pokemon_prefer_mat();
        let RelationMaterialization::PreferFromParentGet { path, .. } = &mat else {
            panic!("expected prefer");
        };
        let row = serde_json::json!({ "name": "electric" });
        let parent = CachedEntity {
            reference: Ref::new("Type", "electric"),
            fields: IndexMap::new(),
            relations: IndexMap::from([(
                "pokemon".to_string(),
                vec![Ref::new("Pokemon", "pikachu")],
            )]),
            last_updated: 0,
            version: 1,
            completeness: EntityCompleteness::Summary,
        };
        let refs = prefer_hydrate_target_refs(
            &row,
            path,
            Some(&parent),
            "pokemon",
            "Pokemon",
            &cgs,
            Cardinality::Many,
        );
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].primary_slot_str(), "pikachu");
    }
}
