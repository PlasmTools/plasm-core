//! Ensure relation traversal rows are typed target-entity rows (decode + GET hydrate).

use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{EntityName, Expr, GetExpr, Ref, CGS};
use plasm_runtime::{entity_to_agent_row_json, CachedEntity};

use crate::execute_session::ExecuteSession;
use crate::http_execute::execute_plasm_parsed_expr;
use crate::plasm_plan::QualifiedEntityKey;
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

async fn fetch_entity_get_by_ref(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    target: &QualifiedEntityKey,
    reference: &Ref,
    trace: Option<&PlasmTraceContext>,
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
    )
    .await?;
    result.entities.into_iter().next().ok_or_else(|| {
        format!(
            "relation hydrate GET returned no `{}` row",
            reference.entity_type
        )
    })
}

/// GET-hydrate any relation targets whose cached/embed rows omit declared CGS fields.
pub(crate) async fn hydrate_relation_entities_if_needed(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    target: &QualifiedEntityKey,
    entities: Vec<CachedEntity>,
    trace: Option<&PlasmTraceContext>,
) -> Result<Vec<CachedEntity>, String> {
    let scoped = entry_scoped_execute_session(es, Some(target))?;
    let cgs = scoped.cgs.as_ref();
    let entity_type = target.entity.as_str();
    if !relation_entities_need_hydration(cgs, entity_type, &entities) {
        return Ok(entities);
    }
    let mut out = Vec::with_capacity(entities.len());
    for entity in entities {
        if !entity_row_schema_incomplete(cgs, entity_type, &entity) {
            out.push(entity);
            continue;
        }
        let mut cache = scoped.lock_graph_cache().await;
        cache.remove(&entity.reference);
        drop(cache);
        let hydrated =
            fetch_entity_get_by_ref(st, es, session_id, target, &entity.reference, trace).await?;
        out.push(hydrated);
    }
    Ok(out)
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
pub(crate) async fn finalize_typed_relation_materialized_node(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    target: &QualifiedEntityKey,
    mut mat: MaterializedNode,
    trace: Option<&PlasmTraceContext>,
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
    )
    .await?;
    let rows = relation_rows_from_entities(&hydrated, cgs);
    let count = hydrated.len();
    mat.result.entities = hydrated;
    mat.result.count = count;
    mat.rows = rows.clone();
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

    fn pokeapi_cgs() -> CGS {
        for p in [
            "apis/pokeapi",
            "plasm-oss/apis/pokeapi",
            "../../apis/pokeapi",
        ] {
            let path = std::path::Path::new(p);
            if path.join("domain.yaml").is_file() {
                return load_schema_dir(path).expect("pokeapi CGS");
            }
        }
        panic!("pokeapi catalog not found");
    }

    fn stub_species_entity() -> CachedEntity {
        let mut fields = IndexMap::new();
        fields.insert(
            "name".into(),
            plasm_core::TypedFieldValue::from(plasm_core::Value::String("pikachu".into())),
        );
        fields.insert(
            "url".into(),
            plasm_core::TypedFieldValue::from(plasm_core::Value::String(
                "https://pokeapi.co/api/v2/pokemon-species/25/".into(),
            )),
        );
        CachedEntity {
            reference: Ref::new("PokemonSpecies", "pikachu"),
            fields,
            relations: IndexMap::new(),
            last_updated: 0,
            version: 1,
            completeness: plasm_runtime::EntityCompleteness::Complete,
        }
    }

    #[test]
    fn relation_rows_from_entities_include_all_cgs_fields() {
        let cgs = pokeapi_cgs();
        let mut entity = stub_species_entity();
        for field in cgs
            .get_entity("PokemonSpecies")
            .expect("species")
            .fields
            .keys()
        {
            if field.as_str() == "name" || field.as_str() == "url" {
                continue;
            }
            entity.fields.insert(
                field.as_str().to_string(),
                plasm_core::TypedFieldValue::from(plasm_core::Value::Integer(190)),
            );
        }
        let rows = relation_rows_from_entities(&[entity], &cgs);
        let row = rows.first().expect("row");
        let obj = row.as_object().expect("object row");
        assert!(obj.contains_key("capture_rate"));
        assert_eq!(obj.get("capture_rate").and_then(|v| v.as_i64()), Some(190));
    }

    #[test]
    fn entity_row_schema_incomplete_detects_missing_fields() {
        let cgs = pokeapi_cgs();
        let entity = stub_species_entity();
        assert!(entity_row_schema_incomplete(
            &cgs,
            "PokemonSpecies",
            &entity
        ));
    }

    #[test]
    fn entity_row_schema_complete_when_all_fields_present() {
        let cgs = pokeapi_cgs();
        let mut entity = stub_species_entity();
        for field in cgs
            .get_entity("PokemonSpecies")
            .expect("species")
            .fields
            .keys()
        {
            if field.as_str() == "name" || field.as_str() == "url" {
                continue;
            }
            entity.fields.insert(
                field.as_str().to_string(),
                plasm_core::TypedFieldValue::from(plasm_core::Value::Integer(0)),
            );
        }
        assert!(!entity_row_schema_incomplete(
            &cgs,
            "PokemonSpecies",
            &entity
        ));
    }
}
