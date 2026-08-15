//! Query-cap teaching exemplars (`Entity{p#=…}`, search filters, entity-ref placeholders).

use crate::schema::{EntityDef, InputFieldSchema};
use crate::symbol_tuning::SymbolMap;
use crate::{FieldType, InputType, ParameterRole, CGS};

use super::symbol_tokens::{ent_sym, id_sym_cap, id_sym_entity};
use super::teaching_util::TEACHING_PARAM_VALUE_PLACEHOLDER;

/// Compound `Entity(p#=$,…)` when the target has multiple `key_vars`.
///
/// Unary entity refs use [`unary_entity_id_teaching_expr_line`] / `$` fallback like scalar identity GET teaching.
pub(crate) fn entity_ref_id_example(
    cgs: &CGS,
    catalog_entry_id: &str,
    target: &str,
    map: Option<&SymbolMap>,
) -> String {
    if !entity_ref_target_in_session(map, catalog_entry_id, target) {
        return TEACHING_PARAM_VALUE_PLACEHOLDER.to_string();
    }
    let target_sym = ent_sym(map, catalog_entry_id, target);
    let p = TEACHING_PARAM_VALUE_PLACEHOLDER;
    let Some(ent) = cgs.get_entity(target) else {
        return format!("{target_sym}({})", TEACHING_PARAM_VALUE_PLACEHOLDER);
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
    for f in cap.object_params()? {
        let Ok(nv) = f.named_value(cgs) else {
            continue;
        };
        let FieldType::EntityRef { target } = &nv.field_type else {
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
        FieldType::String | FieldType::Blob | FieldType::Uuid => format!("{n}={p}"),
        FieldType::Date => format!("{n}={p}"),
        FieldType::Select | FieldType::MultiSelect => format!("{n}={p}"),
        FieldType::EntityRef { target } => {
            format!(
                "{n}={}",
                entity_ref_id_example(cgs, catalog_entry_id, target, map)
            )
        }
        FieldType::Array => format!("{n}=[{p}]"),
        FieldType::Json => format!("{n}={p}"),
    }
}

pub(crate) fn field_is_filter_like(f: &InputFieldSchema) -> bool {
    !matches!(
        f.role,
        Some(ParameterRole::Search)
            | Some(ParameterRole::Sort)
            | Some(ParameterRole::SortDirection)
            | Some(ParameterRole::ResponseControl)
    )
}

/// One `p#=value` for a **required scope** parameter (same as filter slots).
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
    let p = TEACHING_PARAM_VALUE_PLACEHOLDER;
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
            | FieldType::Date
            | FieldType::Select
            | FieldType::MultiSelect
            | FieldType::Array
            | FieldType::Json
            | FieldType::Blob => {
                parts.push(format!("{sym}={p}"));
            }
            FieldType::EntityRef { target } => {
                parts.push(format!(
                    "{sym}={}",
                    entity_ref_id_example(cgs, catalog_entry_id, target, map)
                ));
            }
        }
    }
    Some(format!("{es}({})", parts.join(", ")))
}

/// Unary identity GET teaching: positional literal for simple string ids (e.g. `e#(pikachu)` on
/// Pokemon), otherwise opaque **`p#`** (`e#(p…)`) when the field has an allocated teaching ident
/// symbol; otherwise **`e#($)`** (canonical / unresolved gloss).
pub(crate) fn unary_entity_id_teaching_expr_line(
    es: &str,
    ent: &EntityDef,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> String {
    if let Some(literal) = positional_identity_teaching_literal(ent) {
        return format!("{es}({literal})");
    }
    let sym = id_sym_entity(
        map,
        catalog_entry_id,
        ent.name.as_str(),
        ent.id_field.as_str(),
    );
    format!("{es}({sym})")
}

/// Literal positional identity for teaching rows (B2): simple string `id_field`, no compound keys.
fn positional_identity_teaching_literal(ent: &EntityDef) -> Option<&'static str> {
    if !ent.key_vars.is_empty() {
        return None;
    }
    match ent.id_format {
        Some(crate::schema::IdFormat::Uuid) | Some(crate::schema::IdFormat::Integer) => None,
        Some(crate::schema::IdFormat::Email) => Some("user@example.com"),
        Some(crate::schema::IdFormat::Other) => None,
        Some(crate::schema::IdFormat::Slug) | None => match ent.name.as_str() {
            "Pokemon" => Some("pikachu"),
            _ if ent.id_field.as_str() == "name" => Some("example-name"),
            _ => None,
        },
    }
}

/// Scope predicates + all filter-like parameters (required + optional) with CGS-derived placeholders.
pub(crate) fn query_expr_maximal(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    let Some(is) = &cap.input_schema else {
        return Some(es.to_string());
    };
    let InputType::Object { fields, .. } = &is.input_type else {
        return None;
    };
    let fields = fields.as_slice();

    let scope_fields: Vec<&InputFieldSchema> = fields
        .iter()
        .filter(|f| f.required && matches!(f.role, Some(ParameterRole::Scope)))
        .collect();

    let mut inner: Vec<String> = Vec::new();
    for sf in &scope_fields {
        inner.push(scope_param_slot(sf, cap, cgs, map, catalog_entry_id));
    }

    for f in fields {
        if matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
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
    let Some(is) = &cap.input_schema else {
        return None;
    };
    let InputType::Object { fields, .. } = &is.input_type else {
        return None;
    };
    let mut inner: Vec<String> = Vec::new();
    for f in fields {
        if matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        inner.push(query_param_slot_example(f, cap, cgs, map, catalog_entry_id));
    }
    if inner.is_empty() {
        return None;
    }
    Some(format!("{es}{{{}}}", inner.join(", ")))
}

/// Search filter slots for `e#~"text"{p#=…}` — same param selection as [`query_expr_filters_only`].
pub(crate) fn search_expr_with_filters(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    let Some(is) = &cap.input_schema else {
        return None;
    };
    let InputType::Object { fields, .. } = &is.input_type else {
        return None;
    };
    let mut inner: Vec<String> = Vec::new();
    for f in fields {
        if matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if matches!(f.role, Some(ParameterRole::Search)) {
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        inner.push(query_param_slot_example(f, cap, cgs, map, catalog_entry_id));
    }
    if inner.is_empty() {
        return None;
    }
    Some(format!("{es}~\"text\"{{{}}}", inner.join(", ")))
}

/// Only scope predicates (for a distinct structural example when maximal adds filters).
pub(crate) fn query_expr_scope_only(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    let Some(is) = &cap.input_schema else {
        return None;
    };
    let InputType::Object { fields, .. } = &is.input_type else {
        return None;
    };
    let scope_fields: Vec<&InputFieldSchema> = fields
        .iter()
        .filter(|f| f.required && matches!(f.role, Some(ParameterRole::Scope)))
        .collect();
    if scope_fields.is_empty() {
        return None;
    }
    let mut inner: Vec<String> = Vec::new();
    for sf in &scope_fields {
        inner.push(scope_param_slot(sf, cap, cgs, map, catalog_entry_id));
    }
    Some(format!("{es}{{{}}}", inner.join(", ")))
}
