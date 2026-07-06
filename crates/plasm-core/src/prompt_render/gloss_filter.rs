//! Keep field gloss rows referenced by teaching exemplars (and linked value domains).

use std::collections::HashSet;

use crate::symbol_tuning::SymbolMap;

use super::gloss_dedup::FieldGlossMeaningAtom;
use super::tsv_emit::parse_trailing_projection_bracket;
use super::{EntityTeachingExprRow, TeachingFieldGloss};

/// Map wire labels and projection bracket entries to opaque teaching tokens (`p#` / `v#`).
pub(crate) fn expand_referenced_teaching_slot_tokens(
    referenced: &mut HashSet<String>,
    text: &str,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    entity: &str,
) {
    let Some(map) = map else {
        return;
    };
    for wire in crate::symbol_tuning::wire_slot_idents_in_teaching_fragment(text) {
        referenced.insert(wire.clone());
        let token =
            map.teaching_slot_token_for_entity_row_wire(catalog_entry_id, entity, wire.as_str());
        referenced.insert(token);
        if let Some(vs) =
            map.value_sym_for_teaching_gloss_key(catalog_entry_id, entity, wire.as_str())
        {
            referenced.insert(vs);
        }
        let p = map.ident_sym_entity_field_for(catalog_entry_id, entity, wire.as_str());
        if SymbolMap::is_opaque_p_sym(p.as_str()) {
            referenced.insert(p);
        }
    }
    if let Some(br) = parse_trailing_projection_bracket(text.trim()) {
        for sym in br
            .trim()
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .into_iter()
            .flat_map(|inner| inner.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        {
            referenced.insert(sym.clone());
            if SymbolMap::is_opaque_p_sym(sym.as_str()) || sym.starts_with('v') {
                continue;
            }
            let token =
                map.teaching_slot_token_for_entity_row_wire(catalog_entry_id, entity, sym.as_str());
            referenced.insert(token);
            let p = map.ident_sym_entity_field_for(catalog_entry_id, entity, sym.as_str());
            if SymbolMap::is_opaque_p_sym(p.as_str()) {
                referenced.insert(p);
            }
            if let Some(vs) =
                map.value_sym_for_teaching_gloss_key(catalog_entry_id, entity, sym.as_str())
            {
                referenced.insert(vs);
            }
        }
    }
}

/// Symbols referenced in teaching surface text: opaque `e#`/`m#`/`p#`/`r#`/`v#` plus wire slot names.
fn collect_teaching_slot_symbols(text: &str) -> HashSet<String> {
    let mut out = collect_opaque_domain_symbols(text);
    for w in crate::symbol_tuning::wire_slot_idents_in_teaching_fragment(text) {
        out.insert(w);
    }
    out
}

fn gloss_row_referenced_symbols(g: &TeachingFieldGloss) -> HashSet<String> {
    let mut out = collect_teaching_slot_symbols(&g.symbol);
    for atom in g.meaning.to_meaning_atoms(&g.description) {
        let frag = match atom {
            FieldGlossMeaningAtom::FieldType(s)
            | FieldGlossMeaningAtom::AllowedValues(s)
            | FieldGlossMeaningAtom::Description(s) => s,
        };
        out.extend(collect_teaching_slot_symbols(&frag));
    }
    out
}

/// Keep slot gloss rows referenced by teaching exemplars (and linked value domains).
pub(crate) fn filter_field_gloss_to_referenced_symbols(
    rows: &[TeachingFieldGloss],
    teaching_rows: &[EntityTeachingExprRow],
    entity_surface: &str,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    entity: &str,
) -> Vec<TeachingFieldGloss> {
    let mut referenced = collect_teaching_slot_symbols(entity_surface);
    for row in teaching_rows {
        let expr = &row.teaching_expr.expression;
        referenced.extend(collect_teaching_slot_symbols(expr));
        expand_referenced_teaching_slot_tokens(
            &mut referenced,
            expr,
            map,
            catalog_entry_id,
            entity,
        );
        expand_referenced_teaching_slot_tokens(
            &mut referenced,
            &row.teaching_expr.result_type,
            map,
            catalog_entry_id,
            entity,
        );
    }
    expand_referenced_teaching_slot_tokens(
        &mut referenced,
        entity_surface,
        map,
        catalog_entry_id,
        entity,
    );
    loop {
        let mut expanded = false;
        for g in rows {
            if !referenced.contains(g.symbol.as_str()) {
                continue;
            }
            for sym in gloss_row_referenced_symbols(g) {
                if referenced.insert(sym) {
                    expanded = true;
                }
            }
        }
        if !expanded {
            break;
        }
    }
    let mut kept: Vec<TeachingFieldGloss> = rows
        .iter()
        .filter(|g| {
            g.is_inline_union_summary
                || g.symbol.starts_with('v')
                || g.symbol.starts_with('r')
                || referenced.contains(g.symbol.as_str())
        })
        .cloned()
        .collect();
    if let Some(map) = map {
        for wire in referenced.iter() {
            if SymbolMap::is_opaque_p_sym(wire.as_str()) || wire.starts_with('v') {
                continue;
            }
            if kept.iter().any(|g| g.symbol == wire.as_str()) {
                continue;
            }
            let p = map.ident_sym_entity_field_for(catalog_entry_id, entity, wire.as_str());
            if !SymbolMap::is_opaque_p_sym(p.as_str()) || p == *wire {
                continue;
            }
            if let Some(src) = kept.iter().find(|g| g.symbol == p) {
                let mut alias = src.clone();
                alias.symbol = wire.clone();
                kept.push(alias);
            }
        }
    }
    kept
}

pub(crate) fn collect_opaque_domain_symbols(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if matches!(b, b'e' | b'm' | b'p' | b'r' | b'v') {
            let start = i;
            i += 1;
            if i < bytes.len() && bytes[i].is_ascii_digit() {
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if let Ok(s) = std::str::from_utf8(&bytes[start..i]) {
                    out.insert(s.to_string());
                }
                continue;
            }
        }
        i += 1;
    }
    out
}
