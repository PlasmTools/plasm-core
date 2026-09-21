//! Relation navigation teaching rows and noun-card shape witnesses.

use std::collections::{HashMap, HashSet};

use crate::relation_nav::relation_nav_admissible;
use crate::schema::{Cardinality, EntityDef, RelationSchema};
#[cfg(test)]
use crate::symbol_tuning::ExposureSurface;
use crate::symbol_tuning::SymbolMap;
use crate::{CapabilityKind, CGS};

use super::gloss_collect::GlossScratch;
use super::input_legend::{RowContractLegend, TeachingExprLine};
use super::line_validate::{
    domain_line_work_valid_cached, DomainLineValidCacheKey, DomainLineValidEntry,
};
use super::query_teaching::{
    compound_get_expr_line, query_expr_filters_only, query_expr_maximal, query_expr_scope_only,
    unary_entity_id_teaching_expr_line,
};
#[cfg(test)]
use super::surface_filter::{surface_allows_relation_nav, surface_includes_exposed_entity};
use super::symbol_tokens::id_sym_rel;
#[cfg(test)]
use super::symbol_tokens::{ent_sym, id_sym_entity};
use super::teaching_push::try_push_teaching_example;
use super::tsv_emit::{teaching_relation_field_gloss, write_teaching_tsv_row, DomainTsvRow};
use super::{EntityTeachingExprRow, TeachingHeading};
use crate::symbol_tuning::description_for_agent_gloss;

/// Ordered receiver bases for teaching table dotted calls / relation nav on `ent` (`es` = entity symbol).
///
/// When `prefer_bare` is true (pathless Actions / Creates that need no identity), bare `eN` is
/// tried before `eN(<id>)` so taught forms match executable pathless login/create-session.
pub(crate) fn nav_receiver_candidates(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    prefer_bare: bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let push = |out: &mut Vec<String>, seen: &mut HashSet<String>, s: String| {
        if seen.insert(s.clone()) {
            out.push(s);
        }
    };

    if prefer_bare {
        push(&mut out, &mut seen, es.to_string());
    }

    if let Some(cmp) = compound_get_expr_line(es, ent, cgs, map, catalog_entry_id) {
        push(&mut out, &mut seen, cmp);
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
            push(&mut out, &mut seen, qline);
        }
    }
    let unary = unary_entity_id_teaching_expr_line(es, ent, map, catalog_entry_id);
    push(&mut out, &mut seen, unary);
    if !prefer_bare {
        push(&mut out, &mut seen, es.to_string());
    }
    out
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
    prefer_bare: bool,
) -> Option<String> {
    nav_receiver_candidates(es, ent, cgs, map, catalog_entry_id, prefer_bare)
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

/// StaticSingleton receivers only — never query `eN{…}` (PLP-4 rejects `eN{…}.r#`).
fn relation_singleton_receiver_candidates(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let push = |out: &mut Vec<String>, seen: &mut HashSet<String>, s: String| {
        if seen.insert(s.clone()) {
            out.push(s);
        }
    };
    if let Some(cmp) = compound_get_expr_line(es, ent, cgs, map, catalog_entry_id) {
        push(&mut out, &mut seen, cmp);
    }
    push(
        &mut out,
        &mut seen,
        unary_entity_id_teaching_expr_line(es, ent, map, catalog_entry_id),
    );
    push(&mut out, &mut seen, es.to_string());
    out
}

/// Receiver for relation nav / bare recv: must **parse and type-check alone**.
///
/// Test-only helper for [`incoming_relation_nav_bases_to_entity`]; production teaching uses
/// [`try_build_relation_nav_exemplar`].
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn relation_nav_anchor_expr(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) -> Option<String> {
    relation_singleton_receiver_candidates(es, ent, cgs, map, catalog_entry_id)
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

/// `ParentRecv.rel` expressions that type-check and return `target_ename` (incoming edges).
///
/// With `surface_filter: Some`, only edges whose **parent** (`src_name`) is in
/// [`ExposureSurface::entities`] and passes [`surface_allows_relation_nav`] for that slot are kept —
/// symmetric with outgoing relation-nav rows on the parent entity block.
#[cfg(test)]
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
        }
    }
    out
}

