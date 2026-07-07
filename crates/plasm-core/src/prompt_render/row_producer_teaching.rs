//! Row-producer teaching line enrichment (projection brackets).

use crate::schema::CGS;
use crate::symbol_tuning::{ExposureSurface, SymbolMap};

use super::gloss_collect::GlossScratch;
use super::input_legend::{RowContractLegend, RowProjectionContract};
use super::line_validate::{DomainLineValidCacheKey, DomainLineValidEntry};
use super::row_producer::RowProducerProjection;
use super::surface_filter::surface_allows_entity_field;
use super::symbol_tokens::id_sym_entity;
use super::teaching_push::try_push_teaching_example;
use super::EntityTeachingExprRow;
use crate::schema::RelationSchema;
use std::collections::HashMap;

/// Parse `[p#,…]` into ordered symbols (empty when not a bracket).
pub(crate) fn projection_bracket_syms(bracket: &str) -> Vec<String> {
    bracket
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Order-independent equality of projection field sets (Get vs Query may differ only in order).
pub(crate) fn projection_field_sets_equal(a: &[String], b: &[String]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut aa = a.to_vec();
    let mut bb = b.to_vec();
    aa.sort();
    bb.sort();
    aa == bb
}

/// When the entity projection witness already taught `canonical_bracket`, omit the same
/// `[p#,…]` / `rows:` contract on row-producer lines (parser treats projection as optional).
#[allow(clippy::too_many_arguments)]
pub(crate) fn enrich_row_producer_teaching_line(
    cgs: &CGS,
    cap: &crate::CapabilitySchema,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    ename: &str,
    surface_filter: Option<&ExposureSurface>,
    base_expr: &str,
    base_gloss: Option<String>,
    projection: RowProducerProjection,
    canonical_bracket: Option<&str>,
    witness_taught: bool,
) -> (String, Option<String>, RowContractLegend) {
    let bracket = match projection {
        RowProducerProjection::BareQueryListAll => None,
        RowProducerProjection::CapabilityProvides => {
            let b = capability_row_projection_bracket(
                cgs,
                cap,
                map,
                catalog_entry_id,
                ename,
                surface_filter,
            );
            if witness_taught {
                if let (Some(br), Some(canon)) = (b.as_deref(), canonical_bracket) {
                    let br_syms = projection_bracket_syms(br);
                    let canon_syms = projection_bracket_syms(canon);
                    if projection_field_sets_equal(&br_syms, &canon_syms) {
                        None
                    } else {
                        b
                    }
                } else {
                    b
                }
            } else {
                b
            }
        }
    };
    let row_syms = bracket
        .as_ref()
        .map(|b| {
            b.trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let input_syms = input_param_syms_from_teaching_expr(base_expr, cap, map, catalog_entry_id);
    let rows = if bracket.is_some() {
        RowProjectionContract::Explicit { syms: row_syms }
    } else if witness_taught {
        RowProjectionContract::OmittedSameAsWitness
    } else {
        RowProjectionContract::Absent
    };
    let row_contract = RowContractLegend {
        inputs: input_syms,
        rows,
    };
    let expr = if let Some(b) = bracket {
        format!("{}{}", base_expr.trim(), b)
    } else {
        base_expr.trim().to_string()
    };
    let gloss = merge_result_gloss_with_row_contract(base_gloss, &row_contract);
    (expr, gloss, row_contract)
}

pub(crate) fn row_producer_projection_for_query_line(
    cap: &crate::CapabilitySchema,
    entity_sym: &str,
    line: &str,
) -> RowProducerProjection {
    if cap.kind == crate::CapabilityKind::Query && line == entity_sym {
        RowProducerProjection::BareQueryListAll
    } else {
        RowProducerProjection::CapabilityProvides
    }
}

/// Bracket `[p#,…]` from a capability's ordered `provides` (row contract), when non-empty.
pub(crate) fn capability_row_projection_bracket(
    cgs: &CGS,
    cap: &crate::CapabilitySchema,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    ename: &str,
    surface_filter: Option<&ExposureSurface>,
) -> Option<String> {
    let fields = cgs.effective_ordered_response_fields(cap);
    if fields.is_empty() {
        return None;
    }
    let syms: Vec<String> = fields
        .iter()
        .filter(|k| {
            surface_allows_entity_field(surface_filter, catalog_entry_id, ename, k.as_str())
        })
        .map(|k| id_sym_entity(map, catalog_entry_id, ename, k.as_str()))
        .collect();
    if syms.is_empty() {
        return None;
    }
    Some(format!("[{}]", syms.join(",")))
}

/// Opaque `p#` symbols for params appearing in `{…}` / `~"…"{…}` teaching exemplars.
pub(crate) fn input_param_syms_from_teaching_expr(
    expr: &str,
    _cap: &crate::CapabilitySchema,
    _map: Option<&SymbolMap>,
    _catalog_entry_id: &str,
) -> Vec<String> {
    let Some(open) = expr.find('{') else {
        return Vec::new();
    };
    let rest = &expr[open + 1..];
    let close = rest.find('}').unwrap_or(rest.len());
    let inner = rest[..close].trim();
    if inner.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for slot in split_top_level_commas(inner) {
        let slot = slot.trim();
        let Some((lhs, _)) = slot.split_once('=') else {
            continue;
        };
        let lhs = lhs.trim();
        if lhs.starts_with('p') && lhs.chars().skip(1).all(|c| c.is_ascii_digit()) {
            continue;
        }
        if !out.iter().any(|s: &String| s == lhs) {
            out.push(lhs.to_string());
        }
    }
    out
}

pub(crate) fn split_top_level_commas(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, ch) in input.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(input[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(input[start..].to_string());
    out
}

pub(crate) fn merge_result_gloss_with_row_contract(
    base_gloss: Option<String>,
    _row_contract: &RowContractLegend,
) -> Option<String> {
    base_gloss.filter(|s| !s.is_empty())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn try_push_row_producer_teaching_example(
    gloss_emit: &mut Option<GlossScratch<'_>>,
    teaching_rows: &mut Vec<EntityTeachingExprRow>,
    collect_meta: bool,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    ename: &str,
    surface_filter: Option<&ExposureSurface>,
    cap: &crate::CapabilitySchema,
    base_expr: &str,
    base_gloss: Option<String>,
    cap_leg: Option<String>,
    relation: Option<&RelationSchema>,
    omit_capability_prose: bool,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
    projection: RowProducerProjection,
    canonical_bracket: Option<&str>,
    witness_taught: bool,
) -> bool {
    let (expr, gloss, row_contract) = enrich_row_producer_teaching_line(
        cgs,
        cap,
        map,
        catalog_entry_id,
        ename,
        surface_filter,
        base_expr,
        base_gloss,
        projection,
        canonical_bracket,
        witness_taught,
    );
    try_push_teaching_example(
        gloss_emit,
        teaching_rows,
        collect_meta,
        cgs,
        &expr,
        gloss,
        cap_leg,
        relation,
        Some(&cap.name),
        omit_capability_prose,
        line_valid_cache,
        line_valid_cache_seed,
        map_arc,
        Some(row_contract),
    )
}
