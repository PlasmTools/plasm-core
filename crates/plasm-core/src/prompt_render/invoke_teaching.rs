//! Dotted-call invoke exemplars and union constructor teaching rows.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::schema::{capability_method_label_kebab, EntityDef};
use crate::symbol_tuning::{CapParamTeachingSurface, IdentMetaKey, IdentMetadata, SymbolMap};
use crate::{CapabilityKind, FieldType, InputType, ParameterRole, ValueWireFormat, CGS};

use super::gloss_collect::{commit_teaching_field_gloss_row, push_teaching_field_gloss_row, GlossScratch};
use super::gloss_dedup::{meaning_canonical_sym_for_emit, FieldGlossMeaning};
use super::line_validate::{DomainLineValidCacheKey, DomainLineValidEntry};
use super::query_teaching::{entity_ref_id_example, field_is_filter_like, scope_param_slot, unseeded_entity_ref_invocation_gloss};
use super::relation_teaching::receiver_for_dotted_suffix;
use super::symbol_tokens::{id_sym_cap, met_sym};
use super::teaching_legend::LEGEND_EM_DESC_SEP;
use super::teaching_push::try_push_teaching_example;
use super::teaching_util::{strip_union_constructor_authoring_noise, truncate_inline_desc, TEACHING_PARAM_VALUE_PLACEHOLDER};
use super::EntityTeachingExprRow;

#[inline]
pub(crate) fn path_vars_empty(cap: &crate::CapabilitySchema) -> bool {
    !cap.domain_exemplar_requires_entity_anchor()
}