/// Maps parsed projection witness to a capability id for teaching table coverage (see [`covered_capabilities`]).
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
    let target = target_gloss.trim();
    // No opaque `e#` for the hop target → omit return atom (never invent a bare wire name).
    if target.is_empty() {
        return String::new();
    }
    match relation_receiver_teaching_hint(expr, map) {
        Some(h) => {
            // Glyph mirrors [`ReturnArrow`]: `↣` for a collection hop (`[e#]`), `→` for a single hop.
            let glyph = if target.starts_with('[') {
                super::ReturnArrow::List.glyph()
            } else {
                super::ReturnArrow::Single.glyph()
            };
            format!("relation {h} {glyph} {target}")
        }
        None => target.to_string(),
    }
}

/// Taught program-stratum fanout seat (domain-general CTE name; RA-4 bind then apply).
pub(crate) const RELATION_FANOUT_TEACHING_LABEL: &str = "rows";

/// Bind half of the fanout pair: `rows = <taught list query>`.
pub(crate) fn relation_fanout_bind_expr(query_head: &str) -> String {
    format!("{RELATION_FANOUT_TEACHING_LABEL} = {query_head}")
}

/// Apply half of the fanout pair: `rows => _.r#`.
pub(crate) fn relation_fanout_teaching_expr(rel_sym: &str) -> String {
    format!("{RELATION_FANOUT_TEACHING_LABEL} => _.{rel_sym}")
}

fn strip_teaching_projection_suffix(expr: &str) -> &str {
    let t = expr.trim();
    match super::tsv_emit::parse_trailing_projection_bracket(t) {
        Some(br) => t.strip_suffix(br.as_str()).map(str::trim).unwrap_or(t),
        None => t,
    }
}

/// First taught list-query head for `es` (brace query preferred; projection stripped).
fn taught_list_query_for_entity(
    teaching_rows: &[EntityTeachingExprRow],
    es: &str,
) -> Option<(String, Option<String>)> {
    let prefix_brace = format!("{es}{{");
    let mut bare: Option<(String, Option<String>)> = None;
    for row in teaching_rows {
        let expr = row.teaching_expr.expression.trim();
        if expr.contains(" => ") || split_top_level_eq_is_bind(expr) {
            continue;
        }
        let head = strip_teaching_projection_suffix(expr);
        let gloss = {
            let g = row.teaching_expr.result_type.trim();
            (!g.is_empty()).then(|| g.to_string())
        };
        if head.starts_with(&prefix_brace) {
            return Some((head.to_string(), gloss));
        }
        if head == es && bare.is_none() {
            bare = Some((head.to_string(), gloss));
        }
    }
    bare
}

fn split_top_level_eq_is_bind(expr: &str) -> bool {
    crate::expr_parser::split_assignment_for_binding(expr).is_some()
}

/// Synthesize the same list-query head query teaching would emit (no projection).
#[allow(clippy::too_many_arguments)]
fn synthesized_list_query_head(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) -> Option<String> {
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
            if domain_line_work_valid_cached(
                line_valid_cache,
                line_valid_cache_seed,
                cgs,
                &qline,
                map_arc,
            ) {
                return Some(qline);
            }
        }
    }
    None
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
    let recv = relation_singleton_receiver_candidates(es, ent, cgs, map, catalog_entry_id)
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
        })?;
    Some(format!("{recv}{suffix}"))
}

