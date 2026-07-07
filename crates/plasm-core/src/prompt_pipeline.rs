//! Owned configuration for teaching prompt rendering and symbol expansion — inject via
//! `plasm_runtime::ExecutionConfig` (see the `plasm-runtime` crate).
//!
//! Use [`PromptPipelineConfig::with_focus_spec`] / [`PromptPipelineConfig::render_prompt`] so
//! [`FocusSpec`](crate::symbol_tuning::FocusSpec) lifetimes stay correct for `Seeds` neighbourhoods.

use crate::prompt_render::{
    prompt_surface_stats, render_prompt_tsv_for_single_catalog_exposure,
    render_prompt_tsv_from_bundle, render_prompt_tsv_with_config, render_relation_edge_delta_rows,
    render_teaching_prompt_bundle_for_exposure,
    render_teaching_prompt_bundle_for_exposure_federated, PromptRenderMode, PromptSurfaceStats,
    RenderConfig, TSV_TEACHING_TABLE_HEADER,
};
use crate::schema::CGS;
use crate::symbol_tuning::{
    wire_surface_for_parse, wire_surface_for_teaching_session, ExposureCapabilityKey,
    ExposureEntityKey, ExposureSlotKey, FocusSpec, SymbolMap, SymbolMapCrossRequestCache,
    TeachingExposureSession,
};
use indexmap::IndexMap;
use std::sync::Arc;

/// Which entities drive teaching table slicing (mirrors [`FocusSpec`](crate::symbol_tuning::FocusSpec) but owned).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PromptFocus {
    #[default]
    All,
    Single(String),
    Seeds(Vec<String>),
}

/// Single configuration bundle for prompt rendering and `wire_surface_for_parse` alignment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptPipelineConfig {
    pub focus: PromptFocus,
    pub render_mode: PromptRenderMode,
    pub include_domain_execution_model: bool,
}

impl Default for PromptPipelineConfig {
    fn default() -> Self {
        Self {
            focus: PromptFocus::All,
            render_mode: PromptRenderMode::Tsv,
            include_domain_execution_model: true,
        }
    }
}

impl PromptPipelineConfig {
    fn render_surface(&self, cgs: &CGS, cfg: RenderConfig<'_>) -> String {
        render_prompt_tsv_with_config(cgs, cfg)
    }

