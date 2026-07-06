//! Per-entity teaching block synthesis.

use std::collections::{HashMap, HashSet};

use crate::schema::{
    capability_is_zero_arity_invoke, Cardinality,
};
use crate::symbol_tuning::{
    ExposureSurface, IdentMetaKey, IdentMetadata, SymbolMap,
};
use crate::{CapabilityKind, CapabilityName, FieldType, CGS};

use super::gloss_collect::GlossScratch;
use super::gloss_filter;
use super::input_legend::RowContractLegend;
use super::invoke_teaching::{
    build_standalone_create_paren_args, capability_legend_with_session_gloss,
    emit_array_of_union_constructor_teaching_gloss, format_dotted_call_line, path_vars_empty,
    try_push_union_constructor_teaching_expr_rows,
};
use super::line_validate::{DomainLineValidCacheKey, DomainLineValidEntry};
use super::query_teaching::{
    compound_get_expr_line, query_expr_filters_only, query_expr_maximal, query_expr_scope_only,
    search_expr_with_filters, unary_entity_id_teaching_expr_line,
};
use super::relation_teaching::{
    many_relation_nav_emittable, receiver_for_dotted_suffix, relation_nav_meaning_result_gloss,
    try_push_projection_witness_row,
};
use super::row_producer::RowProducerProjection;
use super::row_producer_teaching::{
    enrich_row_producer_teaching_line, row_producer_projection_for_query_line,
    try_push_row_producer_teaching_example,
};
use super::surface_filter::{
    surface_allows_capability, surface_allows_entity_field, surface_allows_relation_nav,
    surface_exposes_relation_nav_target,
};
use super::symbol_tokens::{ent_sym, id_sym_entity, id_sym_rel, met_sym};
use super::teaching_push::try_push_teaching_example;
use super::tsv_emit::relation_sym_shown_in_query_teaching_rows;
use super::teaching_util::truncate_inline_desc;
use super::{
    EntityTeachingBlock, EntityTeachingExprRow, TeachingHeading,
};

const MAX_MULTI_ARITY_METHOD_LINES: usize = 16;