/// Push StaticSingleton `eN(<id>).r#` and, for declared relations, `rows = <query>` then `rows => _.r#`.
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
    let cardinality_many = rel_schema
        .map(|r| r.cardinality == Cardinality::Many)
        .unwrap_or(false);
    let target_gloss = crate::result_gloss::result_gloss_for_relation_nav(
        target_entity,
        map,
        catalog_entry_id,
        cardinality_many,
    )
    .unwrap_or_default();
    let gloss_key = format!("{es}(<id>).{rel_sym}");
    let result_gloss = relation_nav_meaning_result_gloss(&gloss_key, map, target_gloss);
    let singleton_already =
        super::tsv_emit::relation_sym_shown_in_query_teaching_rows(teaching_rows, rel_sym);
    let singleton_ok = !singleton_already
        && try_build_relation_nav_exemplar(
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
        )
        .is_some_and(|rel_expr| {
            try_push_teaching_example(
                gloss_emit,
                teaching_rows,
                collect_meta,
                cgs,
                &rel_expr,
                Some(result_gloss.clone()),
                rel_desc.clone(),
                rel_schema,
                None,
                false,
                line_valid_cache,
                line_valid_cache_seed,
                map_arc,
                None,
            )
        });
    let fanout_ok = rel_schema.is_some_and(|rel| {
        if !relation_nav_admissible(rel, cgs) {
            return false;
        }
        let Some((query_head, query_gloss)) = taught_list_query_for_entity(teaching_rows, es)
        else {
            return false;
        };
        let bind = relation_fanout_bind_expr(&query_head);
        let bind_already = teaching_rows
            .iter()
            .any(|r| r.teaching_expr.expression == bind);
        let bind_ok = bind_already
            || try_push_teaching_example(
                gloss_emit,
                teaching_rows,
                collect_meta,
                cgs,
                &bind,
                query_gloss,
                None,
                None,
                None,
                false,
                line_valid_cache,
                line_valid_cache_seed,
                map_arc,
                None,
            );
        if !bind_ok {
            return false;
        }
        let fanout = relation_fanout_teaching_expr(rel_sym);
        try_push_teaching_example(
            gloss_emit,
            teaching_rows,
            collect_meta,
            cgs,
            &fanout,
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
    });
    singleton_ok || fanout_ok
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
    catalog_entry_id: &str,
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
        catalog_entry_id,
        cardinality_many,
    )
    .unwrap_or_default();
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
        is_singleton_row_fetch: false,
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
        let description = {
            let d = rel_schema.description.as_str().trim();
            if d.is_empty() {
                String::new()
            } else {
                description_for_agent_gloss(d)
            }
        };
        if let Some(plasm_expr) = try_build_relation_nav_exemplar(
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
        ) {
            if seen_expr.insert(plasm_expr.clone()) {
                append_relation_nav_edge_delta_row(
                    &mut out,
                    &plasm_expr,
                    rel_schema,
                    &r_sym,
                    &description,
                    map_arc,
                    source.entry_id.as_str(),
                    &mut seen_r_gloss,
                    &empty_heading,
                );
            }
        }
        if relation_nav_admissible(rel_schema, cgs) {
            if let Some(query_head) = synthesized_list_query_head(
                &es,
                ent,
                cgs,
                map_arc.map(|m| m.as_ref()),
                source.entry_id.as_str(),
                &mut line_valid_cache,
                0,
                map_arc,
            ) {
                let bind = relation_fanout_bind_expr(&query_head);
                if domain_line_work_valid_cached(&mut line_valid_cache, 0, cgs, &bind, map_arc)
                    && seen_expr.insert(bind.clone())
                {
                    append_relation_nav_edge_delta_row(
                        &mut out,
                        &bind,
                        rel_schema,
                        &r_sym,
                        &description,
                        map_arc,
                        source.entry_id.as_str(),
                        &mut seen_r_gloss,
                        &empty_heading,
                    );
                }
                let fanout = relation_fanout_teaching_expr(&r_sym);
                if domain_line_work_valid_cached(&mut line_valid_cache, 0, cgs, &fanout, map_arc)
                    && seen_expr.insert(fanout.clone())
                {
                    append_relation_nav_edge_delta_row(
                        &mut out,
                        &fanout,
                        rel_schema,
                        &r_sym,
                        &description,
                        map_arc,
                        source.entry_id.as_str(),
                        &mut seen_r_gloss,
                        &empty_heading,
                    );
                }
            }
        }
    }
    out
}
