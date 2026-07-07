//! Union constructor teaching gloss and exemplar rows.

use std::collections::{BTreeSet, HashMap};

use crate::symbol_tuning::SymbolMap;
use crate::CGS;

use super::super::gloss_collect::{
    commit_teaching_field_gloss_row, push_teaching_field_gloss_row, GlossScratch,
};
use super::super::gloss_dedup::{meaning_canonical_sym_for_emit, FieldGlossMeaning};
use super::super::line_validate::{DomainLineValidCacheKey, DomainLineValidEntry};
use super::super::teaching_legend::LEGEND_EM_DESC_SEP;
use super::super::teaching_push::try_push_teaching_example;
use super::super::teaching_util::{strip_union_constructor_authoring_noise, truncate_inline_desc};
use super::super::EntityTeachingExprRow;
use super::structural::{
    format_inline_structural_example_symbolic,
    format_inline_structural_example_symbolic_required_only,
};

pub(crate) fn union_variants_teachable(variants: &[crate::schema::InputVariantSchema]) -> bool {
    !variants.is_empty()
        && variants
            .iter()
            .all(|v| crate::schema::union_variant_constructor_symbol(v).is_some())
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
pub(crate) fn format_union_constructor_gloss_legend(
    v: &crate::schema::InputVariantSchema,
) -> String {
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
    if !union_variants_teachable(variants) {
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
                        gs.state,
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
        Some(gs.map),
        Some(gs.cgs),
        true,
        gs.state,
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
        if !union_variants_teachable(variants) {
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
        if !union_variants_teachable(variants) {
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
        if !union_variants_teachable(variants) {
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
        if !union_variants_teachable(variants) {
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
