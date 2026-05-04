//! Owned configuration for DOMAIN prompt rendering and symbol expansion — inject via
//! `plasm_runtime::ExecutionConfig` (see the `plasm-runtime` crate).
//!
//! Use [`PromptPipelineConfig::with_focus_spec`] / [`PromptPipelineConfig::render_prompt`] so
//! [`FocusSpec`](crate::symbol_tuning::FocusSpec) lifetimes stay correct for `Seeds` neighbourhoods.

use crate::prompt_render::{
    prompt_surface_stats, render_domain_prompt_bundle_for_exposure,
    render_domain_prompt_bundle_for_exposure_federated, render_prompt_surface_from_bundle,
    render_prompt_tsv_for_single_catalog_exposure, render_prompt_tsv_with_config,
    DomainPromptBundle, DomainWaveSurface, PromptRenderMode, PromptSurfaceStats, RenderConfig,
};
use crate::schema::CGS;
use crate::symbol_tuning::{
    expand_expr_for_domain_session, expand_expr_for_parse, DomainExposureSession, FocusSpec,
    IdentMetaKey, IdentMetadata, SymbolMap, SymbolMapCrossRequestCache,
};
use indexmap::IndexMap;
use std::collections::HashMap;
use std::sync::Arc;

/// Which entities drive DOMAIN slicing (mirrors [`FocusSpec`](crate::symbol_tuning::FocusSpec) but owned).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PromptFocus {
    #[default]
    All,
    Single(String),
    Seeds(Vec<String>),
}

/// Single configuration bundle for prompt rendering and `expand_expr_for_parse` alignment.
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

type IdentMetadataMap = HashMap<IdentMetaKey, IdentMetadata>;

struct FederatedExposureResolver<'exposure, 'cgs> {
    by_entity: HashMap<&'exposure str, &'cgs CGS>,
}

impl<'exposure, 'cgs> FederatedExposureResolver<'exposure, 'cgs> {
    fn new(
        by_entry: &'cgs IndexMap<String, &'cgs CGS>,
        exposure: &'exposure DomainExposureSession,
    ) -> Self {
        let by_entity = exposure
            .entities
            .iter()
            .zip(exposure.entity_catalog_entry_ids.iter())
            .map(|(entity, entry_id)| {
                let cgs = by_entry
                    .get(entry_id)
                    .copied()
                    .expect("CGS for catalog entry id");
                (entity.as_str(), cgs)
            })
            .collect();
        Self { by_entity }
    }

    fn resolve(&self, entity: &str) -> &'cgs CGS {
        self.by_entity
            .get(entity)
            .copied()
            .expect("entity must appear in exposure session")
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

    fn session_symbol_map(&self, exposure: &DomainExposureSession) -> Option<Arc<SymbolMap>> {
        self.uses_symbols().then(|| exposure.symbol_map_arc())
    }

