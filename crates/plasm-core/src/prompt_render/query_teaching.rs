//! Query-cap teaching exemplars (`Entity{p#=…}`, search filters, entity-ref placeholders).

use crate::schema::{EntityDef, InputFieldSchema};
use crate::symbol_tuning::SymbolMap;
use crate::{FieldType, CGS};

use super::symbol_tokens::{ent_sym, id_sym_cap, id_sym_entity};
use super::teaching_util::{
    TEACHING_ID_HOLE, TEACHING_PARAM_VALUE_PLACEHOLDER, TEACHING_SEARCH_QUERY_LITERAL,
    TEACHING_SELECT_MEMBER_LITERAL,
};

/// Compound `Entity(p#=<id>,…)` when the target has multiple `key_vars`.
///
/// Unary entity refs use [`unary_entity_id_teaching_expr_line`] / `<id>` like scalar identity GET teaching.
pub(crate) fn entity_ref_id_example(
    cgs: &CGS,
    catalog_entry_id: &str,
    target: &str,
    map: Option<&SymbolMap>,
) -> String {
    if !entity_ref_target_in_session(map, catalog_entry_id, target) {
        return TEACHING_ID_HOLE.to_string();
    }
    let target_sym = ent_sym(map, catalog_entry_id, target);
    let p = TEACHING_ID_HOLE;
    let Some(ent) = cgs.get_entity(target) else {
        return format!("{target_sym}({TEACHING_ID_HOLE})");
    };
    if ent.key_vars.len() > 1 {
        let parts: Vec<String> = ent
            .key_vars
            .iter()
            .map(|kv| {
                format!(
                    "{}={}",
                    id_sym_entity(map, catalog_entry_id, target, kv.as_str()),
                    p
                )
            })
            .collect();
        format!("{}({})", target_sym, parts.join(", "))
    } else {
        unary_entity_id_teaching_expr_line(&target_sym, ent, map, catalog_entry_id)
    }
}

fn entity_ref_target_in_session(
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    target: &str,
) -> bool {
    map.is_some_and(|m| {
        m.try_entity_teaching_term_for(catalog_entry_id, target)
            .is_some()
    })
}

pub(crate) fn unseeded_entity_ref_invocation_gloss(
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    let mut hints = Vec::new();
    for f in cap.input_fields() {
        let Ok(nv) = f.named_value(cgs) else {
            continue;
        };
        let FieldType::EntityRef { target, .. } = &nv.field_type else {
            continue;
        };
        if entity_ref_target_in_session(map, catalog_entry_id, target.as_str()) {
            continue;
        }
        let param = id_sym_cap(map, catalog_entry_id, cap, f.name.as_str());
        hints.push(format!(
            "{param} takes {} — discover/seed it first",
            target.as_str()
        ));
    }
    if hints.is_empty() {
        None
    } else {
        Some(format!("· {}", hints.join("; ")))
    }
}

