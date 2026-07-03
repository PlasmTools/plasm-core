//! Opaque-symbol gloss deduplication — shared by teaching-table synthesis (not MCP-specific).

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use crate::symbol_tuning::{IdentMetadata, SymbolMap};

/// Returns [`None`] when `sym` is a synonym for an earlier opaque symbol with the same `meaning`:
/// caller skips emitting a duplicate gloss row. Otherwise returns the canonical symbol for this meaning.
pub(crate) fn meaning_canonical_sym_for_emit(
    meaning: &str,
    sym: &str,
    meaning_to_canonical: &mut HashMap<String, String>,
    sym_alias: &mut HashMap<String, String>,
) -> Option<String> {
    match meaning_to_canonical.entry(meaning.to_string()) {
        Entry::Occupied(e) => {
            let canonical = e.get().clone();
            if canonical == sym {
                Some(canonical)
            } else {
                sym_alias.insert(sym.to_string(), canonical);
                None
            }
        }
        Entry::Vacant(v) => {
            v.insert(sym.to_string());
            Some(sym.to_string())
        }
    }
}

/// Stable gloss dedupe identity — cap params must not alias entity fields with the same compact meaning.
#[derive(Debug, Clone)]
pub(crate) enum GlossSlotIdentity {
    CapabilityParam {
        domain: String,
        cap: String,
        param: String,
    },
    EntityField {
        entity: String,
        wire: String,
    },
}

impl GlossSlotIdentity {
    fn disambiguator(&self) -> String {
        match self {
            Self::CapabilityParam { domain, cap, param } => {
                format!("cap:{domain}.{cap}.{param}")
            }
            Self::EntityField { entity, wire } => format!("field:{entity}.{wire}"),
        }
    }
}

pub(crate) fn gloss_slot_identity_for_p_sym(
    map: &SymbolMap,
    sym: &str,
    meta: &IdentMetadata,
) -> GlossSlotIdentity {
    map.capability_param_quad_for_p_sym(sym)
        .map(|(_, dom, cap, path)| GlossSlotIdentity::CapabilityParam {
            domain: dom.to_string(),
            cap: cap.to_string(),
            param: path,
        })
        .unwrap_or_else(|| GlossSlotIdentity::EntityField {
            entity: meta.entity().to_string(),
            wire: meta.wire_name().to_string(),
        })
}

pub(crate) fn gloss_p_slot_meaning_key(
    compact: &str,
    catalog_entry_id: &str,
    slot: &GlossSlotIdentity,
) -> String {
    format!(
        "{}\x1f{}\x1f{}",
        compact,
        catalog_entry_id,
        slot.disambiguator()
    )
}

pub(crate) fn merge_opaque_alias_maps(
    p: &HashMap<String, String>,
    v: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut rep = p.clone();
    for (k, val) in v {
        if let Some(existing) = rep.get(k) {
            debug_assert_eq!(
                existing, val,
                "opaque alias collision for key {k:?}: p-map vs v-map disagree"
            );
        }
        rep.insert(k.clone(), val.clone());
    }
    rep
}

/// `p#` / `r#` symbols already demonstrated on a teaching-row LHS (projection brackets or witness).
pub(crate) fn lhs_demonstrated_syms_for_teaching_expr(
    expr: &str,
    result_gloss: Option<&str>,
) -> std::collections::HashSet<String> {
    use crate::symbol_tuning::field_syms_for_teaching_row;

    field_syms_for_teaching_row(expr, result_gloss, None, &[])
        .into_iter()
        .filter(|s| {
            SymbolMap::is_opaque_p_sym(s.as_str()) || SymbolMap::is_opaque_r_sym(s.as_str())
        })
        .collect()
}
