//! CGS-derived **evaluates-to** hints for teaching-table Meaning (`↠ e#` / `→ e#` / `↣ [e#]` / `()`).
//!
//! Return glosses are **opaque session symbols only** (`e#` / `[e#]`). Catalog wire / entity
//! names belong on params and field keys — never as method/query return atoms (federated
//! homographs like `AuthSession` must not reappear as unqualified roots).

use crate::schema::{CapabilityKind, CapabilitySchema, CGS};
use crate::symbol_tuning::SymbolMap;

/// Opaque `e#` for `(catalog_entry_id, entity)` when that pair is in the session map.
///
/// Returns `None` when there is no map, or the pair is not exposed — callers omit the return
/// atom rather than falling back to a bare entity wire name.
pub fn entity_sym_for_gloss(
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    entity: &str,
) -> Option<String> {
    let m = map?;
    m.try_entity_teaching_term_for(catalog_entry_id, entity)
        .or_else(|| {
            // Unset single-graph maps often key as `""`; unique unqualified match is ok.
            if catalog_entry_id.is_empty() {
                m.try_entity_teaching_term(entity)
            } else {
                None
            }
        })
        .map(|t| t.to_string())
}

/// Gloss string for a capability: collection `[e#]`, single `e#`, unit `()`, or `None` (omit).
pub fn result_gloss_for_capability(
    cap: &CapabilitySchema,
    _cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    let domain = cap.domain.as_str();
    let template = &cap.mapping.template.0;
    if let Some(resp) = template.get("response") {
        if resp.get("items").is_some() {
            return collection_gloss(map, catalog_entry_id, domain);
        }
        if resp.get("single").is_some() {
            return single_gloss(map, catalog_entry_id, domain);
        }
    }

    match cap.kind {
        CapabilityKind::Query | CapabilityKind::Search => {
            collection_gloss(map, catalog_entry_id, domain)
        }
        CapabilityKind::Get => single_gloss(map, catalog_entry_id, domain),
        CapabilityKind::Create
        | CapabilityKind::Update
        | CapabilityKind::Delete
        | CapabilityKind::Action => {
            // Writes without `response` in CML still often return an entity slice; `provides` marks that.
            if !cap.provides.is_empty() {
                single_gloss(map, catalog_entry_id, domain)
            } else {
                // CGS: every capability has a type; void / side-effect with no entity payload uses unit `()`.
                Some("()".to_string())
            }
        }
    }
}

/// Single-resource get result (e.g. `Team(42)` => `e1`).
pub fn result_gloss_for_get_entity(
    entity: &str,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    single_gloss(map, catalog_entry_id, entity)
}

/// Relation or entity-ref navigation: same gloss as query (collection) vs get (single).
pub fn result_gloss_for_relation_nav(
    target_entity: &str,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    cardinality_many: bool,
) -> Option<String> {
    if cardinality_many {
        collection_gloss(map, catalog_entry_id, target_entity)
    } else {
        single_gloss(map, catalog_entry_id, target_entity)
    }
}

/// Get with field projection (e.g. `e4(42)[p1,p37]` => `[p1,p37]` — shape of the projected record).
pub fn result_gloss_for_get_projection(field_syms: &[String]) -> String {
    format!("[{}]", field_syms.join(","))
}

/// Search / ranked list of entities — same collection gloss as query.
pub fn result_gloss_for_search_entity(
    entity: &str,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    collection_gloss(map, catalog_entry_id, entity)
}

fn collection_gloss(
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    entity: &str,
) -> Option<String> {
    entity_sym_for_gloss(map, catalog_entry_id, entity).map(|s| format!("[{s}]"))
}

fn single_gloss(map: Option<&SymbolMap>, catalog_entry_id: &str, entity: &str) -> Option<String> {
    entity_sym_for_gloss(map, catalog_entry_id, entity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use crate::symbol_tuning::TeachingExposureSession;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn fixtures_schemas_dir(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas")
            .join(name)
    }

    #[test]
    fn entity_sym_for_gloss_never_returns_bare_wire() {
        assert_eq!(
            entity_sym_for_gloss(None, "file_system", "AuthSession"),
            None
        );
    }

    #[test]
    fn federated_homograph_gloss_resolves_qualified_e_only() {
        let root = fixtures_schemas_dir("plasm_language_matrix");
        let cgs = load_schema_dir(&root).expect("plasm_language_matrix");
        let layers = [&cgs, &cgs];
        let mut exp = TeachingExposureSession::new(&cgs, "venmo", &["LangItem"]);
        exp.expose_entities(&layers, Arc::new(cgs.clone()), "splitwise", &["LangItem"]);
        let map = exp.symbol_map_arc();

        // Unqualified lookup is ambiguous → None (never "LangItem").
        assert_eq!(map.try_entity_teaching_term("LangItem"), None);
        assert_eq!(
            entity_sym_for_gloss(Some(map.as_ref()), "", "LangItem"),
            None
        );

        let e_venmo = entity_sym_for_gloss(Some(map.as_ref()), "venmo", "LangItem")
            .expect("venmo LangItem e#");
        let e_split = entity_sym_for_gloss(Some(map.as_ref()), "splitwise", "LangItem")
            .expect("splitwise LangItem e#");
        assert!(e_venmo.starts_with('e'), "{e_venmo}");
        assert!(e_split.starts_with('e'), "{e_split}");
        assert_ne!(e_venmo, e_split);
        assert_eq!(
            result_gloss_for_get_entity("LangItem", Some(map.as_ref()), "venmo").as_deref(),
            Some(e_venmo.as_str())
        );
    }
}
