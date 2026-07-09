//! Relation navigation teaching rows and projection witnesses.

use std::collections::{HashMap, HashSet};

use crate::relation_nav::relation_nav_admissible;
use crate::schema::{Cardinality, EntityDef, RelationSchema};
use crate::symbol_tuning::{ExposureSurface, SymbolMap};
use crate::{CapabilityKind, CapabilityName, Expr, CGS};

use super::gloss_collect::GlossScratch;
use super::input_legend::{RowContractLegend, TeachingExprLine};
use super::line_validate::{
    domain_line_validate_cached, domain_line_work_valid_cached, DomainLineValidCacheKey,
    DomainLineValidEntry,
};
use super::query_teaching::{
    compound_get_expr_line, query_expr_filters_only, query_expr_maximal, query_expr_scope_only,
    unary_entity_id_teaching_expr_line,
};
use super::surface_filter::{surface_allows_relation_nav, surface_includes_exposed_entity};
use super::symbol_tokens::{ent_sym, id_sym_entity, id_sym_rel};
use super::teaching_push::try_push_teaching_example;
use super::teaching_util::truncate_inline_desc;
use super::tsv_emit::{teaching_relation_field_gloss, write_teaching_tsv_row, DomainTsvRow};
use super::{EntityTeachingExprRow, TeachingHeading};

/// Ordered receiver bases for teaching table dotted calls / relation nav on `ent` (`es` = entity symbol).
pub(crate) fn nav_receiver_candidates(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    if let Some(cmp) = compound_get_expr_line(es, ent, cgs, map, catalog_entry_id) {
        if seen.insert(cmp.clone()) {
            out.push(cmp);
        }
    }
    let mut query_caps: Vec<_> = cgs.find_capabilities(ent.name.as_str(), CapabilityKind::Query);
    query_caps.sort_by(|a, b| a.name.cmp(&b.name));
    for cap in &query_caps {
        for qline in [
            query_expr_maximal(cap, es, cgs, map, catalog_entry_id),
            query_expr_scope_only(cap, es, cgs, map, catalog_entry_id),
            query_expr_filters_only(cap, es, cgs, map, catalog_entry_id),
        ]
        .into_iter()
        .flatten()
        {
            if seen.insert(qline.clone()) {
                out.push(qline);
            }
        }
    }
    let unary = unary_entity_id_teaching_expr_line(es, ent, map, catalog_entry_id);
    if seen.insert(unary.clone()) {
        out.push(unary);
    }
    let bare = es.to_string();
    if seen.insert(bare.clone()) {
        out.push(bare);
    }
    out
}

/// Receiver for relation nav / bare recv: must **parse and type-check alone**.
#[allow(clippy::too_many_arguments)]
pub(crate) fn relation_nav_anchor_expr(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) -> Option<String> {
    nav_receiver_candidates(es, ent, cgs, map, catalog_entry_id)
        .into_iter()
        .find(|recv| {
            domain_line_work_valid_cached(
                line_valid_cache,
                line_valid_cache_seed,
                cgs,
                recv,
                map_arc,
            )
        })
}

/// First receiver such that `recv + suffix` is a valid full teaching table expression (e.g. `.m#(…)`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn receiver_for_dotted_suffix(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    suffix: &str,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) -> Option<String> {
    nav_receiver_candidates(es, ent, cgs, map, catalog_entry_id)
        .into_iter()
        .find(|recv| {
            let full = format!("{recv}{suffix}");
            domain_line_work_valid_cached(
                line_valid_cache,
                line_valid_cache_seed,
                cgs,
                &full,
                map_arc,
            )
        })
}

pub(crate) const MAX_INCOMING_REL_NAV_PROJECTION_BASES: usize = 16;

