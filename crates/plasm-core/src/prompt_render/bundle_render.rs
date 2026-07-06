//! Public teaching bundle / TSV render entrypoints.

use std::sync::Arc;
use std::time::Instant;

use indexmap::IndexMap;

use crate::symbol_tuning::{
    symbol_map_cache_key_federated, symbol_map_cache_key_single_catalog, FocusSpec, SymbolMap,
    TeachingExposureSession,
};
use crate::CGS;

use super::teaching_gloss_emit::{render_teaching_table, render_teaching_table_resolved};
use super::tsv_emit::render_prompt_tsv_from_bundle;
use super::types::{
    PromptRenderMode, RenderConfig, TeachingPromptBundle, TeachingPromptModel, TeachingPromptSettings,
    TeachingPromptSource,
};

/// Render teaching table [`TeachingPromptBundle`] (structured teaching blocks + execution metadata).
pub fn render_teaching_bundle(
    cgs: &CGS,
    source: TeachingPromptSource<'_>,
    settings: TeachingPromptSettings<'_>,
) -> TeachingPromptBundle {
    let render_mode = if settings.symbolic {
        PromptRenderMode::Tsv
    } else {
        PromptRenderMode::Canonical
    };
    let include = settings.include_domain_execution_model;
    let cache = settings.symbol_map_cross_cache;
    match source {
        TeachingPromptSource::Catalog { focus } => render_teaching_prompt_bundle(
            cgs,
            RenderConfig {
                focus,
                render_mode,
                include_domain_execution_model: include,
                symbol_map_cross_cache: cache,
            },
        ),
        TeachingPromptSource::ExecuteWave { exposure } => {
            render_teaching_prompt_bundle_for_exposure(
                cgs,
                RenderConfig {
                    focus: FocusSpec::All,
                    render_mode,
                    include_domain_execution_model: include,
                    symbol_map_cross_cache: cache,
                },
                exposure,
                None,
            )
        }
    }
}

/// Render the teaching table as table-only teaching TSV (`plasm_expr` + `Meaning`); the grammar
/// contract lives statically in [`PLASM_TOOL_DESCRIPTION`], never interleaved here.
pub fn render_teaching_tsv(
    cgs: &CGS,
    source: TeachingPromptSource<'_>,
    settings: TeachingPromptSettings<'_>,
) -> String {
    match source {
        TeachingPromptSource::Catalog { focus } => {
            let render_mode = if settings.symbolic {
                PromptRenderMode::Tsv
            } else {
                PromptRenderMode::Canonical
            };
            render_prompt_tsv_with_config(
                cgs,
                RenderConfig {
                    focus,
                    render_mode,
                    include_domain_execution_model: settings.include_domain_execution_model,
                    symbol_map_cross_cache: settings.symbol_map_cross_cache,
                },
            )
        }
        TeachingPromptSource::ExecuteWave { exposure } => {
            let render_mode = if settings.symbolic {
                PromptRenderMode::Tsv
            } else {
                PromptRenderMode::Canonical
            };
            let cfg = RenderConfig {
                focus: FocusSpec::All,
                render_mode,
                include_domain_execution_model: settings.include_domain_execution_model,
                symbol_map_cross_cache: settings.symbol_map_cross_cache,
            };
            render_prompt_tsv_for_single_catalog_exposure(cgs, cfg, exposure)
        }
    }
}

/// [`render_teaching_prompt_bundle`] with [`RenderConfig::for_expression_surface_validation`].
///
/// Centralizes the config [`crate::cgs_expression_validate::validate_cgs_expression_surface`] must stay aligned with.
pub(crate) fn render_teaching_prompt_bundle_for_validation(cgs: &CGS) -> TeachingPromptBundle {
    render_teaching_prompt_bundle(cgs, RenderConfig::for_expression_surface_validation())
}