    fn build_ident_meta_for_entities<'b, F>(
        &self,
        entities: &[&str],
        mut resolve_cgs: F,
    ) -> Option<IdentMetadataMap>
    where
        F: FnMut(&str) -> &'b CGS,
    {
        if !self.uses_symbols() {
            return None;
        }
        let mut acc = HashMap::new();
        for &entity in entities {
            acc.extend(crate::symbol_tuning::build_ident_metadata(
                resolve_cgs(entity),
                &[entity],
            ));
        }
        Some(acc)
    }

    fn render_domain_bundle_surface<'b, F>(
        &self,
        bundle: &DomainPromptBundle,
        full_entities: &[&str],
        exposure: &DomainExposureSession,
        ident_meta: Option<&IdentMetadataMap>,
        resolve: F,
        wave_surface: DomainWaveSurface,
    ) -> String
    where
        F: FnMut(&str) -> &'b CGS,
    {
        let symbol_map = self.session_symbol_map(exposure);
        render_prompt_surface_from_bundle(
            bundle,
            self.uses_symbols(),
            full_entities,
            symbol_map.as_deref(),
            ident_meta,
            resolve,
            wave_surface,
        )
    }

    /// CLI `--focus` → optional single-entity neighbourhood; otherwise full schema with opaque symbols when render mode uses them (eval / REPL default: see `--symbol-tuning`).
    pub fn for_cli_focus(focus: Option<&str>) -> Self {
        let mut s = Self::default();
        if let Some(f) = focus {
            s.focus = PromptFocus::Single(f.to_string());
        }
        s
    }

    /// Same as [`RenderConfig::for_eval_canonical`](crate::prompt_render::RenderConfig::for_eval_canonical): canonical DOMAIN names, no `e#`/`p#`/`m#`.
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

    /// DOMAIN prompt string (same rules as [`RenderConfig::for_eval`](crate::prompt_render::RenderConfig::for_eval) + optional REPL focus override; TSV vs markdown follows [`Self::render_mode`]).
    pub fn render_prompt(&self, cgs: &CGS, repl_focus_override: Option<&str>) -> String {
        self.with_focus_spec(repl_focus_override, |focus| {
            self.render_surface(cgs, self.render_config_for_focus(focus))
        })
    }

    /// DOMAIN prompt TSV table (expression-first grammar teaching surface).
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

    /// First DOMAIN wave: **exact** seed entities + monotonic [`DomainExposureSession`] symbols (no 2-hop union).
    pub fn render_domain_first_wave_for_session(
        &self,
        cgs: &CGS,
        exposure: &DomainExposureSession,
        symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    ) -> String {
        let cfg = RenderConfig {
            symbol_map_cross_cache,
            ..self.render_config_for_focus(FocusSpec::All)
        };
        render_prompt_tsv_for_single_catalog_exposure(cgs, cfg, exposure)
    }

    /// First DOMAIN wave for a **federated** session: one [`CGS`] per registry `entry_id`.
    pub fn render_domain_first_wave_for_session_federated<'b>(
        &self,
        by_entry: &'b IndexMap<String, &'b CGS>,
        exposure: &'b DomainExposureSession,
        symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    ) -> String {
        let cfg = RenderConfig {
            symbol_map_cross_cache,
            ..self.render_config_for_focus(FocusSpec::All)
        };
        let bundle =
            render_domain_prompt_bundle_for_exposure_federated(by_entry, cfg, exposure, None);
        let full_entities: Vec<&str> = exposure.entities.iter().map(|s| s.as_str()).collect();
        let resolver = FederatedExposureResolver::new(by_entry, exposure);
        let ident_meta =
            self.build_ident_meta_for_entities(&full_entities, |entity| resolver.resolve(entity));
        self.render_domain_bundle_surface(
            &bundle,
            &full_entities,
            exposure,
            ident_meta.as_ref(),
            |entity| resolver.resolve(entity),
            DomainWaveSurface::InitialTeaching,
        )
    }

    /// Incremental DOMAIN: append table blocks for `new_entity_names` only (symbols stable vs `exposure`).
    pub fn render_domain_exposure_delta(
        &self,
        cgs: &CGS,
        exposure: &DomainExposureSession,
        new_entity_names: &[&str],
        symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    ) -> String {
        let cfg = RenderConfig {
            symbol_map_cross_cache,
            ..self.render_config_for_focus(FocusSpec::All)
        };
        let bundle =
            render_domain_prompt_bundle_for_exposure(cgs, cfg, exposure, Some(new_entity_names));
        let ident_meta = self.build_ident_meta_for_entities(new_entity_names, |_| cgs);
        self.render_domain_bundle_surface(
            &bundle,
            new_entity_names,
            exposure,
            ident_meta.as_ref(),
            |_| cgs,
            DomainWaveSurface::AdditiveWave,
        )
    }

    /// Incremental DOMAIN for federated sessions (per-entity owning graph).
    pub fn render_domain_exposure_delta_federated<'b>(
        &self,
        by_entry: &'b IndexMap<String, &'b CGS>,
        exposure: &'b DomainExposureSession,
        new_entity_names: &[&str],
        symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    ) -> String {
        let cfg = RenderConfig {
            symbol_map_cross_cache,
            ..self.render_config_for_focus(FocusSpec::All)
        };
        let bundle = render_domain_prompt_bundle_for_exposure_federated(
            by_entry,
            cfg,
            exposure,
            Some(new_entity_names),
        );
        let resolver = FederatedExposureResolver::new(by_entry, exposure);
        let ident_meta =
            self.build_ident_meta_for_entities(new_entity_names, |entity| resolver.resolve(entity));
        self.render_domain_bundle_surface(
            &bundle,
            new_entity_names,
            exposure,
            ident_meta.as_ref(),
            |entity| resolver.resolve(entity),
            DomainWaveSurface::AdditiveWave,
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

    /// Expand symbolic tokens before parse (REPL / eval); optional override wins over [`Self::focus`].
    pub fn expand_expr_line(
        &self,
        line: &str,
        cgs: &CGS,
        repl_focus_override: Option<&str>,
    ) -> String {
        self.with_focus_spec(repl_focus_override, |focus| {
            expand_expr_for_parse(line, cgs, focus, self.uses_symbols())
        })
    }

    /// Expand using session entity seeds (HTTP execute run line).
    pub fn expand_expr_for_session_entities(
        &self,
        line: &str,
        cgs: &CGS,
        entities: &[String],
    ) -> String {
        self.with_entity_seed_focus(entities, |focus| {
            expand_expr_for_parse(line, cgs, focus, self.uses_symbols())
        })
    }

    /// Expand using monotonic session symbols ([`DomainExposureSession`]) when present.
    pub fn expand_expr_for_session_with_optional_exposure(
        &self,
        line: &str,
        cgs: &CGS,
        entities: &[String],
        exposure: Option<&DomainExposureSession>,
    ) -> String {
        if let Some(exp) = exposure {
            expand_expr_for_domain_session(line, exp, self.uses_symbols())
        } else {
            self.expand_expr_for_session_entities(line, cgs, entities)
        }
    }
}