    fn render_config_for_focus<'a>(&self, focus: FocusSpec<'a>) -> RenderConfig<'a> {
        RenderConfig {
            focus,
            render_mode: self.render_mode,
            include_domain_execution_model: self.include_domain_execution_model,
            symbol_map_cross_cache: None,
        }
    }

    fn with_entity_seed_focus<R>(
        &self,
        entities: &[String],
        f: impl FnOnce(FocusSpec<'_>) -> R,
    ) -> R {
        let refs: Vec<&str> = entities.iter().map(|s| s.as_str()).collect();
        f(FocusSpec::Seeds(&refs))
    }

    fn session_symbol_map(&self, exposure: &TeachingExposureSession) -> Option<Arc<SymbolMap>> {
        self.uses_symbols().then(|| exposure.symbol_map_arc())
    }

    /// CLI `--focus` → optional single-entity neighbourhood; otherwise full schema with opaque symbols when render mode uses them (eval / REPL default: see `--symbol-tuning`).
    pub fn for_cli_focus(focus: Option<&str>) -> Self {
        let mut s = Self::default();
        if let Some(f) = focus {
            s.focus = PromptFocus::Single(f.to_string());
        }
        s
    }

    /// Same as [`RenderConfig::for_eval_canonical`](crate::prompt_render::RenderConfig::for_eval_canonical): canonical teaching table names, no `e#`/`p#`/`m#`.
    pub fn for_canonical_no_symbols() -> Self {
        Self {
            focus: PromptFocus::All,
            render_mode: PromptRenderMode::Canonical,
            include_domain_execution_model: true,
        }
    }

    pub fn with_render_mode(mut self, render_mode: PromptRenderMode) -> Self {
        self.render_mode = render_mode;
        self
    }

    pub const fn uses_symbols(&self) -> bool {
        self.render_mode.uses_symbols()
    }

    /// Merge optional per-REPL / per-call focus override: when `Some`, wins over [`Self::focus`].
    pub fn with_focus_spec<R>(
        &self,
        override_focus: Option<&str>,
        f: impl FnOnce(FocusSpec<'_>) -> R,
    ) -> R {
        if let Some(foc) = override_focus {
            return f(FocusSpec::Single(foc));
        }
        match &self.focus {
            PromptFocus::All => f(FocusSpec::All),
            PromptFocus::Single(s) => f(FocusSpec::Single(s.as_str())),
            PromptFocus::Seeds(seeds) => {
                let refs: Vec<&str> = seeds.iter().map(|s| s.as_str()).collect();
                f(FocusSpec::Seeds(&refs))
            }
        }
    }

    /// teaching prompt string (same rules as [`RenderConfig::for_eval`](crate::prompt_render::RenderConfig::for_eval) + optional REPL focus override; TSV vs markdown follows [`Self::render_mode`]).
    pub fn render_prompt(&self, cgs: &CGS, repl_focus_override: Option<&str>) -> String {
        self.with_focus_spec(repl_focus_override, |focus| {
            self.render_surface(cgs, self.render_config_for_focus(focus))
        })
    }

    /// teaching prompt TSV table (expression-first grammar teaching surface).
    pub fn render_prompt_tsv(&self, cgs: &CGS, repl_focus_override: Option<&str>) -> String {
        self.with_focus_spec(repl_focus_override, |focus| {
            render_prompt_tsv_with_config(cgs, self.render_config_for_focus(focus))
        })
    }

    /// Execute-session prompt: always seed from `entities` (HTTP `POST /execute` body); ignores [`Self::focus`] for neighbourhood.
    pub fn render_prompt_for_session_entities(&self, cgs: &CGS, entities: &[String]) -> String {
        self.with_entity_seed_focus(entities, |focus| {
            self.render_surface(cgs, self.render_config_for_focus(focus))
        })
    }

    /// First teaching wave: **exact** seed entities + monotonic [`TeachingExposureSession`] symbols (no 2-hop union).
    pub fn render_teaching_first_wave_for_session(
        &self,
        cgs: &CGS,
        exposure: &TeachingExposureSession,
        symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    ) -> String {
        let cfg = RenderConfig {
            symbol_map_cross_cache,
            ..self.render_config_for_focus(FocusSpec::All)
        };
        render_prompt_tsv_for_single_catalog_exposure(cgs, cfg, exposure)
    }

    /// First teaching wave for a **federated** session: one [`CGS`] per registry `entry_id`.
    pub fn render_teaching_first_wave_for_session_federated<'b>(
        &self,
        by_entry: &'b IndexMap<String, &'b CGS>,
        exposure: &'b TeachingExposureSession,
        symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    ) -> String {
        let cfg = RenderConfig {
            symbol_map_cross_cache,
            ..self.render_config_for_focus(FocusSpec::All)
        };
        let bundle =
            render_teaching_prompt_bundle_for_exposure_federated(by_entry, cfg, exposure, None);
        render_prompt_tsv_from_bundle(&bundle)
    }

    /// Incremental teaching table: append table blocks for `new_entity_names` only (symbols stable vs `exposure`).
    pub fn render_teaching_exposure_delta(
        &self,
        cgs: &CGS,
        exposure: &TeachingExposureSession,
        new_entity_names: &[&str],
        symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    ) -> String {
        self.render_teaching_exposure_delta_with_edges(
            cgs,
            exposure,
            new_entity_names,
            &[],
            symbol_map_cross_cache,
        )
    }

    /// Like [`Self::render_teaching_exposure_delta`], prepending relation-hop rows unlocked this wave.
    pub fn render_teaching_exposure_delta_with_edges(
        &self,
        cgs: &CGS,
        exposure: &TeachingExposureSession,
        new_entity_names: &[&str],
        new_relation_slots: &[ExposureSlotKey],
        symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    ) -> String {
        let entity_delta = {
            let cfg = RenderConfig {
                symbol_map_cross_cache,
                ..self.render_config_for_focus(FocusSpec::All)
            };
            let bundle = render_teaching_prompt_bundle_for_exposure(
                cgs,
                cfg,
                exposure,
                Some(new_entity_names),
            );
            render_prompt_tsv_from_bundle(&bundle)
        };
        splice_relation_edge_rows_into_delta(
            exposure,
            new_relation_slots,
            self.session_symbol_map(exposure).as_ref(),
            entity_delta,
        )
    }

    /// Incremental teaching table for federated sessions (per-entity owning graph).
    pub fn render_teaching_exposure_delta_federated<'b>(
        &self,
        by_entry: &'b IndexMap<String, &'b CGS>,
        exposure: &'b TeachingExposureSession,
        new_entities: &[ExposureEntityKey],
        symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    ) -> String {
        self.render_teaching_exposure_delta_federated_with_edges(
            by_entry,
            exposure,
            new_entities,
            &[],
            symbol_map_cross_cache,
        )
    }

    /// Like [`Self::render_teaching_exposure_delta_federated`], prepending relation-hop rows unlocked this wave.
    pub fn render_teaching_exposure_delta_federated_with_edges<'b>(
        &self,
        by_entry: &'b IndexMap<String, &'b CGS>,
        exposure: &'b TeachingExposureSession,
        new_entities: &[ExposureEntityKey],
        new_relation_slots: &[ExposureSlotKey],
        symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    ) -> String {
        let cfg = RenderConfig {
            symbol_map_cross_cache,
            ..self.render_config_for_focus(FocusSpec::All)
        };
        let bundle = render_teaching_prompt_bundle_for_exposure_federated(
            by_entry,
            cfg,
            exposure,
            Some(new_entities),
        );
        let entity_delta = render_prompt_tsv_from_bundle(&bundle);
        splice_relation_edge_rows_into_delta(
            exposure,
            new_relation_slots,
            self.session_symbol_map(exposure).as_ref(),
            entity_delta,
        )
    }

    /// Compact teaching TSV when ranked replay adds mutators without new entities.
    pub fn render_teaching_new_capabilities_delta(
        &self,
        cgs: &CGS,
        exposure: &TeachingExposureSession,
        new_caps: &std::collections::BTreeSet<ExposureCapabilityKey>,
        symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    ) -> String {
        let cfg = RenderConfig {
            symbol_map_cross_cache,
            ..self.render_config_for_focus(FocusSpec::All)
        };
        crate::prompt_render::render_teaching_new_capabilities_delta_tsv(
            cgs, cfg, exposure, new_caps,
        )
    }

    /// Federated variant of [`Self::render_teaching_new_capabilities_delta`].
    pub fn render_teaching_new_capabilities_delta_federated<'b>(
        &self,
        by_entry: &'b IndexMap<String, &'b CGS>,
        exposure: &TeachingExposureSession,
        new_caps: &std::collections::BTreeSet<ExposureCapabilityKey>,
        symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    ) -> String {
        let cfg = RenderConfig {
            symbol_map_cross_cache,
            ..self.render_config_for_focus(FocusSpec::All)
        };
        crate::prompt_render::render_teaching_new_capabilities_delta_tsv_federated(
            by_entry, cfg, exposure, new_caps,
        )
    }

    pub fn prompt_surface_stats(
        &self,
        cgs: &CGS,
        repl_focus_override: Option<&str>,
        prompt: &str,
    ) -> PromptSurfaceStats {
        self.with_focus_spec(repl_focus_override, |focus| {
            prompt_surface_stats(cgs, self.render_config_for_focus(focus), prompt)
        })
    }

    pub fn prompt_surface_stats_for_session_entities(
        &self,
        cgs: &CGS,
        entities: &[String],
        prompt: &str,
    ) -> PromptSurfaceStats {
        self.with_entity_seed_focus(entities, |focus| {
            prompt_surface_stats(cgs, self.render_config_for_focus(focus), prompt)
        })
    }

    /// Wire-surface render for parse (REPL / eval); optional override wins over [`Self::focus`].
    pub fn wire_surface_line(
        &self,
        line: &str,
        cgs: &CGS,
        repl_focus_override: Option<&str>,
    ) -> String {
        self.with_focus_spec(repl_focus_override, |focus| {
            wire_surface_for_parse(line, cgs, focus, self.uses_symbols())
        })
    }

    /// Wire-surface render using session entity seeds (HTTP execute run line).
    pub fn wire_surface_for_session_entities(
        &self,
        line: &str,
        cgs: &CGS,
        entities: &[String],
    ) -> String {
        self.with_entity_seed_focus(entities, |focus| {
            wire_surface_for_parse(line, cgs, focus, self.uses_symbols())
        })
    }

    /// Wire-surface render using monotonic session symbols ([`TeachingExposureSession`]) when present.
    pub fn wire_surface_for_session_with_optional_exposure(
        &self,
        line: &str,
        cgs: &CGS,
        entities: &[String],
        exposure: Option<&TeachingExposureSession>,
    ) -> String {
        if let Some(exp) = exposure {
            wire_surface_for_teaching_session(line, exp, self.uses_symbols())
        } else {
            self.wire_surface_for_session_entities(line, cgs, entities)
        }
    }
}

fn splice_relation_edge_rows_into_delta(
    exposure: &TeachingExposureSession,
    new_relation_slots: &[ExposureSlotKey],
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
    entity_delta: String,
) -> String {
    if new_relation_slots.is_empty() {
        return entity_delta;
    }
    let edge_body = render_relation_edge_delta_rows(exposure, new_relation_slots, map_arc);
    if edge_body.is_empty() {
        return entity_delta;
    }
    if let Some(rest) = entity_delta.strip_prefix(TSV_TEACHING_TABLE_HEADER) {
        format!("{TSV_TEACHING_TABLE_HEADER}{edge_body}{rest}")
    } else {
        format!("{TSV_TEACHING_TABLE_HEADER}{edge_body}{entity_delta}")
    }
}
