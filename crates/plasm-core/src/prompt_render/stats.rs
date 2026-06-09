//! Prompt surface statistics.

use super::*;

pub fn json_tool_surface_counts(
    cgs: &CGS,
    focus: FocusSpec<'_>,
    symbol_tuning: bool,
) -> (usize, usize) {
    let (names, _) =
        crate::symbol_tuning::resolve_prompt_surface_entities(cgs, focus, symbol_tuning);
    cap_nav_counts_from_names(cgs, &names)
}

fn cap_nav_counts_from_names(cgs: &CGS, names: &[String]) -> (usize, usize) {
    let full_set: HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let capability_tools = cgs
        .capabilities
        .values()
        .filter(|cap| full_set.contains(cap.domain.as_str()))
        .count();
    let mut navigation_tools = 0usize;
    for e in names {
        if let Some(ent) = cgs.get_entity(e.as_str()) {
            navigation_tools += navigation_edge_count(cgs, ent);
        }
    }
    (capability_tools, navigation_tools)
}

pub(crate) fn domain_expression_tool_count_resolved(
    cgs: &CGS,
    names: &[String],
    exposure_opt: Option<&crate::symbol_tuning::TeachingExposureSession>,
    symbol_tuning: bool,
) -> usize {
    let full_entities: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let map: Option<Arc<crate::symbol_tuning::SymbolMap>> = if symbol_tuning {
        exposure_opt.map(|e| e.symbol_map_arc())
    } else {
        Some(Arc::new(crate::symbol_tuning::SymbolMap::build(
            cgs,
            &full_entities,
        )))
    };
    let mut n = 0usize;
    let mut line_valid_cache = HashMap::new();
    let line_valid_cache_seed = exposure_opt
        .map(prompt_line_valid_cache_seed_exposure)
        .unwrap_or_else(|| prompt_line_valid_cache_seed_cgs(cgs));
    let surface_filter = exposure_opt.map(|e| &e.surface);
    let entity_catalog_ids: IndexMap<(&str, &str), ()> = exposure_opt
        .map(exposure_qualified_catalog_ids)
        .unwrap_or_default();
    for &ename in &full_entities {
        let mut seen_expr: HashSet<TeachingRowDedupeKey> = HashSet::new();
        let mut gloss_emit_none = None;
        let session_entry_id = catalog_entry_id_for_exposed_entity(&entity_catalog_ids, ename)
            .map(str::to_string)
            .or_else(|| cgs.entry_id.clone());
        let block = collect_entity_teaching_block(
            cgs,
            ename,
            map.as_deref(),
            None,
            false,
            &mut line_valid_cache,
            line_valid_cache_seed,
            map.clone(),
            &mut gloss_emit_none,
            surface_filter,
            session_entry_id.as_deref(),
        );
        for row in &block.teaching_rows {
            if seen_expr.insert(row.dedupe_key.clone()) {
                n += 1;
            }
        }
    }
    n
}

/// Full stats for a prompt string already rendered with `config` (same `config.focus` as render).
pub fn prompt_surface_stats(
    cgs: &CGS,
    config: RenderConfig<'_>,
    prompt: &str,
) -> PromptSurfaceStats {
    let (names, exposure_opt) = crate::symbol_tuning::resolve_prompt_surface_entities(
        cgs,
        config.focus,
        config.uses_symbols(),
    );
    let (capability_tools, navigation_tools) = cap_nav_counts_from_names(cgs, &names);
    let json_tool_estimate = domain_expression_tool_count_resolved(
        cgs,
        &names,
        exposure_opt.as_ref(),
        config.uses_symbols(),
    );
    let prompt_chars = prompt.chars().count();
    let token_estimate = prompt_chars / 4;
    let prompt_tokens_o200k = crate::o200k_token_count::o200k_token_count(prompt);
    PromptSurfaceStats {
        prompt_chars,
        token_estimate,
        prompt_tokens_o200k,
        capability_tools,
        navigation_tools,
        json_tool_estimate,
    }
}

fn navigation_edge_count(cgs: &CGS, ent: &EntityDef) -> usize {
    let rel_names: HashSet<&str> = ent.relations.keys().map(|s| s.as_str()).collect();
    let mut n = ent.relations.len();
    for (fname, f) in &ent.fields {
        if f.named_value(cgs)
            .ok()
            .is_some_and(|nv| matches!(nv.field_type, FieldType::EntityRef { .. }))
            && !rel_names.contains(fname.as_str())
        {
            n += 1;
        }
    }
    n
}