/// One `p#=value` in `Entity{p#=,…}` — opaque param symbols on the LHS (not wire names).
fn query_param_slot_example(
    f: &InputFieldSchema,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> String {
    let n = id_sym_cap(map, catalog_entry_id, cap, f.name.as_str());
    let p = TEACHING_PARAM_VALUE_PLACEHOLDER;
    let Ok(nv) = f.named_value(cgs) else {
        return format!("{n}={p}");
    };
    if matches!(nv.field_type, FieldType::Array) {
        return format!("{n}={p}");
    }
    match &nv.field_type {
        FieldType::Integer | FieldType::Number | FieldType::Money | FieldType::Boolean => {
            format!("{n}={p}")
        }
        FieldType::String | FieldType::Blob | FieldType::Uuid | FieldType::DigitId => {
            format!("{n}={p}")
        }
        FieldType::Date => format!("{n}={p}"),
        // Query/search Select holes stay quoted `"<member>"` — not a first-member
        // exemplar (T_enum_query_hole). Meaning lists members; the quotes teach
        // the same literal law the compiler already demands.
        FieldType::Select => format!("{n}={TEACHING_SELECT_MEMBER_LITERAL}"),
        FieldType::MultiSelect => format!("{n}=[{TEACHING_SELECT_MEMBER_LITERAL}]"),
        FieldType::EntityRef { target, .. } => {
            format!(
                "{n}={}",
                entity_ref_id_example(cgs, catalog_entry_id, target, map)
            )
        }
        FieldType::Array => format!("{n}=[{p}]"),
        FieldType::Json => format!("{n}={p}"),
    }
}

/// One `p#=value` for a scope parameter (same as filter slots).
pub(crate) fn scope_param_slot(
    f: &InputFieldSchema,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> String {
    query_param_slot_example(f, cap, cgs, map, catalog_entry_id)
}

/// `Entity(k=v,…)` for multi-`key_vars` GET examples (validated like other teaching lines).
pub(crate) fn compound_get_expr_line(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    if ent.key_vars.len() <= 1 {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    let p = TEACHING_ID_HOLE;
    for kv in &ent.key_vars {
        let f = ent.fields.get(kv)?;
        let sym = id_sym_entity(map, catalog_entry_id, ent.name.as_str(), kv.as_str());
        let nv = f.named_value(cgs).ok()?;
        match &nv.field_type {
            FieldType::Integer
            | FieldType::Number
            | FieldType::Money
            | FieldType::Boolean
            | FieldType::String
            | FieldType::Uuid
            | FieldType::DigitId
            | FieldType::Date
            | FieldType::Select
            | FieldType::MultiSelect
            | FieldType::Array
            | FieldType::Json
            | FieldType::Blob => {
                parts.push(format!("{sym}={p}"));
            }
            FieldType::EntityRef { target, .. } => {
                parts.push(format!(
                    "{sym}={}",
                    entity_ref_id_example(cgs, catalog_entry_id, target, map)
                ));
            }
        }
    }
    Some(format!("{es}({})", parts.join(", ")))
}

/// True when a Get must be keyed by identity / scope — never bare `e#` or `e#.m#()`.
pub(crate) fn get_requires_identity_anchor(
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    _ent: &EntityDef,
) -> bool {
    cap.get_requires_identity_anchor(cgs)
}

/// Identity GET with explicit id wire: `e#{wire=<wire>}` (valid brace→Get sugar).
///
/// Callers must gate this on [`EntityDef::teaches_token_identity_braces`] — not
/// “no Query peer”. File-polarity unary Gets (email / path / note_id) stay
/// [`unary_entity_id_teaching_expr_line`].
pub(crate) fn keyed_identity_get_teaching_expr_line(
    es: &str,
    ent: &EntityDef,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    if ent.id_field.is_empty() {
        return None;
    }
    let wire = id_sym_entity(
        map,
        catalog_entry_id,
        ent.name.as_str(),
        ent.id_field.as_str(),
    );
    Some(format!("{es}{{{wire}={TEACHING_PARAM_VALUE_PLACEHOLDER}}}"))
}

/// Primary Get identity teaching: token-identity braces IFF
/// [`EntityDef::teaches_token_identity_braces`]. Otherwise `e#(<id>)`.
pub(crate) fn primary_get_identity_teaching_expr_line(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> String {
    if ent.teaches_token_identity_braces(cgs) {
        keyed_identity_get_teaching_expr_line(es, ent, map, catalog_entry_id)
            .unwrap_or_else(|| unary_entity_id_teaching_expr_line(es, ent, map, catalog_entry_id))
    } else {
        unary_entity_id_teaching_expr_line(es, ent, map, catalog_entry_id)
    }
}

/// Unary identity GET teaching: always `e#(<id>)` — never sample ids or bare `$`.
/// Token-identity Gets (Get-only + `implicit_request_identity`) use
/// [`keyed_identity_get_teaching_expr_line`] on the fetch head, not this hole.
pub(crate) fn unary_entity_id_teaching_expr_line(
    es: &str,
    _ent: &EntityDef,
    _map: Option<&SymbolMap>,
    _catalog_entry_id: &str,
) -> String {
    format!("{es}({TEACHING_ID_HOLE})")
}

/// All authored scope (required and optional) plus all selection parameters.
pub(crate) fn query_expr_maximal(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    let mut inner: Vec<String> = Vec::new();
    for sf in cap.scope_params() {
        inner.push(scope_param_slot(sf, cap, cgs, map, catalog_entry_id));
    }

    for f in cap.selection_params() {
        inner.push(query_param_slot_example(f, cap, cgs, map, catalog_entry_id));
    }

    if inner.is_empty() {
        return Some(es.to_string());
    }
    Some(format!("{es}{{{}}}", inner.join(", ")))
}

/// Filter predicates only (no scope) — one `Entity{p#=…}` line per query cap so teaching table shows **filter**
/// field symbols even when scope+filters are merged on the maximal line.
pub(crate) fn query_expr_filters_only(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    let mut inner: Vec<String> = Vec::new();
    for f in cap.selection_params() {
        inner.push(query_param_slot_example(f, cap, cgs, map, catalog_entry_id));
    }
    if inner.is_empty() {
        return None;
    }
    Some(format!("{es}{{{}}}", inner.join(", ")))
}

fn search_non_text_selection(
    cap: &crate::CapabilitySchema,
) -> impl Iterator<Item = &InputFieldSchema> {
    let text_name = cap
        .search_text_selection_param()
        .map(|f| f.name.as_str())
        .unwrap_or("");
    cap.selection_params()
        .iter()
        .filter(move |f| f.name != text_name)
}

/// Primary search exemplar: `e#~"<query>"` plus braces for **required** non-text selection
/// (e.g. AppWorld `access_token`). Bare tilde alone must not teach away required credentials.
pub(crate) fn search_expr_primary(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> String {
    let mut inner: Vec<String> = Vec::new();
    for f in search_non_text_selection(cap).filter(|f| f.required) {
        inner.push(query_param_slot_example(f, cap, cgs, map, catalog_entry_id));
    }
    if inner.is_empty() {
        format!("{es}~{TEACHING_SEARCH_QUERY_LITERAL}")
    } else {
        format!(
            "{es}~{TEACHING_SEARCH_QUERY_LITERAL}{{{}}}",
            inner.join(", ")
        )
    }
}

/// Optional-filter twin for `e#~"<query>"{p#=…}` — only when selection has optional non-text
/// slots beyond the free-text `~` hole (and beyond required credentials already on the primary).
pub(crate) fn search_expr_with_filters(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    let non_text: Vec<&InputFieldSchema> = search_non_text_selection(cap).collect();
    if !non_text.iter().any(|f| !f.required) {
        return None;
    }
    let mut inner: Vec<String> = Vec::new();
    for f in &non_text {
        inner.push(query_param_slot_example(f, cap, cgs, map, catalog_entry_id));
    }
    Some(format!(
        "{es}~{TEACHING_SEARCH_QUERY_LITERAL}{{{}}}",
        inner.join(", ")
    ))
}

/// Only scope predicates (for a distinct structural example when maximal adds filters).
pub(crate) fn query_expr_scope_only(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    if cap.scope_params().is_empty() {
        return None;
    }
    let mut inner: Vec<String> = Vec::new();
    for sf in cap.scope_params() {
        inner.push(scope_param_slot(sf, cap, cgs, map, catalog_entry_id));
    }
    Some(format!("{es}{{{}}}", inner.join(", ")))
}