/// Omit path-bound scope keys from explicit dotted-call `(…)` when they are already supplied by the
/// receiver: unary `Entity($)` / symbolic unary `e#(p#)` identity injects `{entity}_id`, and compound
/// `Entity(k1=$, k2=$)` injects each `key_vars` slot that also appears as a path template variable.
pub(crate) fn field_omitted_from_path_inject(
    ent: &EntityDef,
    cap: &crate::CapabilitySchema,
    field_name: &str,
) -> bool {
    let path_vars = crate::schema::path_var_names_from_mapping_json(&cap.mapping.template.0);
    if !path_vars.iter().any(|pv| pv == field_name) {
        return false;
    }
    let unary_anchor_id = format!("{}_id", ent.name.to_lowercase());
    if field_name == unary_anchor_id {
        return true;
    }
    // Compound receiver `Entity(k1=$,…)` may inject path vars that duplicate explicit scope args,
    // but only when every identity key that appears on this capability's HTTP path is also a
    // declared required scope parameter (some APIs bind extra path segments purely from row keys).
    if ent.key_vars.len() > 1 {
        if let Some(is) = cap.input_schema.as_ref() {
            if let InputType::Object { fields, .. } = &is.input_type {
                let required_scope: HashSet<&str> = fields
                    .iter()
                    .filter(|f| f.required && matches!(f.role, Some(ParameterRole::Scope)))
                    .map(|f| f.name.as_str())
                    .collect();
                let path_set: HashSet<&str> = path_vars.iter().map(|s| s.as_str()).collect();
                let every_path_bound_key_declared = ent.key_vars.iter().all(|kv| {
                    let k = kv.as_str();
                    !path_set.contains(k) || required_scope.contains(k)
                });
                if every_path_bound_key_declared
                    && ent.key_vars.iter().any(|kv| kv.as_str() == field_name)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Capability legend after result gloss in teaching rows: `[scope …]` / `optional params: …` only.
/// Required invoke parameters are implicit from the taught expression; standalone `p#` gloss rows
/// carry wire names and types.
pub(crate) fn format_capability_legend_line(
    map: &SymbolMap,
    cgs: &CGS,
    cap: &crate::CapabilitySchema,
    _anchor_entity: &str,
    _ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    _catalog_entry_id: &str,
) -> String {
    const MAX_DESC: usize = 80;
    let kebab = capability_method_label_kebab(cap);
    let raw = cap.description.as_str().trim();
    let gloss = if raw.is_empty() {
        kebab
    } else {
        truncate_inline_desc(raw, MAX_DESC)
    };
    let sig = map.capability_input_signature_gloss(cgs, cap);
    if sig.is_empty() {
        gloss
    } else if gloss.is_empty() {
        sig
    } else {
        format!("{sig}{LEGEND_EM_DESC_SEP}{gloss}")
    }
}

#[inline]
pub(crate) fn capability_legend_for_domain(
    map: Option<&SymbolMap>,
    cgs: &CGS,
    cap: &crate::CapabilitySchema,
    anchor_entity: &str,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    catalog_entry_id: &str,
) -> Option<String> {
    map.map(|m| {
        format_capability_legend_line(m, cgs, cap, anchor_entity, ident_meta, catalog_entry_id)
    })
}

#[inline]
pub(crate) fn capability_legend_with_session_gloss(
    map: Option<&SymbolMap>,
    cgs: &CGS,
    cap: &crate::CapabilitySchema,
    anchor_entity: &str,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    catalog_entry_id: &str,
) -> Option<String> {
    let mut leg =
        capability_legend_for_domain(map, cgs, cap, anchor_entity, ident_meta, catalog_entry_id)?;
    if let Some(hint) = unseeded_entity_ref_invocation_gloss(cap, cgs, map, catalog_entry_id) {
        if !leg.is_empty() {
            leg.push(' ');
        }
        leg.push_str(&hint);
    }
    Some(leg)
}

/// Structural invoke RHS inside union constructors (`v101{…}`): keyed by opaque `p#` when a
/// [`SymbolMap`] is present (teaching TSV); canonical [`RenderMode`] uses wire names.
pub(crate) fn format_inline_structural_example_symbolic(
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    domain: &str,
    cap_name: &str,
    path_prefix: &str,
    ty: &crate::InputType,
    _cgs: &CGS,
) -> String {
    match ty {
        crate::InputType::None | crate::InputType::Value { .. } => {
            TEACHING_PARAM_VALUE_PLACEHOLDER.to_string()
        }
        crate::InputType::Object { fields, .. } => {
            let mut parts = Vec::new();
            for sf in fields {
                let seg = if path_prefix.is_empty() {
                    sf.name.clone()
                } else {
                    format!("{path_prefix}.{}", sf.name)
                };
                match &sf.wire {
                    crate::InputFieldWire::Inline(inner) => {
                        let rhs = format_inline_structural_example_symbolic(
                            map,
                            catalog_entry_id,
                            domain,
                            cap_name,
                            &seg,
                            inner.as_ref(),
                            _cgs,
                        );
                        let lhs = map
                            .map(|m| {
                                m.ident_sym_cap_param_for(
                                    catalog_entry_id,
                                    domain,
                                    cap_name,
                                    seg.as_str(),
                                )
                            })
                            .unwrap_or_else(|| sf.name.clone());
                        parts.push(format!("{lhs}={rhs}"));
                    }
                    crate::InputFieldWire::Registry(_) => {
                        let lhs = map
                            .map(|m| {
                                m.ident_sym_cap_param_for(
                                    catalog_entry_id,
                                    domain,
                                    cap_name,
                                    seg.as_str(),
                                )
                            })
                            .unwrap_or_else(|| sf.name.clone());
                        parts.push(format!("{lhs}={}", TEACHING_PARAM_VALUE_PLACEHOLDER));
                    }
                }
            }
            format!("{{{}}}", parts.join(","))
        }
        crate::InputType::Array { element_type, .. } => {
            format!(
                "[{}]",
                format_inline_structural_example_symbolic(
                    map,
                    catalog_entry_id,
                    domain,
                    cap_name,
                    path_prefix,
                    element_type.as_ref(),
                    _cgs,
                )
            )
        }
        crate::InputType::Union { .. } => TEACHING_PARAM_VALUE_PLACEHOLDER.to_string(),
    }
}

/// Like [`format_inline_structural_example_symbolic`] for an object body, but **only required** fields
/// and **no** `,..` optional tail — union constructor payloads inside `{…}` must parse as plain `k=v` pairs.
pub(crate) fn format_inline_structural_example_symbolic_required_only(
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    domain: &str,
    cap_name: &str,
    path_prefix: &str,
    ty: &crate::InputType,
    cgs: &CGS,
) -> String {
    let crate::InputType::Object { fields, .. } = ty else {
        return format_inline_structural_example_symbolic(
            map,
            catalog_entry_id,
            domain,
            cap_name,
            path_prefix,
            ty,
            cgs,
        );
    };
    let mut parts = Vec::new();
    for sf in fields {
        if !sf.required {
            continue;
        }
        let seg = if path_prefix.is_empty() {
            sf.name.clone()
        } else {
            format!("{path_prefix}.{}", sf.name)
        };
        match &sf.wire {
            crate::InputFieldWire::Inline(inner) => {
                let rhs = format_inline_structural_example_symbolic_required_only(
                    map,
                    catalog_entry_id,
                    domain,
                    cap_name,
                    &seg,
                    inner.as_ref(),
                    cgs,
                );
                let lhs = map
                    .map(|m| {
                        m.ident_sym_cap_param_for(catalog_entry_id, domain, cap_name, seg.as_str())
                    })
                    .unwrap_or_else(|| sf.name.clone());
                parts.push(format!("{lhs}={rhs}"));
            }
            crate::InputFieldWire::Registry(_) => {
                let lhs = map
                    .map(|m| {
                        m.ident_sym_cap_param_for(catalog_entry_id, domain, cap_name, seg.as_str())
                    })
                    .unwrap_or_else(|| sf.name.clone());
                parts.push(format!("{lhs}={}", TEACHING_PARAM_VALUE_PLACEHOLDER));
            }
        }
    }
    let inner = parts.join(",");
    format!("{{{inner}}}")
}

pub(crate) fn format_union_constructor_invoke_example(
    variant: &crate::schema::InputVariantSchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    domain: &str,
    cap_name: &str,
    operations_field: &str,
) -> Option<String> {
    let ctor = crate::schema::union_variant_constructor_symbol(variant)?;
    let body_ty = crate::schema::input_variant_body_type(variant);
    let prefix = format!("{}.{}", operations_field, variant.name);
    Some(format!(
        "{}{}",
        ctor,
        format_inline_structural_example_symbolic(
            map,
            catalog_entry_id,
            domain,
            cap_name,
            &prefix,
            &body_ty,
            cgs,
        )
    ))
}

/// Root-level invoke union (`input_schema.type: union`): ctor body uses flat param paths (`p5`, …).
pub(crate) fn format_root_union_constructor_invoke_example(
    variant: &crate::schema::InputVariantSchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    domain: &str,
    cap_name: &str,
) -> Option<String> {
    let ctor = crate::schema::union_variant_constructor_symbol(variant)?;
    let body_ty = crate::schema::input_variant_body_type(variant);
    Some(format!(
        "{}{}",
        ctor,
        format_inline_structural_example_symbolic_required_only(
            map,
            catalog_entry_id,
            domain,
            cap_name,
            "",
            &body_ty,
            cgs
        )
    ))
}

/// `v101`-row **Meaning** column: variant discriminator name + prose (not the symbolic ctor shape).
pub(crate) fn format_union_constructor_gloss_legend(v: &crate::schema::InputVariantSchema) -> String {
    const MAX_DESC: usize = 120;
    let disc = v.name.as_str();
    let raw =
        strip_union_constructor_authoring_noise(v.description.as_deref().unwrap_or("").trim());
    if raw.is_empty() {
        return disc.to_string();
    }
    format!(
        "{disc}{LEGEND_EM_DESC_SEP}{}",
        truncate_inline_desc(&raw, MAX_DESC)
    )
}

pub(crate) fn emit_union_array_constructor_teaching_gloss(
    gs: &mut GlossScratch<'_>,
    union_ty: &crate::InputType,
) {
    let crate::InputType::Union { variants } = union_ty else {
        return;
    };
    if variants.is_empty()
        || variants
            .iter()
            .any(|v| crate::schema::union_variant_constructor_symbol(v).is_none())
    {
        return;
    }
    let mut keys = BTreeSet::new();
    crate::schema::collect_registry_keys_from_input_type(union_ty, &mut keys);
    let cid = gs.catalog_entry_id;
    for key in keys {
        let fp = format!("{}|vr:{}", cid, key.as_str());
        if let Some(vsym) = gs.map.value_domain_fp_to_sym().get(&fp) {
            let vs = vsym.as_wire();
            if let Some(vg) = gs.map.value_domain_gloss_for_v_sym(&vs) {
                let Some(v_canon) = meaning_canonical_sym_for_emit(
                    vg,
                    &vs,
                    &mut gs.state.registry_value_gloss_canonical_v,
                    &mut gs.state.registry_v_sym_alias,
                ) else {
                    continue;
                };
                if gs.state.defined_value_domains.insert(v_canon.clone()) {
                    push_teaching_field_gloss_row(
                        gs.field_gloss,
                        v_canon,
                        vg,
                        gs.entity,
                        cid,
                        Some(gs.map),
                        Some(gs.meta),
                        Some(gs.cgs),
                        false,
                        &mut gs.state,
                    );
                }
            }
        }
    }
    let alts: Vec<&str> = variants
        .iter()
        .filter_map(crate::schema::union_variant_constructor_symbol)
        .collect();
    let union_summary = format!("union · {}", alts.join(" | "));
    let summary_sym = crate::symbol_tuning::next_opaque_v_symbol_after_map_and_extra_syms(
        gs.map,
        gs.field_gloss.iter().map(|g| g.symbol.as_str()),
    );
    commit_teaching_field_gloss_row(
        gs.field_gloss,
        summary_sym,
        FieldGlossMeaning::InlineUnionSummary {
            summary: union_summary.clone(),
        },
        None,
        gs.entity,
        cid,
        None,
        Some(gs.map),
        Some(gs.cgs),
        true,
        &mut gs.state,
    );
}

pub(crate) fn emit_array_of_union_constructor_teaching_gloss(
    gs: &mut GlossScratch<'_>,
    cap: &crate::CapabilitySchema,
) {
    let Some(is) = cap.input_schema.as_ref() else {
        return;
    };
    if let crate::InputType::Union { variants } = &is.input_type {
        if variants.is_empty()
            || variants
                .iter()
                .any(|v| crate::schema::union_variant_constructor_symbol(v).is_none())
        {
            return;
        }
        emit_union_array_constructor_teaching_gloss(gs, &is.input_type);
        return;
    }
    let crate::InputType::Object { fields, .. } = &is.input_type else {
        return;
    };
    for field in fields {
        let crate::InputFieldWire::Inline(ty) = &field.wire else {
            continue;
        };
        let crate::InputType::Array { element_type, .. } = ty.as_ref() else {
            continue;
        };
        let el = element_type.as_ref();
        let crate::InputType::Union { variants } = el else {
            continue;
        };
        if variants.is_empty()
            || variants
                .iter()
                .any(|v| crate::schema::union_variant_constructor_symbol(v).is_none())
        {
            continue;
        }
        emit_union_array_constructor_teaching_gloss(gs, el);
        return;
    }
}

/// One validated teaching row per union variant constructor (`v101{p#=$,…}`) before the dotted-call assembly line.
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_push_union_constructor_teaching_expr_rows(
    gloss_emit: &mut Option<GlossScratch<'_>>,
    teaching_rows: &mut Vec<EntityTeachingExprRow>,
    collect_meta: bool,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    cap: &crate::CapabilitySchema,
    catalog_entry_id: &str,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) {
    let Some(is) = cap.input_schema.as_ref() else {
        return;
    };
    if let crate::InputType::Union { variants } = &is.input_type {
        if variants.is_empty()
            || variants
                .iter()
                .any(|v| crate::schema::union_variant_constructor_symbol(v).is_none())
        {
            return;
        }
        for v in variants {
            let Some(expr_line) = format_root_union_constructor_invoke_example(
                v,
                cgs,
                map,
                catalog_entry_id,
                cap.domain.as_str(),
                cap.name.as_str(),
            ) else {
                continue;
            };
            let legend = format_union_constructor_gloss_legend(v);
            let _ = try_push_teaching_example(
                gloss_emit,
                teaching_rows,
                collect_meta,
                cgs,
                &expr_line,
                Some(legend),
                None,
                None,
                Some(&cap.name),
                false,
                line_valid_cache,
                line_valid_cache_seed,
                map_arc,
                None,
            );
        }
        return;
    }
    let crate::InputType::Object { fields, .. } = &is.input_type else {
        return;
    };
    for field in fields {
        let crate::InputFieldWire::Inline(ty) = &field.wire else {
            continue;
        };
        let crate::InputType::Array { element_type, .. } = ty.as_ref() else {
            continue;
        };
        let el = element_type.as_ref();
        let crate::InputType::Union { variants } = el else {
            continue;
        };
        if variants.is_empty()
            || variants
                .iter()
                .any(|v| crate::schema::union_variant_constructor_symbol(v).is_none())
        {
            return;
        }
        for v in variants {
            let Some(expr_line) = format_union_constructor_invoke_example(
                v,
                cgs,
                map,
                catalog_entry_id,
                cap.domain.as_str(),
                cap.name.as_str(),
                field.name.as_str(),
            ) else {
                continue;
            };
            let legend = format_union_constructor_gloss_legend(v);
            let _ = try_push_teaching_example(
                gloss_emit,
                teaching_rows,
                collect_meta,
                cgs,
                &expr_line,
                Some(legend),
                None,
                None,
                Some(&cap.name),
                false,
                line_valid_cache,
                line_valid_cache_seed,
                map_arc,
                None,
            );
        }
        return;
    }
}

/// One `key=value` for dotted-call `method(k=v,…)` — equality/entity forms parse as invoke args (not query `>=` predicates).
pub(crate) fn invoke_dotted_call_arg_example(
    f: &crate::InputFieldSchema,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    let n = id_sym_cap(
        map,
        catalog_entry_id,
        cap,
        f.name.as_str(),
        CapParamTeachingSurface::InvokeArg,
    );
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
        | FieldType::Number => Some(format!("{n}={p}")),
        FieldType::Select | FieldType::MultiSelect => Some(format!("{n}={p}")),
        FieldType::EntityRef { target } => Some(format!(
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
    let is = cap.input_schema.as_ref()?;
    if let InputType::Union { variants } = &is.input_type {
        if variants.is_empty()
            || variants
                .iter()
                .any(|v| crate::schema::union_variant_constructor_symbol(v).is_none())
        {
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
    let InputType::Object { fields, .. } = &is.input_type else {
        return None;
    };
    let mut parts: Vec<String> = Vec::new();
    let mut required_example_failed = false;
    for f in fields {
        if !f.required || !matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if field_omitted_from_path_inject(ent, cap, f.name.as_str()) {
            continue;
        }
        parts.push(scope_param_slot(f, cap, cgs, map, catalog_entry_id));
    }
    for f in fields {
        if matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        if field_omitted_from_path_inject(ent, cap, f.name.as_str()) {
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

/// Parentheses for **standalone** `Entity.create(…)` when the capability has required `role: scope`
/// parameters (no anchor to inject them). [`build_dotted_call_paren_args`] skips scope fields;
/// without scope slots, lines like `Comment.create(text=…)` fail validation for nested REST creates.
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
    let is = cap.input_schema.as_ref()?;
    let InputType::Object { fields, .. } = &is.input_type else {
        return None;
    };
    let has_required_scope = fields
        .iter()
        .any(|f| f.required && matches!(f.role, Some(ParameterRole::Scope)));
    if !has_required_scope {
        return build_dotted_call_paren_args(ename, cap, cgs, map, catalog_entry_id);
    }

    let ent = cgs.get_entity(ename)?;
    let mut parts: Vec<String> = Vec::new();
    let mut required_failed = false;
    for f in fields {
        if matches!(f.role, Some(ParameterRole::Scope)) {
            if f.required {
                parts.push(scope_param_slot(f, cap, cgs, map, catalog_entry_id));
            }
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        if field_omitted_from_path_inject(ent, cap, f.name.as_str()) {
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
    )?;
    Some(format!("{recv}{suffix}"))
}
