//! CGS-derived **evaluates-to** hints for teaching table `;;` comments (`=> [e#]` / `e#` / `=> ()`), not mixed into the expression.
//! Relation navigation lines use the same shape: `expr  ;;  => e#` or `=> [e#]` (see [`result_gloss_for_relation_nav`]).

use crate::schema::{CapabilityKind, CapabilitySchema, OutputType, CGS};

/// Canonical or symbolic entity name for gloss text (string is the serialization boundary).
pub fn entity_sym_for_gloss(map: Option<&crate::symbol_tuning::SymbolMap>, entity: &str) -> String {
    map.and_then(|m| m.try_entity_teaching_term(entity))
        .map(|t| t.to_string())
        .unwrap_or_else(|| entity.to_string())
}

/// Gloss string for a capability: collection `[e#]` / `[Team]`, single `e#` / `Team`, or `None` (omit).
pub fn result_gloss_for_capability(
    cap: &CapabilitySchema,
    _cgs: &CGS,
    map: Option<&crate::symbol_tuning::SymbolMap>,
) -> Option<String> {
    let template = &cap.mapping.template.0;
    if let Some(resp) = template.get("response") {
        if resp.get("items").is_some() {
            return Some(collection_gloss(cap.domain.as_str(), map));
        }
        if resp.get("single").is_some() {
            return Some(single_gloss(cap.domain.as_str(), map));
        }
    }

    if cap.is_read() {
        if matches!(
            cap.output_schema.as_ref().map(|output| &output.output_type),
            Some(OutputType::Collection { .. })
        ) {
            return Some(collection_gloss(cap.domain.as_str(), map));
        }
        if cap.kind == CapabilityKind::Action {
            return Some(single_gloss(cap.domain.as_str(), map));
        }
    }

    match cap.kind {
        CapabilityKind::Query | CapabilityKind::Search => {
            Some(collection_gloss(cap.domain.as_str(), map))
        }
        CapabilityKind::Get => Some(single_gloss(cap.domain.as_str(), map)),
        CapabilityKind::Create
        | CapabilityKind::Update
        | CapabilityKind::Delete
        | CapabilityKind::Action => {
            // Writes without `response` in CML still often return an entity slice; `provides` marks that.
            if !cap.provides.is_empty() {
                Some(single_gloss(cap.domain.as_str(), map))
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
    map: Option<&crate::symbol_tuning::SymbolMap>,
) -> String {
    single_gloss(entity, map)
}

/// Relation or entity-ref navigation: same `=>` gloss as query (collection) vs get (single).
pub fn result_gloss_for_relation_nav(
    target_entity: &str,
    map: Option<&crate::symbol_tuning::SymbolMap>,
    cardinality_many: bool,
) -> String {
    if cardinality_many {
        collection_gloss(target_entity, map)
    } else {
        single_gloss(target_entity, map)
    }
}

/// Get with field projection (e.g. `e4(42)[p1,p37]` => `[p1,p37]` — shape of the projected record).
pub fn result_gloss_for_get_projection(field_syms: &[String]) -> String {
    format!("[{}]", field_syms.join(","))
}

/// Search / ranked list of entities — same collection gloss as query.
pub fn result_gloss_for_search_entity(
    entity: &str,
    map: Option<&crate::symbol_tuning::SymbolMap>,
) -> String {
    collection_gloss(entity, map)
}

fn collection_gloss(entity: &str, map: Option<&crate::symbol_tuning::SymbolMap>) -> String {
    let s = entity_sym_for_gloss(map, entity);
    format!("[{s}]")
}

fn single_gloss(entity: &str, map: Option<&crate::symbol_tuning::SymbolMap>) -> String {
    entity_sym_for_gloss(map, entity)
}