/// Non–zero-arity invoke/create/update: `e#($).m#(p#=…)` (same rules as parser dotted-call capability resolution).
#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_multi_arity_method_lines(
    cgs: &CGS,
    ename: &str,
    es: &str,
    map: Option<&SymbolMap>,
    surface_filter: Option<&ExposureSurface>,
    catalog_entry_id: &str,
    multi_arity_methods: &[&crate::CapabilitySchema],
    standalone_creates: &[&crate::CapabilitySchema],
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) -> Vec<(CapabilityName, String)> {
    let mut out: Vec<(CapabilityName, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let Some(ent) = cgs.get_entity(ename) else {
        return out;
    };

    for cap in multi_arity_methods {
        if !surface_allows_capability(surface_filter, catalog_entry_id, cap) {
            continue;
        }
        if !seen.insert(cap.name.to_string()) {
            continue;
        }
        if let Some(line) = format_dotted_call_line(
            ename,
            cap,
            ent,
            es,
            cgs,
            map,
            catalog_entry_id,
            line_valid_cache,
            line_valid_cache_seed,
            map_arc,
        ) {
            out.push((cap.name.clone(), line));
        }
    }
    // Anchored creates: `Parent($).create-child(args)` — cap.domain is the child,
    // but the CML path binds `{ename}_id` from the anchor.
    for cap_name in cgs.create_caps_for_anchor(ename) {
        let Some(cap) = cgs.capabilities.get(cap_name.as_str()) else {
            continue;
        };
        if !surface_allows_capability(surface_filter, catalog_entry_id, cap) {
            continue;
        }
        if !seen.insert(cap.name.to_string()) {
            continue;
        }
        if let Some(line) = format_dotted_call_line(
            ename,
            cap,
            ent,
            es,
            cgs,
            map,
            catalog_entry_id,
            line_valid_cache,
            line_valid_cache_seed,
            map_arc,
        ) {
            out.push((cap.name.clone(), line));
        }
    }

    // Standalone creates: `Entity.create(args)` — cap.domain == ename, no anchor needed.
    for cap in standalone_creates {
        if !surface_allows_capability(surface_filter, catalog_entry_id, cap) {
            continue;
        }
        if seen.contains(cap.name.as_str()) {
            continue;
        }
        if !seen.insert(cap.name.to_string()) {
            continue;
        }
        let ms = met_sym(map, catalog_entry_id, ename, cap);
        let line = match build_standalone_create_paren_args(ename, cap, cgs, map, catalog_entry_id)
        {
            Some(args) => format!("{es}.{ms}({args})"),
            None => format!("{es}.{ms}()"),
        };
        out.push((cap.name.clone(), line));
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.into_iter().take(MAX_MULTI_ARITY_METHOD_LINES).collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_entity_teaching_block(
    cgs: &CGS,
    ename: &str,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    collect_meta: bool,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    gloss_emit: &mut Option<GlossScratch<'_>>,
    surface_filter: Option<&ExposureSurface>,
    catalog_entry_id_override: Option<&str>,
) -> EntityTeachingBlock {
    let map: Option<&SymbolMap> = map_arc.map(|a| a.as_ref());
    let mut teaching_rows: Vec<EntityTeachingExprRow> = Vec::new();

    let Some(ent) = cgs.get_entity(ename) else {
        return EntityTeachingBlock {
            heading: TeachingHeading::default(),
            field_gloss_rows: Vec::new(),
            teaching_rows,
        };
    };
    let catalog_entry_id = catalog_entry_id_override
        .or(cgs.entry_id.as_deref())
        .unwrap_or("");
    let es = ent_sym(map, catalog_entry_id, ename);
    let manifest = cgs.capability_manifest(ename);
    let ent_desc_short = {
        let d = ent.description.as_str().trim();
        (!d.is_empty()).then(|| truncate_inline_desc(d, 200))
    };
    let heading = TeachingHeading::from_entity_banner_description(ent_desc_short.as_deref());
    if let Some(gs) = gloss_emit.as_mut() {
        gs.emit_before_teaching_example(&es, ent_desc_short.as_deref(), None, &[]);
    }

    let primary_get_projection_bracket: Option<String> = cgs
        .domain_projection_teaching_wire_fields(ename, ent)
        .and_then(|f| {
            let f: Vec<String> = f
                .into_iter()
                .filter(|k| {
                    surface_allows_entity_field(surface_filter, catalog_entry_id, ename, k.as_str())
                })
                .collect();
            if f.is_empty() {
                return None;
            }
            let syms: Vec<String> = f
                .iter()
                .map(|k| id_sym_entity(map, catalog_entry_id, ename, k.as_str()))
                .collect();
            Some(format!("[{}]", syms.join(",")))
        });

    let get_caps: Vec<_> = cgs
        .find_capabilities(ename, CapabilityKind::Get)
        .into_iter()
        .filter(|cap| surface_allows_capability(surface_filter, catalog_entry_id, cap))
        .collect();
    let only_singleton_gets = !get_caps.is_empty()
        && get_caps
            .iter()
            .all(|cap| path_vars_empty(cap) && capability_is_zero_arity_invoke(cap));

    let mut singleton_get_caps: Vec<_> = get_caps
        .iter()
        .copied()
        .filter(|cap| path_vars_empty(cap) && capability_is_zero_arity_invoke(cap))
        .collect();
    singleton_get_caps.sort_by(|a, b| a.name.cmp(&b.name));

    let get_gloss = Some(crate::result_gloss::result_gloss_for_get_entity(ename, map));
    let primary_get_cap = cgs
        .resolved_primary_get_for_projection(ename, ent)
        .filter(|cap| surface_allows_capability(surface_filter, catalog_entry_id, cap));

    let mut query_caps: Vec<_> = cgs
        .find_capabilities(ename, CapabilityKind::Query)
        .into_iter()
        .filter(|cap| surface_allows_capability(surface_filter, catalog_entry_id, cap))
        .collect();
    let primary_q_name = cgs.primary_query_capability(ename).map(|c| c.name.clone());
    query_caps.sort_by(|a, b| {
        let a_pri = primary_q_name.as_deref() == Some(a.name.as_str());
        let b_pri = primary_q_name.as_deref() == Some(b.name.as_str());
        match (a_pri, b_pri) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        }
    });
    let query_cap_refs: Vec<&crate::CapabilitySchema> = query_caps.to_vec();

    // Projection witness before other `e#…` lines for this entity (query/get/relation) so the field
    // narrow `[p#,…]` is taught once; row-producer lines omit the same bracket/`rows:` contract.
    let canonical_bracket = primary_get_projection_bracket
        .as_deref()
        .filter(|b| !b.trim().is_empty());
    let witness_taught = canonical_bracket.is_some_and(|bracket| {
        try_push_projection_witness_row(
            gloss_emit,
            &mut teaching_rows,
            collect_meta,
            cgs,
            map,
            bracket,
            ename,
            &es,
            ent,
            primary_get_cap,
            &query_cap_refs,
            line_valid_cache,
            line_valid_cache_seed,
            map_arc,
            surface_filter,
            catalog_entry_id,
        )
    });

    let mut seen_singleton_cap: HashSet<String> = HashSet::new();
    for cap in &singleton_get_caps {
        if !seen_singleton_cap.insert(cap.name.to_string()) {
            continue;
        }
        let ms = met_sym(map, catalog_entry_id, ename, cap);
        let expr = format!("{es}.{ms}()");
        let result_gloss = crate::result_gloss::result_gloss_for_capability(cap, cgs, map);
        let cap_leg = capability_legend_with_session_gloss(
            map,
            cgs,
            cap,
            ename,
            ident_meta,
            catalog_entry_id,
        );
        try_push_teaching_example(
            gloss_emit,
            &mut teaching_rows,
            collect_meta,
            cgs,
            &expr,
            result_gloss,
            cap_leg,
            None,
            Some(&cap.name),
            true,
            line_valid_cache,
            line_valid_cache_seed,
            map_arc,
            None,
        );
    }

    let mut emitted_primary_get = false;
    if primary_get_cap.is_some() && !only_singleton_gets {
        let primary_name = primary_get_cap.map(|c| &c.name);
        if let Some(cmp) = compound_get_expr_line(&es, ent, cgs, map, catalog_entry_id) {
            if try_push_teaching_example(
                gloss_emit,
                &mut teaching_rows,
                collect_meta,
                cgs,
                &cmp,
                get_gloss.clone(),
                None,
                None,
                primary_name,
                true,
                line_valid_cache,
                line_valid_cache_seed,
                map_arc,
                None,
            ) {
                emitted_primary_get = true;
            }
        }
        // Unary identity get only when there is no query surface (compound already attempted above).
        if !emitted_primary_get && query_caps.is_empty() {
            let line_base = unary_entity_id_teaching_expr_line(&es, ent, map, catalog_entry_id);
            if try_push_teaching_example(
                gloss_emit,
                &mut teaching_rows,
                collect_meta,
                cgs,
                &line_base,
                get_gloss.clone(),
                None,
                None,
                primary_name,
                true,
                line_valid_cache,
                line_valid_cache_seed,
                map_arc,
                None,
            ) {
                emitted_primary_get = true;
            }
        }
    }

    let mut zero_arity_method_caps: Vec<&crate::CapabilitySchema> = manifest
        .zero_arity_methods
        .iter()
        .copied()
        .filter(|cap| surface_allows_capability(surface_filter, catalog_entry_id, cap))
        .collect();
    zero_arity_method_caps.sort_by(|a, b| a.name.cmp(&b.name));

    let mut pathless: Vec<&crate::CapabilitySchema> = Vec::new();
    let mut pathful: Vec<&crate::CapabilitySchema> = Vec::new();
    for cap in &zero_arity_method_caps {
        if path_vars_empty(cap) {
            pathless.push(cap);
        } else {
            pathful.push(cap);
        }
    }

    for group in [&pathless, &pathful] {
        if group.is_empty() {
            continue;
        }
        for cap in group.iter() {
            let ms = met_sym(map, catalog_entry_id, ename, cap);
            let expr = if path_vars_empty(cap) {
                format!("{es}.{ms}()")
            } else {
                let suffix = format!(".{ms}()");
                let Some(recv) = receiver_for_dotted_suffix(
                    &es,
                    ent,
                    cgs,
                    map,
                    catalog_entry_id,
                    &suffix,
                    line_valid_cache,
                    line_valid_cache_seed,
                    map_arc,
                ) else {
                    continue;
                };
                format!("{recv}{suffix}")
            };
            let result_gloss = crate::result_gloss::result_gloss_for_capability(cap, cgs, map);
            let cap_leg = capability_legend_with_session_gloss(
                map,
                cgs,
                cap,
                ename,
                ident_meta,
                catalog_entry_id,
            );
            try_push_teaching_example(
                gloss_emit,
                &mut teaching_rows,
                collect_meta,
                cgs,
                &expr,
                result_gloss,
                cap_leg,
                None,
                Some(&cap.name),
                false,
                line_valid_cache,
                line_valid_cache_seed,
                map_arc,
                None,
            );
        }
    }
    for (cap_name, line) in collect_multi_arity_method_lines(
        cgs,
        ename,
        &es,
        map,
        surface_filter,
        catalog_entry_id,
        &manifest.multi_arity_methods,
        &manifest.standalone_creates,
        line_valid_cache,
        line_valid_cache_seed,
        map_arc,
    ) {
        let cap_ref = cgs.capabilities.get(&cap_name);
        if let Some(cap) = cap_ref {
            if let Some(gs) = gloss_emit.as_mut() {
                emit_array_of_union_constructor_teaching_gloss(gs, cap);
            }
            try_push_union_constructor_teaching_expr_rows(
                gloss_emit,
                &mut teaching_rows,
                collect_meta,
                cgs,
                map,
                cap,
                catalog_entry_id,
                line_valid_cache,
                line_valid_cache_seed,
                map_arc,
            );
        }
        let cap_leg = cap_ref.and_then(|c| {
            capability_legend_with_session_gloss(map, cgs, c, ename, ident_meta, catalog_entry_id)
        });
        let gloss =
            cap_ref.and_then(|c| crate::result_gloss::result_gloss_for_capability(c, cgs, map));
        try_push_teaching_example(
            gloss_emit,
            &mut teaching_rows,
            collect_meta,
            cgs,
            &line,
            gloss,
            cap_leg,
            None,
            Some(&cap_name),
            false,
            line_valid_cache,
            line_valid_cache_seed,
            map_arc,
            None,
        );
    }

    if !query_caps.is_empty() {
        let mut local_seen: HashSet<String> = HashSet::new();
        let mut query_line_count: usize = 0;
        const MAX_QUERY_LINES: usize = 2;
        for cap in &query_caps {
            if query_line_count >= MAX_QUERY_LINES {
                break;
            }
            let qgloss = crate::result_gloss::result_gloss_for_capability(cap, cgs, map);
            let cap_leg = capability_legend_with_session_gloss(
                map,
                cgs,
                cap,
                ename,
                ident_meta,
                catalog_entry_id,
            );
            let mut added = false;
            if let Some(line) = query_expr_maximal(cap, &es, cgs, map, catalog_entry_id) {
                let projection = row_producer_projection_for_query_line(cap, &es, &line);
                if local_seen.insert(line.clone())
                    && try_push_row_producer_teaching_example(
                        gloss_emit,
                        &mut teaching_rows,
                        collect_meta,
                        cgs,
                        map,
                        catalog_entry_id,
                        ename,
                        surface_filter,
                        cap,
                        &line,
                        qgloss.clone(),
                        cap_leg.clone(),
                        None,
                        true,
                        line_valid_cache,
                        line_valid_cache_seed,
                        map_arc,
                        projection,
                        canonical_bracket,
                        witness_taught,
                    )
                {
                    added = true;
                    query_line_count += 1;
                }
            }
            if !added {
                if let Some(line) = query_expr_scope_only(cap, &es, cgs, map, catalog_entry_id) {
                    if local_seen.insert(line.clone())
                        && try_push_row_producer_teaching_example(
                            gloss_emit,
                            &mut teaching_rows,
                            collect_meta,
                            cgs,
                            map,
                            catalog_entry_id,
                            ename,
                            surface_filter,
                            cap,
                            &line,
                            qgloss.clone(),
                            cap_leg.clone(),
                            None,
                            true,
                            line_valid_cache,
                            line_valid_cache_seed,
                            map_arc,
                            RowProducerProjection::CapabilityProvides,
                            canonical_bracket,
                            witness_taught,
                        )
                    {
                        added = true;
                        query_line_count += 1;
                    }
                }
            }
            if !added {
                if let Some(line) = query_expr_filters_only(cap, &es, cgs, map, catalog_entry_id) {
                    if local_seen.insert(line.clone())
                        && try_push_row_producer_teaching_example(
                            gloss_emit,
                            &mut teaching_rows,
                            collect_meta,
                            cgs,
                            map,
                            catalog_entry_id,
                            ename,
                            surface_filter,
                            cap,
                            &line,
                            qgloss.clone(),
                            cap_leg.clone(),
                            None,
                            true,
                            line_valid_cache,
                            line_valid_cache_seed,
                            map_arc,
                            RowProducerProjection::CapabilityProvides,
                            canonical_bracket,
                            witness_taught,
                        )
                    {
                        query_line_count += 1;
                    }
                }
            }
        }
    }

    // Unary `e#(p…)` / `e#($)` after query lines when primary GET was not emitted earlier.
    if primary_get_cap.is_some()
        && !only_singleton_gets
        && !emitted_primary_get
        && !query_caps.is_empty()
    {
        let primary_name = primary_get_cap.map(|c| &c.name);
        let keyed = unary_entity_id_teaching_expr_line(&es, ent, map, catalog_entry_id);
        let _ = try_push_teaching_example(
            gloss_emit,
            &mut teaching_rows,
            collect_meta,
            cgs,
            &keyed,
            get_gloss.clone(),
            None,
            None,
            primary_name,
            true,
            line_valid_cache,
            line_valid_cache_seed,
            map_arc,
            None,
        );
    }

    let mut search_caps: Vec<_> = cgs
        .find_capabilities(ename, CapabilityKind::Search)
        .into_iter()
        .filter(|cap| surface_allows_capability(surface_filter, catalog_entry_id, cap))
        .collect();
    if !search_caps.is_empty() {
        let line = format!("{es}~\"text\"");
        search_caps.sort_by(|a, b| a.name.cmp(&b.name));
        let scap = cgs
            .primary_search_capability(ename)
            .filter(|cap| surface_allows_capability(surface_filter, catalog_entry_id, cap))
            .or_else(|| search_caps.first().copied());
        let sg =
            scap.and_then(|cap| crate::result_gloss::result_gloss_for_capability(cap, cgs, map));
        let cap_leg = scap.and_then(|cap| {
            capability_legend_with_session_gloss(map, cgs, cap, ename, ident_meta, catalog_entry_id)
        });
        let (search_line, search_gloss, search_contract) = scap.map_or_else(
            || (line.clone(), sg.clone(), RowContractLegend::default()),
            |cap| {
                enrich_row_producer_teaching_line(
                    cgs,
                    cap,
                    map,
                    catalog_entry_id,
                    ename,
                    surface_filter,
                    &line,
                    sg.clone(),
                    RowProducerProjection::CapabilityProvides,
                    canonical_bracket,
                    witness_taught,
                )
            },
        );
        try_push_teaching_example(
            gloss_emit,
            &mut teaching_rows,
            collect_meta,
            cgs,
            &search_line,
            search_gloss,
            cap_leg.clone(),
            None,
            scap.map(|c| &c.name),
            true,
            line_valid_cache,
            line_valid_cache_seed,
            map_arc,
            Some(search_contract),
        );
        if let (Some(cap), Some(filter_line)) = (
            scap,
            scap.and_then(|cap| search_expr_with_filters(cap, &es, cgs, map, catalog_entry_id)),
        ) {
            try_push_row_producer_teaching_example(
                gloss_emit,
                &mut teaching_rows,
                collect_meta,
                cgs,
                map,
                catalog_entry_id,
                ename,
                surface_filter,
                cap,
                &filter_line,
                sg,
                cap_leg,
                None,
                true,
                line_valid_cache,
                line_valid_cache_seed,
                map_arc,
                RowProducerProjection::CapabilityProvides,
                canonical_bracket,
                witness_taught,
            );
        }
    }

    let mut nav_keys: Vec<String> = ent
        .relations
        .keys()
        .map(|k| k.as_str().to_string())
        .collect();
    let rel_names: HashSet<&str> = ent.relations.keys().map(|s| s.as_str()).collect();
    for fname in ent.fields.keys() {
        if let Some(f) = ent.fields.get(fname) {
            if f.named_value(cgs)
                .ok()
                .is_some_and(|nv| matches!(nv.field_type, FieldType::EntityRef { .. }))
                && !rel_names.contains(fname.as_str())
            {
                nav_keys.push(fname.as_str().to_string());
            }
        }
    }
    nav_keys.sort();
    const MAX_REL_NAV_LINES: usize = 4;
    for rel in nav_keys.iter().take(MAX_REL_NAV_LINES) {
        let (target_entity, skip_many_unresolved, rel_for_meta) =
            if let Some(rel_schema) = ent.relations.get(rel.as_str()) {
                if !surface_allows_relation_nav(
                    surface_filter,
                    catalog_entry_id,
                    ename,
                    rel.as_str(),
                    true,
                ) {
                    continue;
                }
                let skip = rel_schema.cardinality == Cardinality::Many
                    && !many_relation_nav_emittable(rel_schema);
                (rel_schema.target_resource.clone(), skip, Some(rel_schema))
            } else if let Some(f) = ent.fields.get(rel.as_str()) {
                if !surface_allows_relation_nav(
                    surface_filter,
                    catalog_entry_id,
                    ename,
                    rel.as_str(),
                    false,
                ) {
                    continue;
                }
                match f.named_value(cgs) {
                    Ok(nv) => match &nv.field_type {
                        FieldType::EntityRef { target } => (target.clone(), false, None),
                        _ => continue,
                    },
                    Err(_) => continue,
                }
            } else {
                continue;
            };
        if !surface_exposes_relation_nav_target(
            surface_filter,
            cgs,
            catalog_entry_id,
            target_entity.as_str(),
        ) {
            continue;
        }
        if skip_many_unresolved {
            continue;
        }
        let rel_sym = if rel_for_meta.is_some() {
            id_sym_rel(map, catalog_entry_id, ename, rel.as_str())
        } else {
            id_sym_entity(map, catalog_entry_id, ename, rel.as_str())
        };
        if relation_sym_shown_in_query_teaching_rows(&teaching_rows, &rel_sym) {
            continue;
        }
        let suffix = format!(".{rel_sym}");
        let Some(recv) = receiver_for_dotted_suffix(
            &es,
            ent,
            cgs,
            map,
            catalog_entry_id,
            &suffix,
            line_valid_cache,
            line_valid_cache_seed,
            map_arc,
        ) else {
            continue;
        };
        let rel_expr = format!("{recv}{suffix}");
        // Relation prose lives on the standalone `r#` gloss row; nav exemplars carry typing only.
        let rel_desc_opt = if rel_for_meta.is_some() {
            None
        } else if let Some(f) = ent.fields.get(rel.as_str()) {
            let d = f.description.as_str().trim();
            if d.is_empty() {
                None
            } else {
                Some(truncate_inline_desc(d, 120))
            }
        } else {
            None
        };
        let cardinality_many = ent
            .relations
            .get(rel.as_str())
            .map(|r| r.cardinality == Cardinality::Many)
            .unwrap_or(false);
        let target_gloss = crate::result_gloss::result_gloss_for_relation_nav(
            target_entity.as_str(),
            map,
            cardinality_many,
        );
        let result_gloss = relation_nav_meaning_result_gloss(&rel_expr, map, target_gloss);
        try_push_teaching_example(
            gloss_emit,
            &mut teaching_rows,
            collect_meta,
            cgs,
            &rel_expr,
            Some(result_gloss),
            rel_desc_opt,
            rel_for_meta,
            None,
            false,
            line_valid_cache,
            line_valid_cache_seed,
            map_arc,
            None,
        );
    }

    let mut field_gloss_rows = gloss_emit
        .as_mut()
        .map(|gs| std::mem::take(gs.field_gloss))
        .unwrap_or_default();
    field_gloss_rows = gloss_filter::filter_field_gloss_to_referenced_symbols(
        &field_gloss_rows,
        &teaching_rows,
        &es,
        map,
        catalog_entry_id,
        ename,
    );

    EntityTeachingBlock {
        heading,
        field_gloss_rows,
        teaching_rows,
    }
}
