//! Dotted-call invoke exemplars and union constructor teaching rows.

mod dotted_call;
mod structural;
mod union_ctor;

use std::collections::HashMap;

use crate::schema::capability_method_label_kebab;
use crate::symbol_tuning::{IdentMetaKey, IdentMetadata, SymbolMap};
use crate::CGS;

use super::query_teaching::unseeded_entity_ref_invocation_gloss;
use super::teaching_legend::LEGEND_EM_DESC_SEP;
use super::teaching_util::truncate_inline_desc;

pub(crate) use dotted_call::{build_standalone_create_paren_args, format_dotted_call_line};
pub(crate) use union_ctor::{
    emit_array_of_union_constructor_teaching_gloss, try_push_union_constructor_teaching_expr_rows,
};

#[inline]
pub(crate) fn path_vars_empty(cap: &crate::CapabilitySchema) -> bool {
    !cap.domain_exemplar_requires_entity_anchor()
}

/// Capability legend after result gloss in teaching rows: `[scope …]` / `optional params: …` only.
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