/// `ParentRecv.rel` expressions that type-check and return `target_ename` (incoming edges).
///
/// With `surface_filter: Some`, only edges whose **parent** (`src_name`) is in
/// [`ExposureSurface::entities`] and passes [`surface_allows_relation_nav`] for that slot are kept —
/// symmetric with outgoing relation-nav rows on the parent entity block.
#[allow(clippy::too_many_arguments)]
pub(crate) fn incoming_relation_nav_bases_to_entity(
    cgs: &CGS,
    target_ename: &str,
    map: Option<&SymbolMap>,
    surface_filter: Option<&ExposureSurface>,
    catalog_entry_id: &str,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) -> Vec<String> {
    use crate::schema::IncomingNavSlotKind;

    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for edge in cgs.incoming_nav_edges_to(target_ename) {
        let src_name = edge.source_entity.as_str();
        if !surface_includes_exposed_entity(surface_filter, cgs, catalog_entry_id, src_name) {
            continue;
        }
        let Some(src_ent) = cgs.get_entity(src_name) else {
            continue;
        };
        let parent_es = ent_sym(map, catalog_entry_id, src_name);
        let is_relation = matches!(edge.kind, IncomingNavSlotKind::Relation);
        if is_relation {
            let Some(rel_s) = src_ent.relations.get(edge.slot_name.as_str()) else {
                continue;
            };
            if rel_s.cardinality == Cardinality::Many && !relation_nav_admissible(rel_s, cgs) {
                continue;
            }
        }
        if !surface_allows_relation_nav(
            surface_filter,
            catalog_entry_id,
            src_name,
            edge.slot_name.as_str(),
            is_relation,
        ) {
            continue;
        }
        let Some(recv) = relation_nav_anchor_expr(
            &parent_es,
            src_ent,
            cgs,
            map,
            catalog_entry_id,
            line_valid_cache,
            line_valid_cache_seed,
            map_arc,
        ) else {
            continue;
        };
        let expr = if is_relation {
            format!(
                "{}.{}",
                recv,
                id_sym_rel(map, catalog_entry_id, src_name, edge.slot_name.as_str())
            )
        } else {
            format!(
                "{}.{}",
                recv,
                id_sym_entity(map, catalog_entry_id, src_name, edge.slot_name.as_str())
            )
        };
        if domain_line_work_valid_cached(
            line_valid_cache,
            line_valid_cache_seed,
            cgs,
            &expr,
            map_arc,
        ) && seen.insert(expr.clone())
        {
            out.push(expr);
            if out.len() >= MAX_INCOMING_REL_NAV_PROJECTION_BASES {
                return out;
            }
        }
    }
    out
}

/// Maps parsed projection witness to a capability id for teaching table coverage (see [`covered_capabilities`]).
pub(crate) fn projection_witness_source_capability<'a>(
    expr: &Expr,
    witness_cap: Option<&'a crate::CapabilitySchema>,
    primary_get_cap: Option<&'a crate::CapabilitySchema>,
    query_caps: &[&'a crate::CapabilitySchema],
) -> Option<&'a CapabilityName> {
    match expr {
        Expr::Get(_) => primary_get_cap.map(|c| &c.name),
        Expr::Query(_) => witness_cap
            .map(|c| &c.name)
            .or_else(|| query_caps.first().map(|c| &c.name)),
        _ => None,
    }
}

