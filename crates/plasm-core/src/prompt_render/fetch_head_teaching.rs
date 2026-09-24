//! First executable `e#` fetch heads: sole bare `e#`, multi `e#.m#()`, optional `e#(<id>)`.

use std::collections::{HashMap, HashSet};

use crate::sole_nullary_singleton_get;
use crate::symbol_tuning::{ExposureSurface, IdentMetaKey, IdentMetadata, SymbolMap};
use crate::{CapabilityKind, CapabilitySchema, CGS};

use super::gloss_collect::GlossScratch;
use super::invoke_teaching::{capability_legend_with_session_gloss, receiver_absent};
use super::line_validate::{DomainLineValidCacheKey, DomainLineValidEntry};
use super::query_teaching::get_requires_identity_anchor;
use super::row_producer::with_projection_bracket;
use super::surface_filter::surface_allows_capability;
use super::symbol_tokens::met_sym;
use super::teaching_push::try_push_teaching_example;
use super::EntityTeachingExprRow;

/// Push singleton Get teaching rows (sole bare `e#` or multi `e#.m#()`). No noun/shape cards.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_entity_fetch_heads(
    gloss_emit: &mut Option<GlossScratch<'_>>,
    teaching_rows: &mut Vec<EntityTeachingExprRow>,
    collect_meta: bool,
    cgs: &CGS,
    ename: &str,
    es: &str,
    ent: &crate::schema::EntityDef,
    map: Option<&SymbolMap>,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
    surface_filter: Option<&ExposureSurface>,
    catalog_entry_id: &str,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    // Canonical `[p#,…]` projection when primary Get/Query teaches a field alphabet.
    projection_bracket: Option<&str>,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
) {
    let sole_cap = sole_nullary_singleton_get(cgs, ename)
        .filter(|cap| surface_allows_capability(surface_filter, catalog_entry_id, cap));

    if let Some(cap) = sole_cap {
        push_sole_nullary_bare_head(
            gloss_emit,
            teaching_rows,
            collect_meta,
            cgs,
            ename,
            es,
            map,
            map_arc,
            catalog_entry_id,
            ident_meta,
            cap,
            projection_bracket,
            line_valid_cache,
            line_valid_cache_seed,
        );
        return;
    }

    // Compound-key entities teach `e#(k=…)` later — not zero-arity `e#.m#()`.
    if ent.key_vars.len() > 1 {
        return;
    }

    let mut singleton_get_caps: Vec<_> = cgs
        .find_capabilities(ename, CapabilityKind::Get)
        .into_iter()
        .filter(|cap| surface_allows_capability(surface_filter, catalog_entry_id, cap))
        .filter(|cap| {
            receiver_absent(cap)
                && crate::capability_is_zero_arity_invoke(cap)
                && !get_requires_identity_anchor(cap, cgs, ent)
        })
        .collect();
    singleton_get_caps.sort_by(|a, b| a.name.cmp(&b.name));

    let mut seen: HashSet<String> = HashSet::new();
    for cap in &singleton_get_caps {
        if !seen.insert(cap.name.to_string()) {
            continue;
        }
        let ms = met_sym(map, catalog_entry_id, ename, cap);
        let expr = with_projection_bracket(format!("{es}.{ms}()"), projection_bracket);
        let result_gloss =
            crate::result_gloss::result_gloss_for_capability(cap, cgs, map, catalog_entry_id);
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
            teaching_rows,
            collect_meta,
            cgs,
            &expr,
            result_gloss,
            cap_leg,
            None,
            Some(&cap.name),
            line_valid_cache,
            line_valid_cache_seed,
            map_arc,
            None,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn push_sole_nullary_bare_head(
    gloss_emit: &mut Option<GlossScratch<'_>>,
    teaching_rows: &mut Vec<EntityTeachingExprRow>,
    collect_meta: bool,
    cgs: &CGS,
    ename: &str,
    es: &str,
    map: Option<&SymbolMap>,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
    catalog_entry_id: &str,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    cap: &CapabilitySchema,
    projection_bracket: Option<&str>,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
) {
    let result_gloss =
        crate::result_gloss::result_gloss_for_capability(cap, cgs, map, catalog_entry_id);
    let cap_leg =
        capability_legend_with_session_gloss(map, cgs, cap, ename, ident_meta, catalog_entry_id);
    let bare_expr = with_projection_bracket(es, projection_bracket);
    if try_push_teaching_example(
        gloss_emit,
        teaching_rows,
        collect_meta,
        cgs,
        &bare_expr,
        result_gloss,
        cap_leg,
        None,
        Some(&cap.name),
        line_valid_cache,
        line_valid_cache_seed,
        map_arc,
        None,
    ) {
        if let Some(row) = teaching_rows.last_mut() {
            row.teaching_expr.is_singleton_row_fetch = true;
            row.teaching_expr.arrow = super::ReturnArrow::Single;
        }
    }
    // Sole-nullary Get does not consume identity. Teaching `eN(<id>)` here is a
    // polarity lie (pathless seat plus a keyed form that will not parse/run).
}
