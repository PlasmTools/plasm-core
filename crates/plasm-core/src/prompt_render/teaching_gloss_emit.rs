//! Teaching-table TSV row synthesis (per-catalog dynamic prompts).

use super::gloss_dedup::*;
use super::*;

pub(crate) fn teaching_expr_line_fingerprint(row: &TeachingExprLine) -> String {
    format!(
        "{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
        row.expression,
        row.result_type,
        row.legend.scope,
        row.legend.optional.is_present() as u8,
        row.legend.compact_args,
        row.legend.description,
        row.is_projection_teaching as u8,
    )
}

/// Post-alias dedupe must keep distinct capability witnesses even when `p#` collapse makes
/// [`teaching_expr_line_fingerprint`] collide (Proof `document_edit_v2` vs other dotted calls).
fn post_rewrite_teaching_row_fingerprint(
    row: &EntityTeachingExprRow,
    meta: &TeachingLineMeta,
) -> String {
    format!(
        "{}|cap:{}",
        teaching_expr_line_fingerprint(&row.teaching_expr),
        meta.source_capability.as_deref().unwrap_or("")
    )
}

pub(crate) fn rewrite_teaching_expr_line_opaque_tokens(
    row: &mut TeachingExprLine,
    rep: &HashMap<String, String>,
) {
    row.expression = crate::symbol_tuning::rewrite_opaque_ident_tokens(&row.expression, rep);
    row.result_type = crate::symbol_tuning::rewrite_opaque_ident_tokens(&row.result_type, rep);
    row.legend.scope = crate::symbol_tuning::rewrite_opaque_ident_tokens(&row.legend.scope, rep);
    if row.legend.optional.is_present() {
        row.legend.optional = OptionalLegend::Present;
    }
    row.legend.compact_args =
        crate::symbol_tuning::rewrite_opaque_ident_tokens(&row.legend.compact_args, rep);
    row.legend.description =
        crate::symbol_tuning::rewrite_opaque_ident_tokens(&row.legend.description, rep);
}

pub(crate) fn rewrite_field_gloss_opaque_tokens(
    g: &mut TeachingFieldGloss,
    rep: &HashMap<String, String>,
) {
    g.symbol = crate::symbol_tuning::rewrite_opaque_ident_tokens(&g.symbol, rep);
    g.field_type = crate::symbol_tuning::rewrite_opaque_ident_tokens(&g.field_type, rep);
    g.allowed_values = crate::symbol_tuning::rewrite_opaque_ident_tokens(&g.allowed_values, rep);
    g.description = crate::symbol_tuning::rewrite_opaque_ident_tokens(&g.description, rep);
}

/// Tracks `p#` / `v#` gloss lines emitted before teaching example rows (first-use only).
pub(crate) struct FieldGlossEmitState {
    pub(crate) registry_p_slot_compact_gloss: HashMap<String, String>,
    pub(crate) registry_compact_meaning_canonical_p: HashMap<String, String>,
    pub(crate) registry_p_sym_alias: HashMap<String, String>,
    pub(crate) registry_value_gloss_canonical_v: HashMap<String, String>,
    pub(crate) registry_v_sym_alias: HashMap<String, String>,
    pub(crate) non_registry_slots: HashMap<String, IdentMetadata>,
    pub(crate) defined_value_domains: HashSet<String>,
}

/// Shared per-render caches for teaching table table synthesis (line validation, gloss dedup, metadata).
pub(crate) struct TeachingSynthesisSession<'a> {
    line_valid_cache: HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    gloss_emit_state: FieldGlossEmitState,
    map_arc: Option<std::sync::Arc<SymbolMap>>,
    ident_meta: Option<HashMap<crate::symbol_tuning::IdentMetaKey, IdentMetadata>>,
    surface_filter: Option<&'a ExposureSurface>,
    entity_catalog_ids: IndexMap<(&'a str, &'a str), ()>,
    collect_meta: bool,
}