/// One validated `base[p#,…]` line teaching scalar projection for this entity type.
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_push_projection_witness_row(
    gloss_emit: &mut Option<GlossScratch<'_>>,
    teaching_rows: &mut Vec<EntityTeachingExprRow>,
    collect_meta: bool,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    bracket: &str,
    ename: &str,
    es: &str,
    ent: &EntityDef,
    primary_get_cap: Option<&crate::CapabilitySchema>,
    query_caps: &[&crate::CapabilitySchema],
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
    surface_filter: Option<&ExposureSurface>,
    catalog_entry_id: &str,
) -> bool {
    let bracket = bracket.trim();
    if bracket.is_empty() || !bracket.starts_with('[') {
        return false;
    }

    let mut seen_bases: HashSet<String> = HashSet::new();
    let mut attempts: Vec<(String, Option<&crate::CapabilitySchema>)> = Vec::new();

    let bare = es.to_string();
    if seen_bases.insert(bare.clone()) {
        attempts.push((bare, None));
    }
    for cap in query_caps {
        for qline in [
            query_expr_maximal(cap, es, cgs, map, catalog_entry_id),
            query_expr_scope_only(cap, es, cgs, map, catalog_entry_id),
            query_expr_filters_only(cap, es, cgs, map, catalog_entry_id),
        ]
        .into_iter()
        .flatten()
        {
            if seen_bases.insert(qline.clone()) {
                attempts.push((qline, Some(cap)));
            }
        }
    }
    if let Some(cmp) = compound_get_expr_line(es, ent, cgs, map, catalog_entry_id) {
        if seen_bases.insert(cmp.clone()) {
            attempts.push((cmp, primary_get_cap));
        }
    }
    for rel_base in incoming_relation_nav_bases_to_entity(
        cgs,
        ename,
        map,
        surface_filter,
        catalog_entry_id,
        line_valid_cache,
        line_valid_cache_seed,
        map_arc,
    ) {
        if seen_bases.insert(rel_base.clone()) {
            attempts.push((rel_base, None));
        }
    }
    // Unary identity get is omitted from projection attempts when list/query exists — teach
    // `e#{{…}}[p#,…]` instead of unary `e#(p#)[p#,…]` / `e#($)[p#,…]` (same policy as primary-get emission).
    if query_caps.is_empty() {
        let unary = unary_entity_id_teaching_expr_line(es, ent, map, catalog_entry_id);
        if seen_bases.insert(unary.clone()) {
            attempts.push((unary, primary_get_cap));
        }
    }

    for (base, witness_cap) in attempts {
        let full = format!("{base}{bracket}");
        let Some((parsed, _wire)) = domain_line_validate_cached(
            line_valid_cache,
            line_valid_cache_seed,
            cgs,
            &full,
            map_arc,
        ) else {
            continue;
        };
        let gloss_core = witness_cap
            .and_then(|c| crate::result_gloss::result_gloss_for_capability(c, cgs, map))
            .or_else(|| {
                primary_get_cap
                    .and_then(|c| crate::result_gloss::result_gloss_for_capability(c, cgs, map))
            })
            .unwrap_or_else(|| {
                if base.contains('{') {
                    crate::result_gloss::result_gloss_for_search_entity(ename, map)
                } else {
                    crate::result_gloss::result_gloss_for_get_entity(ename, map)
                }
            });
        let gloss = format!("{gloss_core} · projection");
        let source_cap = projection_witness_source_capability(
            &parsed.expr,
            witness_cap,
            primary_get_cap,
            query_caps,
        );
        return try_push_teaching_example(
            gloss_emit,
            teaching_rows,
            collect_meta,
            cgs,
            &full,
            Some(gloss),
            None,
            None,
            source_cap,
            false,
            line_valid_cache,
            line_valid_cache_seed,
            map_arc,
            None,
        );
    }
    false
}

/// Receiver token for relation-nav teaching: symbolic leading `e#`, else canonical entity name before `(` / `{`.
pub(crate) fn relation_receiver_teaching_hint(
    expr: &str,
    map: Option<&SymbolMap>,
) -> Option<String> {
    let t = expr.trim_start();
    if map.is_some() {
        if !t.starts_with('e') {
            return None;
        }
        let b = t.as_bytes();
        let mut end = 1usize;
        while end < b.len() && b[end].is_ascii_digit() {
            end += 1;
        }
        return (end > 1).then(|| t[..end].to_string());
    }
    let delim_idx = t.find(|c| ['(', '{'].contains(&c))?;
    let head = t[..delim_idx].trim();
    (!head.is_empty()).then(|| head.to_string())
}

