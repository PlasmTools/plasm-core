//! Dotted-call invoke exemplar assembly (`e#(…).m#(…)`).

use std::collections::HashMap;

use crate::schema::EntityDef;
use crate::scope_entity_ref_infer::should_omit_invoke_teaching_arg;
use crate::symbol_tuning::SymbolMap;
use crate::{CapabilityKind, FieldType, InputType, ValueWireFormat, CGS};

use super::super::line_validate::{DomainLineValidCacheKey, DomainLineValidEntry};
use super::super::query_teaching::{entity_ref_id_example, scope_param_slot};
use super::super::relation_teaching::receiver_for_dotted_suffix;
use super::path_vars_empty;
use super::super::symbol_tokens::{id_sym_cap, met_sym};
use super::super::teaching_util::TEACHING_PARAM_VALUE_PLACEHOLDER;
use super::union_ctor::{
    format_root_union_constructor_invoke_example, format_union_constructor_invoke_example,
    union_variants_teachable,
};

/// One `key=value` for dotted-call `method(k=v,…)` — equality/entity forms parse as invoke args (not query `>=` predicates).
pub(crate) fn invoke_dotted_call_arg_example(
    f: &crate::InputFieldSchema,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    let n = id_sym_cap(map, catalog_entry_id, cap, f.name.as_str());
    let p = TEACHING_PARAM_VALUE_PLACEHOLDER;
    if let crate::InputFieldWire::Inline(ty) = &f.wire {
        return Some(match ty.as_ref() {
            crate::InputType::Array { element_type, .. } => {
                if let crate::InputType::Union { variants } = element_type.as_ref() {
                    if variants
                        .iter()
                        .all(|v| crate::schema::union_variant_constructor_symbol(v).is_some())
                    {
                        // Edit-v2-style ops use wire discriminator `op`; nested ctor RHS in dotted-call
                        // teaching lines type-check end-to-end. Proof `/ops` comment batches (and similar)
                        // discriminate with `type`; keep invoke exemplars placeholder-heavy — standalone
                        // union ctor rows still teach each `vNNN{…}` branch.
                        if variants.iter().all(|v| v.wire.field == "type") {
                            return Some(format!("{n}=[{p}]"));
                        }
                        let a = format_union_constructor_invoke_example(
                            variants.first()?,
                            cgs,
                            map,
                            catalog_entry_id,
                            cap.domain.as_str(),
                            cap.name.as_str(),
                            f.name.as_str(),
                        )?;
                        // Pair `replace_block` with `insert_after` so the teaching line shows both a
                        // flat `{markdown=$}` body and nested `[{markdown=$}]` blocks arrays.
                        let b = variants
                            .get(2)
                            .and_then(|vx| {
                                format_union_constructor_invoke_example(
                                    vx,
                                    cgs,
                                    map,
                                    catalog_entry_id,
                                    cap.domain.as_str(),
                                    cap.name.as_str(),
                                    f.name.as_str(),
                                )
                            })
                            .unwrap_or_else(|| a.clone());
                        return Some(format!("{n}=[{a},{b}]"));
                    }
                }
                format!("{n}=[{p}]")
            }
            _ => format!("{n}={p}"),
        });
    }
    let nv = match f.named_value(cgs) {
        Ok(nv) => nv,
        Err(_) => return Some(format!("{n}={p}")),
    };
    match &nv.field_type {
        FieldType::Boolean
        | FieldType::String
        | FieldType::Blob
        | FieldType::Json
        | FieldType::Uuid
        | FieldType::Integer
        | FieldType::Number
        | FieldType::Money => Some(format!("{n}={p}")),
        FieldType::Select | FieldType::MultiSelect => Some(format!("{n}={p}")),
        FieldType::EntityRef { target, .. } => Some(format!(
            "{n}={}",
            entity_ref_id_example(cgs, catalog_entry_id, target, map)
        )),
        FieldType::Date => match &nv.value_format {
            // Same placeholder as strings — avoid teaching ISO literals in teaching table dotted-call invokes.
            Some(ValueWireFormat::Temporal(_)) => Some(format!(
                "{n}={p}",
                n = n,
                p = TEACHING_PARAM_VALUE_PLACEHOLDER
            )),
            _ => None,
        },
        FieldType::Array => match f.resolved_array_items(cgs) {
            Some(_items) => Some(format!("{n}=[{p}]", n = n, p = p)),
            None => Some(format!(r#"{n}=[]"#)),
        },
    }
}

pub(crate) fn build_dotted_call_paren_args(
    anchor_entity: &str,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    let ent = cgs.get_entity(anchor_entity)?;
    if let Some(InputType::Union { variants }) = cap
        .invocation_input_schemas()
        .map(|schema| &schema.input_type)
        .find(|ty| matches!(ty, InputType::Union { .. }))
    {
        if !union_variants_teachable(variants) {
            return None;
        }
        let v = variants.first()?;
        return format_root_union_constructor_invoke_example(
            v,
            cgs,
            map,
            catalog_entry_id,
            cap.domain.as_str(),
            cap.name.as_str(),
        );
    }
    let mut parts: Vec<String> = Vec::new();
    let mut required_example_failed = false;
    for f in cap.scope_params() {
        if !f.required {
            continue;
        }
        if should_omit_invoke_teaching_arg(ent, cap, f, cgs) {
            continue;
        }
        parts.push(scope_param_slot(f, cap, cgs, map, catalog_entry_id));
    }
    let invoke_fields = cap.control_params().iter().chain(
        cap.invocation_input_schemas()
            .filter_map(|schema| match &schema.input_type {
                InputType::Object { fields, .. } => Some(fields.as_slice()),
                _ => None,
            })
            .flatten(),
    );
    for f in invoke_fields {
        if should_omit_invoke_teaching_arg(ent, cap, f, cgs) {
            continue;
        }
        match invoke_dotted_call_arg_example(f, cap, cgs, map, catalog_entry_id) {
            Some(a) => parts.push(a),
            None if f.required => required_example_failed = true,
            None => {}
        }
    }
    if required_example_failed {
        return None;
    }
    // Path-bound scope slots may be fully injected from a compound receiver (`Entity(k1=$,k2=$)`),
    // leaving only `method()` for zero-body deletes / similar invokes.
    if parts.is_empty() {
        return Some(String::new());
    }
    Some(parts.join(", "))
}

/// Parentheses for **standalone** `Entity.create(…)` when the capability has required parent-scope
/// parameters and therefore no anchor from which to inject them.
pub(crate) fn build_standalone_create_paren_args(
    ename: &str,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    if cap.kind != CapabilityKind::Create {
        return build_dotted_call_paren_args(ename, cap, cgs, map, catalog_entry_id);
    }
    let has_required_scope = cap.scope_params().iter().any(|f| f.required);
    if !has_required_scope {
        return build_dotted_call_paren_args(ename, cap, cgs, map, catalog_entry_id);
    }

    let ent = cgs.get_entity(ename)?;
    let mut parts: Vec<String> = Vec::new();
    let mut required_failed = false;
    for f in cap.scope_params() {
        if f.required {
            parts.push(scope_param_slot(f, cap, cgs, map, catalog_entry_id));
        }
    }
    let invoke_fields = cap.control_params().iter().chain(
        cap.invocation_input_schemas()
            .filter_map(|schema| match &schema.input_type {
                InputType::Object { fields, .. } => Some(fields.as_slice()),
                _ => None,
            })
            .flatten(),
    );
    for f in invoke_fields {
        if should_omit_invoke_teaching_arg(ent, cap, f, cgs) {
            continue;
        }
        match invoke_dotted_call_arg_example(f, cap, cgs, map, catalog_entry_id) {
            Some(a) => parts.push(a),
            None if f.required => required_failed = true,
            None => {}
        }
    }
    if required_failed {
        return None;
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(", "))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn format_dotted_call_line(
    anchor_entity: &str,
    cap: &crate::CapabilitySchema,
    ent: &EntityDef,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) -> Option<String> {
    let args = build_dotted_call_paren_args(anchor_entity, cap, cgs, map, catalog_entry_id)?;
    let ms = met_sym(map, catalog_entry_id, cap.domain.as_str(), cap);
    let suffix = format!(".{ms}({args})");
    let prefer_bare = path_vars_empty(cap);
    let recv = receiver_for_dotted_suffix(
        es,
        ent,
        cgs,
        map,
        catalog_entry_id,
        &suffix,
        line_valid_cache,
        line_valid_cache_seed,
        map_arc,
        prefer_bare,
    )?;
    Some(format!("{recv}{suffix}"))
}