impl<'a> TeachingSynthesisSession<'a> {
    fn new(
        line_valid_cache_seed: u64,
        map_arc: Option<std::sync::Arc<SymbolMap>>,
        ident_meta: Option<HashMap<crate::symbol_tuning::IdentMetaKey, IdentMetadata>>,
        surface_filter: Option<&'a ExposureSurface>,
        entity_catalog_ids: IndexMap<(&'a str, &'a str), ()>,
        collect_meta: bool,
    ) -> Self {
        Self {
            line_valid_cache: HashMap::with_capacity(8192),
            line_valid_cache_seed,
            gloss_emit_state: FieldGlossEmitState {
                registry_p_slot_compact_gloss: HashMap::new(),
                registry_compact_meaning_canonical_p: HashMap::new(),
                registry_p_sym_alias: HashMap::new(),
                registry_value_gloss_canonical_v: HashMap::new(),
                registry_v_sym_alias: HashMap::new(),
                non_registry_slots: HashMap::new(),
                defined_value_domains: HashSet::new(),
            },
            map_arc,
            ident_meta,
            surface_filter,
            entity_catalog_ids,
            collect_meta,
        }
    }

    fn apply_opaque_alias_rewrites(
        &self,
        teaching_blocks_out: &mut [EntityTeachingBlock],
        model_out: &mut [EntityTeachingPrompt],
    ) {
        let rep = merge_opaque_alias_maps(
            &self.gloss_emit_state.registry_p_sym_alias,
            &self.gloss_emit_state.registry_v_sym_alias,
        );
        if rep.is_empty() {
            return;
        }
        if self.collect_meta {
            debug_assert_eq!(
                teaching_blocks_out.len(),
                model_out.len(),
                "model rows must stay aligned with teaching blocks"
            );
            for (block, prompt) in teaching_blocks_out.iter_mut().zip(model_out.iter_mut()) {
                for g in &mut block.field_gloss_rows {
                    rewrite_field_gloss_opaque_tokens(g, &rep);
                }
                for row in &mut block.teaching_rows {
                    rewrite_teaching_expr_line_opaque_tokens(&mut row.teaching_expr, &rep);
                    row.meta.expression = crate::symbol_tuning::rewrite_opaque_ident_tokens(
                        &row.meta.expression,
                        &rep,
                    );
                }
                let mut seen = HashSet::new();
                let mut new_rows = Vec::new();
                let mut new_lines = Vec::new();
                for (row, meta) in block.teaching_rows.drain(..).zip(prompt.lines.drain(..)) {
                    let fp = post_rewrite_teaching_row_fingerprint(&row, &meta);
                    if seen.insert(fp) {
                        new_rows.push(row);
                        new_lines.push(meta);
                    }
                }
                block.teaching_rows = new_rows;
                prompt.lines = new_lines;
            }
        } else {
            for block in teaching_blocks_out.iter_mut() {
                for g in &mut block.field_gloss_rows {
                    rewrite_field_gloss_opaque_tokens(g, &rep);
                }
                for row in &mut block.teaching_rows {
                    rewrite_teaching_expr_line_opaque_tokens(&mut row.teaching_expr, &rep);
                    row.meta.expression = crate::symbol_tuning::rewrite_opaque_ident_tokens(
                        &row.meta.expression,
                        &rep,
                    );
                }
                let mut seen = HashSet::new();
                block.teaching_rows.retain(|row| {
                    let fp = post_rewrite_teaching_row_fingerprint(row, &row.meta);
                    seen.insert(fp)
                });
            }
        }
    }
}

/// Per-entity field gloss rows built directly into [`TeachingFieldGloss`] (no compact transcript).
pub(crate) struct GlossScratch<'a> {
    pub(crate) field_gloss: &'a mut Vec<TeachingFieldGloss>,
    pub(crate) state: &'a mut FieldGlossEmitState,
    pub(crate) map: &'a SymbolMap,
    pub(crate) meta: &'a HashMap<IdentMetaKey, IdentMetadata>,
    pub(crate) catalog_entry_id: &'a str,
    pub(crate) entity: &'a str,
    pub(crate) cgs: &'a CGS,
}