pub(crate) fn relation_nav_meaning_result_gloss(
    expr: &str,
    map: Option<&SymbolMap>,
    target_gloss: String,
) -> String {
    match relation_receiver_teaching_hint(expr, map) {
        Some(h) => {
            // Glyph mirrors [`ReturnArrow`]: `↣` for a collection hop (`[e#]`), `→` for a single hop.
            let glyph = if target_gloss.trim_start().starts_with('[') {
                super::ReturnArrow::List.glyph()
            } else {
                super::ReturnArrow::Single.glyph()
            };
            format!("relation {h} {glyph} {target_gloss}")
        }
        None => target_gloss,
    }
}

/// Build a validated relation-nav exemplar (`recv.r#` or entity-ref hop) when admissible.
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_build_relation_nav_exemplar(
    es: &str,
    ent: &EntityDef,
    rel_schema: Option<&RelationSchema>,
    rel_sym: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) -> Option<String> {
    if let Some(rel) = rel_schema {
        if !relation_nav_admissible(rel, cgs) {
            return None;
        }
    }
    let suffix = format!(".{rel_sym}");
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

/// Push one relation-nav teaching row after building a validated exemplar.
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_emit_relation_nav_teaching_row(
    gloss_emit: &mut Option<GlossScratch<'_>>,
    teaching_rows: &mut Vec<EntityTeachingExprRow>,
    collect_meta: bool,
    es: &str,
    ent: &EntityDef,
    rel_schema: Option<&RelationSchema>,
    rel_sym: &str,
    target_entity: &str,
    rel_desc: Option<String>,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) -> bool {
    let Some(rel_expr) = try_build_relation_nav_exemplar(
        es,
        ent,
        rel_schema,
        rel_sym,
        cgs,
        map,
        catalog_entry_id,
        line_valid_cache,
        line_valid_cache_seed,
        map_arc,
    ) else {
        return false;
    };
    let cardinality_many = rel_schema
        .map(|r| r.cardinality == Cardinality::Many)
        .unwrap_or(false);
    let target_gloss =
        crate::result_gloss::result_gloss_for_relation_nav(target_entity, map, cardinality_many);
    let result_gloss = relation_nav_meaning_result_gloss(&rel_expr, map, target_gloss);
    try_push_teaching_example(
        gloss_emit,
        teaching_rows,
        collect_meta,
        cgs,
        &rel_expr,
        Some(result_gloss),
        rel_desc,
        rel_schema,
        None,
        false,
        line_valid_cache,
        line_valid_cache_seed,
        map_arc,
        None,
    )
}

/// Append one validated relation-hop row to an expand/federate edge-delta TSV body.
#[allow(clippy::too_many_arguments)]
fn append_relation_nav_edge_delta_row(
    out: &mut String,
    plasm_expr: &str,
    rel_schema: &RelationSchema,
    r_sym: &str,
    description: &str,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
    seen_r_gloss: &mut HashSet<String>,
    empty_heading: &TeachingHeading,
) {
    if let Some(m) = map_arc {
        if let Some(gloss) = teaching_relation_field_gloss(m, r_sym, description) {
            if seen_r_gloss.insert(r_sym.to_string()) {
                write_teaching_tsv_row(out, DomainTsvRow::FieldGloss(&gloss));
            }
        }
    }
    let cardinality_many = rel_schema.cardinality == Cardinality::Many;
    let target_gloss = crate::result_gloss::result_gloss_for_relation_nav(
        rel_schema.target_resource.as_str(),
        map_arc.map(|m| m.as_ref()),
        cardinality_many,
    );
    let result_type =
        relation_nav_meaning_result_gloss(plasm_expr, map_arc.map(|m| m.as_ref()), target_gloss);
    let line = TeachingExprLine::empty_legend(plasm_expr.to_string());
    let arrow = if cardinality_many {
        super::ReturnArrow::List
    } else {
        super::ReturnArrow::Single
    };
    let line = TeachingExprLine {
        expression: line.expression,
        result_type,
        legend: line.legend,
        is_projection_teaching: false,
        row_contract: RowContractLegend::default(),
        arrow,
    };
    write_teaching_tsv_row(
        out,
        DomainTsvRow::TeachingExpr {
            line: &line,
            identity_returns_row: false,
            attach_entity_heading: false,
            heading: empty_heading,
        },
    );
}

/// Thin relation-hop rows for expand/federate waves (parent entity already exposed; target just seeded).
pub(crate) fn render_relation_edge_delta_rows(
    exposure: &crate::symbol_tuning::TeachingExposureSession,
    new_relation_slots: &[crate::symbol_tuning::ExposureSlotKey],
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) -> String {
    const MAX_EDGE_DELTA_ROWS: usize = 8;
    let mut out = String::new();
    let mut seen_expr: HashSet<String> = HashSet::new();
    let mut seen_r_gloss: HashSet<String> = HashSet::new();
    let mut slots: Vec<_> = new_relation_slots
        .iter()
        .filter(|slot| matches!(slot, crate::symbol_tuning::ExposureSlotKey::Relation { .. }))
        .collect();
    slots.sort_by(|a, b| match (a, b) {
        (
            crate::symbol_tuning::ExposureSlotKey::Relation {
                source: sa,
                relation: ra,
            },
            crate::symbol_tuning::ExposureSlotKey::Relation {
                source: sb,
                relation: rb,
            },
        ) => (sa.entry_id.as_str(), sa.entity.as_str(), ra.as_str()).cmp(&(
            sb.entry_id.as_str(),
            sb.entity.as_str(),
            rb.as_str(),
        )),
        _ => std::cmp::Ordering::Equal,
    });

    let empty_heading = TeachingHeading {
        description: String::new(),
    };
    let mut line_valid_cache: HashMap<DomainLineValidCacheKey, DomainLineValidEntry> =
        HashMap::new();

    for slot in slots {
        if seen_expr.len() >= MAX_EDGE_DELTA_ROWS {
            break;
        }
        let crate::symbol_tuning::ExposureSlotKey::Relation { source, relation } = slot else {
            continue;
        };
        let Some(cgs) = exposure.catalog_cgs_for_entry(source.entry_id.as_str()) else {
            continue;
        };
        let Some(ent) = cgs.get_entity(source.entity.as_str()) else {
            continue;
        };
        let Some(rel_schema) = ent.relations.get(relation.as_str()) else {
            continue;
        };
        let Some(es) =
            exposure.qualified_entity_symbol(source.entry_id.as_str(), source.entity.as_str())
        else {
            continue;
        };
        let r_sym = id_sym_rel(
            map_arc.map(|m| m.as_ref()),
            source.entry_id.as_str(),
            source.entity.as_str(),
            relation.as_str(),
        );
        if !r_sym.starts_with('r') {
            continue;
        }
        let Some(plasm_expr) = try_build_relation_nav_exemplar(
            &es,
            ent,
            Some(rel_schema),
            &r_sym,
            cgs,
            map_arc.map(|m| m.as_ref()),
            source.entry_id.as_str(),
            &mut line_valid_cache,
            0,
            map_arc,
        ) else {
            continue;
        };
        if !seen_expr.insert(plasm_expr.clone()) {
            continue;
        }
        let description = {
            let d = rel_schema.description.as_str().trim();
            if d.is_empty() {
                String::new()
            } else {
                truncate_inline_desc(d, 120)
            }
        };
        append_relation_nav_edge_delta_row(
            &mut out,
            &plasm_expr,
            rel_schema,
            &r_sym,
            &description,
            map_arc,
            &mut seen_r_gloss,
            &empty_heading,
        );
    }
    out
}