/// Render teaching table (many-shot examples) and structured execution metadata.
pub fn render_teaching_prompt_bundle(cgs: &CGS, config: RenderConfig<'_>) -> TeachingPromptBundle {
    let span = crate::spans::prompt_domain_bundle(
        &config.focus,
        config.uses_symbols(),
        config.include_domain_execution_model,
    );
    let _g = span.enter();

    if config.uses_symbols() {
        let exposure =
            crate::symbol_tuning::teaching_exposure_session_from_focus(cgs, config.focus);
        return render_teaching_prompt_bundle_for_exposure(cgs, config, &exposure, None);
    }

    let wall = Instant::now();
    let t0 = Instant::now();
    tracing::debug!("prompt: entity_slices_for_render");
    let (full_entities, dim_entities) =
        crate::symbol_tuning::entity_slices_for_render(cgs, config.focus);
    tracing::debug!(
        elapsed_ms = t0.elapsed().as_millis() as u64,
        full_entities = full_entities.len(),
        "render_teaching_prompt_bundle phase=entity_slices"
    );

    let t1 = Instant::now();
    tracing::debug!(
        full = full_entities.len(),
        dim = dim_entities.len(),
        "prompt: symbol_map_for_prompt"
    );
    let map_opt =
        crate::symbol_tuning::symbol_map_for_prompt(cgs, config.focus, config.uses_symbols());
    tracing::debug!(
        elapsed_ms = t1.elapsed().as_millis() as u64,
        has_symbol_map = map_opt.is_some(),
        "render_teaching_prompt_bundle phase=symbol_map"
    );

    if let Some(ref map) = map_opt {
        let t_leg = Instant::now();
        let legend = map.format_legend(cgs);
        tracing::debug!(
            elapsed_ms = t_leg.elapsed().as_millis() as u64,
            legend_chars = legend.len(),
            "render_teaching_prompt_bundle phase=format_legend"
        );
    }

    let t2 = Instant::now();
    tracing::debug!("prompt: render_teaching_table");
    let mut teaching_blocks = Vec::new();
    let mut entities_buf = Vec::new();
    let fill_model = config.include_domain_execution_model;
    render_teaching_table(
        cgs,
        &full_entities,
        map_opt.clone(),
        &mut teaching_blocks,
        &mut entities_buf,
        fill_model,
        false,
        None,
    );
    tracing::debug!(
        elapsed_ms = t2.elapsed().as_millis() as u64,
        teaching_entities = teaching_blocks.len(),
        "render_teaching_prompt_bundle phase=teaching_table"
    );
    let model = if fill_model {
        TeachingPromptModel {
            entities: entities_buf,
        }
    } else {
        TeachingPromptModel::default()
    };

    tracing::debug!(
        teaching_entities = teaching_blocks.len(),
        total_elapsed_ms = wall.elapsed().as_millis() as u64,
        "render_teaching_prompt_bundle done"
    );
    TeachingPromptBundle {
        teaching_blocks,
        model,
    }
}

/// Like [`render_teaching_prompt_bundle_for_exposure`], but each exposed entity is rendered against its
/// owning catalog graph (`by_entry` keyed by registry `entry_id`, aligned with
/// [`crate::symbol_tuning::TeachingExposureSession::entity_catalog_entry_ids`]).
pub fn render_teaching_prompt_bundle_for_exposure_federated<'b>(
    by_entry: &'b IndexMap<String, &'b CGS>,
    config: RenderConfig<'_>,
    exposure: &'b crate::symbol_tuning::TeachingExposureSession,
    emit_entity_blocks: Option<&[crate::symbol_tuning::ExposureEntityKey]>,
) -> TeachingPromptBundle {
    let span = crate::spans::prompt_domain_bundle_exposure_federated(
        emit_entity_blocks.is_some(),
        config.uses_symbols(),
    );
    let _g = span.enter();

    let cgs_layers: Vec<&CGS> = by_entry.values().copied().collect();
    let (full_entities, _dim_entities) =
        crate::symbol_tuning::entity_slices_for_render_federated(&cgs_layers, exposure);
    let map_opt: Option<Arc<SymbolMap>> = if config.uses_symbols() {
        let key = config
            .symbol_map_cross_cache
            .filter(|c| c.is_enabled())
            .map(|_| symbol_map_cache_key_federated(&cgs_layers, exposure));
        let (arc, lru_hit) = exposure.symbol_map_arc_cross(config.symbol_map_cross_cache, key);
        if let Some(hit) = lru_hit {
            tracing::Span::current().record("cache.hit", hit);
        }
        Some(arc)
    } else {
        None
    };

    let mut teaching_blocks = Vec::new();
    let mut entities_buf = Vec::new();
    let fill_model = config.include_domain_execution_model;

    let emit_set: Option<std::collections::BTreeSet<(String, String)>> =
        emit_entity_blocks.map(|keys| {
            keys.iter()
                .map(|k| (k.entry_id.clone(), k.entity.to_string()))
                .collect()
        });

    let federated_blocks: Vec<(String, &str)> = exposure
        .entities
        .iter()
        .zip(exposure.entity_catalog_entry_ids.iter())
        .map(|(entity, entry_id)| (entry_id.clone(), entity.as_str()))
        .collect();

    render_teaching_table_resolved(
        |ename| {
            let _ = ename;
            by_entry
                .values()
                .next()
                .expect("federated by_entry non-empty")
        },
        &full_entities,
        map_opt.clone(),
        Some(exposure),
        &mut teaching_blocks,
        &mut entities_buf,
        fill_model,
        false,
        None,
        emit_set.as_ref(),
        Some(&federated_blocks),
        Some(by_entry),
    );

    let model = if fill_model {
        TeachingPromptModel {
            entities: entities_buf,
        }
    } else {
        TeachingPromptModel::default()
    };

    TeachingPromptBundle {
        teaching_blocks,
        model,
    }
}