impl GlossScratch<'_> {
    pub(crate) fn emit_before_teaching_example(
        &mut self,
        expr: &str,
        cap_legend: Option<&str>,
        result_gloss: Option<&str>,
        optional_param_syms: &[String],
    ) {
        emit_field_def_lines_before_example(
            self.field_gloss,
            expr,
            cap_legend,
            result_gloss,
            optional_param_syms,
            self.map,
            self.entity,
            self.catalog_entry_id,
            self.meta,
            self.state,
            self.cgs,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_field_def_lines_before_example(
    out: &mut Vec<TeachingFieldGloss>,
    expr: &str,
    cap_legend: Option<&str>,
    result_gloss: Option<&str>,
    optional_param_syms: &[String],
    map: &SymbolMap,
    entity: &str,
    catalog_entry_id: &str,
    ident_meta: &HashMap<IdentMetaKey, IdentMetadata>,
    state: &mut FieldGlossEmitState,
    cgs: &CGS,
) {
    let en = EntityName::from(entity.to_string());
    let cid = catalog_entry_id.to_string();
    for sym in crate::symbol_tuning::field_syms_for_teaching_row(
        expr,
        result_gloss,
        cap_legend,
        optional_param_syms,
    ) {
        let wire_owned = map.wire_for_opaque_p_sym(sym.as_str());
        let field_name = if sym.starts_with('r') {
            map.resolve_relation_ident(sym.as_str())
                .unwrap_or(sym.as_str())
        } else {
            wire_owned.as_deref().unwrap_or(sym.as_str())
        };
        // Capability `p#` maps to a leaf expand key (e.g. `blocks`) that may equal a relation wire
        // name on the same entity — resolve metadata from the full param path + CGS, not relation gloss.
        let meta = map
            .capability_param_quad_for_p_sym(sym.as_str())
            .and_then(|(eid, dom, cap, path)| {
                if !eid.is_empty() && eid.as_str() != catalog_entry_id {
                    return None;
                }
                crate::symbol_tuning::ident_metadata_for_capability_input_path(
                    cgs,
                    dom.as_str(),
                    cap.as_str(),
                    path.as_str(),
                )
            })
            .or_else(|| {
                map.capability_param_key_for_p_sym(sym.as_str())
                    .and_then(|(dom, w)| {
                        ident_meta
                            .get(&(cid.clone(), dom.clone(), w.clone()))
                            .cloned()
                    })
            })
            .or_else(|| {
                ident_meta
                    .get(&(cid.clone(), en.clone(), field_name.to_string()))
                    .cloned()
            });

        // Registry-backed `value_ref` slots: dedupe `v#` by gloss body first, then `p#` by compact Meaning.
        // Rewrite embedded `v#` in compact strings using [`registry_v_sym_alias`] before `p#` dedupe.
        if let (Some(m), Some(vs)) = (&meta, map.value_sym_for_p_sym(sym.as_str())) {
            if let IdentMetadata::RegistryBacked { .. } = m {
                if let Some(vg) = map.value_domain_gloss_for_v_sym(&vs) {
                    if let Some(v_canon) = meaning_canonical_sym_for_emit(
                        vg,
                        &vs,
                        &mut state.registry_value_gloss_canonical_v,
                        &mut state.registry_v_sym_alias,
                    ) {
                        if state.defined_value_domains.insert(v_canon.clone()) {
                            push_teaching_field_gloss_row(
                                out,
                                v_canon.clone(),
                                vg,
                                entity,
                                catalog_entry_id,
                                Some(map),
                                Some(ident_meta),
                                Some(cgs),
                                false,
                            );
                        }
                    }
                    let compact_raw =
                        compact_p_slot_registry_description(map, sym.as_str(), m, cgs)
                            .unwrap_or_else(|| {
                                let w = crate::symbol_tuning::registry_backed_compact_wire_label(m);
                                let mut c = format!("{} · {}", vs, w);
                                let d = m.description().trim();
                                if !d.is_empty() {
                                    let t = crate::symbol_tuning::gloss_description_truncated(d);
                                    c = format!("{} · {} · {}", vs, w, t);
                                }
                                c
                            });
                    let compact = crate::symbol_tuning::rewrite_opaque_ident_tokens(
                        &compact_raw,
                        &state.registry_v_sym_alias,
                    );
                    let slot = gloss_slot_identity_for_p_sym(map, sym.as_str(), m);
                    let p_meaning_key =
                        gloss_p_slot_meaning_key(&compact, m.catalog_entry_id(), &slot);
                    if meaning_canonical_sym_for_emit(
                        &p_meaning_key,
                        sym.as_str(),
                        &mut state.registry_compact_meaning_canonical_p,
                        &mut state.registry_p_sym_alias,
                    )
                    .is_none()
                    {
                        state
                            .registry_p_slot_compact_gloss
                            .insert(sym.clone(), compact.clone());
                        continue;
                    }
                    if state
                        .registry_p_slot_compact_gloss
                        .get(&sym)
                        .is_some_and(|prev| prev == &compact)
                    {
                        continue;
                    }
                    state
                        .registry_p_slot_compact_gloss
                        .insert(sym.clone(), compact.clone());
                    push_teaching_field_gloss_row(
                        out,
                        sym.clone(),
                        &compact,
                        entity,
                        catalog_entry_id,
                        Some(map),
                        Some(ident_meta),
                        Some(cgs),
                        false,
                    );
                    continue;
                }
            }
        }

        let should_emit = match (&meta, state.non_registry_slots.get(&sym)) {
            (Some(m), None) => {
                state.non_registry_slots.insert(sym.clone(), (*m).clone());
                true
            }
            (Some(m), Some(prev)) if *prev != *m => {
                state.non_registry_slots.insert(sym.clone(), (*m).clone());
                true
            }
            (None, None) => {
                state.non_registry_slots.insert(
                    sym.clone(),
                    crate::symbol_tuning::IdentMetadata::SyntheticUnknown {
                        catalog_entry_id: cid.clone(),
                        entity: en.clone(),
                        wire_name: field_name.to_string(),
                        description: field_name.to_string(),
                    },
                );
                true
            }
            _ => false,
        };
        if should_emit {
            let gloss = match meta {
                Some(m) => m.render_gloss_with_cgs(Some(map), Some(cgs)),
                None => field_name.to_string(),
            };
            if sym.starts_with('p')
                && state
                    .registry_p_slot_compact_gloss
                    .get(sym.as_str())
                    .is_some_and(|prev| prev == &gloss)
            {
                continue;
            }
            if sym.starts_with('p') {
                state
                    .registry_p_slot_compact_gloss
                    .insert(sym.clone(), gloss.clone());
            }
            push_teaching_field_gloss_row(
                out,
                sym.clone(),
                &gloss,
                entity,
                catalog_entry_id,
                Some(map),
                Some(ident_meta),
                Some(cgs),
                false,
            );
        }
    }
}

/// Per-entity many-shot examples — `focus` still subsets *which* entities appear.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_teaching_table_resolved<'b, F>(
    mut resolve: F,
    full_entities: &[&str],
    map_arc: Option<std::sync::Arc<SymbolMap>>,
    exposure_for_ident: Option<&TeachingExposureSession>,
    teaching_blocks_out: &mut Vec<EntityTeachingBlock>,
    model_out: &mut Vec<EntityTeachingPrompt>,
    fill_model: bool,
    _include_contract_preamble: bool,
    emit_entity_blocks: Option<&[&str]>,
    emit_entity_keys: Option<&std::collections::BTreeSet<(String, String)>>,
    federated_blocks: Option<&[(String, &str)]>,
    federated_by_entry: Option<&'b IndexMap<String, &'b CGS>>,
) where
    F: FnMut(&str) -> &'b CGS,
{
    let line_valid_cache_seed = full_entities
        .first()
        .map(|&ename| prompt_line_valid_cache_seed_cgs(resolve(ename)))
        .unwrap_or_else(|| {
            exposure_for_ident
                .map(prompt_line_valid_cache_seed_exposure)
                .unwrap_or(0)
        });
    let entity_catalog_ids: IndexMap<(&str, &str), ()> = exposure_for_ident
        .map(exposure_qualified_catalog_ids)
        .unwrap_or_default();
    let surface_filter = exposure_for_ident.map(|e| &e.surface);
    let ident_meta = match (map_arc.as_deref(), exposure_for_ident) {
        (Some(_), Some(exposure)) => {
            Some(exposure.ident_metadata_for_exposure_entities(full_entities))
        }
        (Some(_), None) => {
            let mut acc = HashMap::new();
            for &e in full_entities {
                let cgs = resolve(e);
                acc.extend(crate::symbol_tuning::build_ident_metadata(cgs, &[e]));
            }
            Some(acc)
        }
        _ => None,
    };

    let mut session = TeachingSynthesisSession::new(
        line_valid_cache_seed,
        map_arc,
        ident_meta,
        surface_filter,
        entity_catalog_ids,
        fill_model,
    );

    let render_one = |session: &mut TeachingSynthesisSession<'_>,
                      cgs: &CGS,
                      ename: &str,
                      catalog_entry_id: &str,
                      teaching_blocks_out: &mut Vec<EntityTeachingBlock>,
                      model_out: &mut Vec<EntityTeachingPrompt>| {
        // Validation memo is per-entity: a failed receiver probe for one domain must not
        // stick in the shared session cache and block capability witnesses on another.
        session.line_valid_cache.clear();
        let mut field_gloss_accum = Vec::new();
        let session_map = session.map_arc.as_ref().map(|a| a.as_ref());
        let mut gloss_emit: Option<GlossScratch<'_>> =
            match (session_map, session.ident_meta.as_ref()) {
                (Some(m), Some(meta)) => Some(GlossScratch {
                    field_gloss: &mut field_gloss_accum,
                    state: &mut session.gloss_emit_state,
                    map: m,
                    meta,
                    catalog_entry_id,
                    entity: ename,
                    cgs,
                }),
                _ => None,
            };
        let block = collect_entity_teaching_block(
            cgs,
            ename,
            session.map_arc.as_ref(),
            session.ident_meta.as_ref(),
            session.collect_meta,
            &mut session.line_valid_cache,
            session.line_valid_cache_seed,
            &mut gloss_emit,
            session.surface_filter,
            Some(catalog_entry_id),
        );
        if block.teaching_rows.is_empty() {
            debug_assert!(
                    false,
                    "teaching block empty for entity {ename} — CGS::validate should have rejected this via cgs_expression_validate"
                );
            tracing::warn!(
                target: "plasm_core::prompt_render",
                entity = ename,
                "empty teaching block; schema should have failed CGS::validate"
            );
            return;
        }
        let mut seen_expr: HashSet<TeachingRowDedupeKey> = HashSet::new();
        let mut emitted_metas: Vec<TeachingLineMeta> = Vec::new();
        let mut kept_rows: Vec<EntityTeachingExprRow> = Vec::new();
        for row in block.teaching_rows {
            if seen_expr.insert(row.dedupe_key.clone()) {
                if session.collect_meta {
                    emitted_metas.push(row.meta.clone());
                }
                kept_rows.push(row);
            }
        }
        teaching_blocks_out.push(EntityTeachingBlock {
            heading: block.heading,
            field_gloss_rows: block.field_gloss_rows,
            teaching_rows: kept_rows,
        });
        if session.collect_meta {
            model_out.push(EntityTeachingPrompt {
                entity: ename.to_string(),
                lines: emitted_metas,
            });
        }
    };

    if let Some(blocks) = federated_blocks {
        let by_entry =
            federated_by_entry.expect("federated_by_entry required when federated_blocks is set");
        for (entry_id, ename) in blocks {
            if let Some(set) = emit_entity_keys {
                if !set.contains(&(entry_id.clone(), ename.to_string())) {
                    continue;
                }
            }
            let cgs = by_entry
                .get(entry_id.as_str())
                .copied()
                .expect("CGS for catalog entry id");
            render_one(
                &mut session,
                cgs,
                ename,
                entry_id.as_str(),
                teaching_blocks_out,
                model_out,
            );
        }
    } else {
        let block_iter: Vec<&str> = if let Some(e) = emit_entity_blocks {
            e.to_vec()
        } else {
            full_entities.to_vec()
        };
        for &ename in &block_iter {
            let cgs = resolve(ename);
            let catalog_entry_id_owned =
                catalog_entry_id_for_exposed_entity(&session.entity_catalog_ids, ename)
                    .map(str::to_string)
                    .or_else(|| cgs.entry_id.clone())
                    .unwrap_or_default();
            render_one(
                &mut session,
                cgs,
                ename,
                catalog_entry_id_owned.as_str(),
                teaching_blocks_out,
                model_out,
            );
        }
    }

    session.apply_opaque_alias_rewrites(teaching_blocks_out, model_out);
}

/// Per-entity many-shot examples using a single [`CGS`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_teaching_table(
    cgs: &CGS,
    full_entities: &[&str],
    map_arc: Option<std::sync::Arc<SymbolMap>>,
    teaching_blocks_out: &mut Vec<EntityTeachingBlock>,
    model_out: &mut Vec<EntityTeachingPrompt>,
    fill_model: bool,
    include_contract_preamble: bool,
    emit_entity_blocks: Option<&[&str]>,
) {
    render_teaching_table_resolved(
        |_| cgs,
        full_entities,
        map_arc,
        None,
        teaching_blocks_out,
        model_out,
        fill_model,
        include_contract_preamble,
        emit_entity_blocks,
        None,
        None,
        None::<&IndexMap<String, &CGS>>,
    );
}