/// Teaching bundle using [`crate::symbol_tuning::TeachingExposureSession`] (monotonic `e#`/`m#`/`p#`).
/// When `emit_entity_blocks` is `Some`, only those entity blocks are rendered (incremental wave).
pub fn render_teaching_prompt_bundle_for_exposure(
    cgs: &CGS,
    config: RenderConfig<'_>,
    exposure: &crate::symbol_tuning::TeachingExposureSession,
    emit_entity_blocks: Option<&[&str]>,
) -> TeachingPromptBundle {
    let span = crate::spans::prompt_domain_bundle_exposure(
        emit_entity_blocks.is_some(),
        config.uses_symbols(),
    );
    let _g = span.enter();

    let refs: Vec<&str> = exposure.entities.iter().map(|s| s.as_str()).collect();
    let focus = crate::symbol_tuning::FocusSpec::SeedsExact(&refs);
    let (full_entities, _dim_entities) = crate::symbol_tuning::entity_slices_for_render(cgs, focus);
    let map_opt: Option<Arc<SymbolMap>> = if config.uses_symbols() {
        let key = config
            .symbol_map_cross_cache
            .filter(|c| c.is_enabled())
            .map(|_| symbol_map_cache_key_single_catalog(cgs, exposure));
        let (arc, lru_hit) = exposure.symbol_map_arc_cross(config.symbol_map_cross_cache, key);
        if let Some(hit) = lru_hit {
            tracing::Span::current().record("cache.hit", hit);
        }
        Some(arc)
    } else {
        None
    };

    let mut teaching_blocks = Vec::new();
    let mut entities_buf = Vec::new();
    let fill_model = config.include_domain_execution_model;
    render_teaching_table_resolved(
        |_| cgs,
        &full_entities,
        map_opt.clone(),
        Some(exposure),
        &mut teaching_blocks,
        &mut entities_buf,
        fill_model,
        false,
        emit_entity_blocks,
        None,
        None,
        None::<&IndexMap<String, &CGS>>,
    );
    let model = if fill_model {
        TeachingPromptModel {
            entities: entities_buf,
        }
    } else {
        TeachingPromptModel::default()
    };

    TeachingPromptBundle {
        teaching_blocks,
        model,
    }
}

/// Render the Plasm teaching surface for the given CGS and [`RenderConfig`].
///
/// The only prompt-facing teaching form is TSV; this wrapper is retained for older callers that
/// historically asked for the markdown teaching surface.
pub fn render_prompt_with_config(cgs: &CGS, config: RenderConfig<'_>) -> String {
    render_prompt_tsv_with_config(cgs, config)
}

/// TSV for a **single-catalog** [`TeachingExposureSession`]: one [`render_teaching_prompt_bundle_for_exposure`]
/// plus the session’s memoized [`SymbolMap`] / [`TeachingExposureSession::ident_metadata_for_exposure_entities`]
/// so bundle rows and TSV metadata cannot drift.
pub(crate) fn render_prompt_tsv_for_single_catalog_exposure(
    cgs: &CGS,
    config: RenderConfig<'_>,
    exposure: &TeachingExposureSession,
) -> String {
    let bundle = render_teaching_prompt_bundle_for_exposure(cgs, config, exposure, None);
    render_prompt_tsv_from_bundle(&bundle)
}

/// Render the teaching table teaching surface as TSV with stable, Plasm-expression-first rows.
///
/// Columns:
/// `plasm_expr`, `Meaning`
pub fn render_prompt_tsv_with_config(cgs: &CGS, config: RenderConfig<'_>) -> String {
    if config.uses_symbols() {
        let exposure =
            crate::symbol_tuning::teaching_exposure_session_from_focus(cgs, config.focus);
        return render_prompt_tsv_for_single_catalog_exposure(cgs, config, &exposure);
    }
    // Canonical names: 2-hop neighbourhood slice (not execute-parity [`TeachingExposureSession`]).
    let bundle = render_teaching_prompt_bundle(cgs, config);
    render_prompt_tsv_from_bundle(&bundle)
}
