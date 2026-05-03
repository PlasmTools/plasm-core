//! CGS prompt renderer — TSV **Plasm** many-shot examples (`plasm_expr` + `Meaning`), with `p#`
//! glosses emitted before first use in symbolic modes.
//!
//! This is the prompt string for `plasm-eval` / BAML, REPL startup / `:schema`, HTTP execute session `prompt`, and MCP DOMAIN after `plasm_context`.
//! Build via [`render_prompt_with_config`] or [`render_prompt_tsv_with_config`]. Both now emit the
//! TSV teaching surface. [`RenderConfig::for_eval`] defaults to [`PromptRenderMode::Tsv`] (`e#` /
//! `m#` / `p#`); legacy compact/canonical modes affect symbol naming only, not the output format.
//! The prompt opens with a compact pseudo-EBNF **Plasm language contract** (see
//! [`DOMAIN_VALID_EXPR_MARKER`]) defining the stable syntax surface (`{ }`, `.`, `[ ]`, assignments,
//! final roots, `;;`, etc.). Catalogue-specific DOMAIN rows then act as many-shot semantic
//! instantiations: they teach which concrete `e#` / `m#` / `p#` symbols, fields, methods, scoped
//! filters, and relations are valid for this catalogue wave. The `~` search form is mentioned in the
//! contract **only** when at least one entity in the rendered slice has a Search capability (matching
//! per-entity DOMAIN rows). A mandatory tagged `<<TAG` heredoc bullet appears when the slice includes
//! any non-`short` [`StringSemantics`](crate::StringSemantics).
//!
//! **DOMAIN** is **per-entity blocks** of **valid Plasm expressions only** (CGS-validated before emit).
//! Each block starts with `e#  ;;  …` (entity semantic gloss), then **four-space** example lines
//! (`p#` gloss on the line before first use: `type · description` from CGS). Model output must be those expression shapes—not prose.
//! Use [`RenderConfig::focus`] to subset entities.
//!
//! **Relations** lines teach `Get(id).relation` when that path **parses and type-checks**. For terminal relation chains, the example
//! line already ends with `;;  => e#`, so the redundant standalone `p#  ;;  => e# · …` gloss line
//! before it is omitted (see [`skip_redundant_terminal_relation_sym_gloss`]). For cardinality-many
//! edges with `materialize` (`from_parent_get`, `query_scoped`, …) the IR is [`Expr::Chain`](crate::Expr);
//! many-relations without materialization **fail parse** and are omitted from DOMAIN.
//!
//! **Validation:** every **single-expression** DOMAIN example (after stripping human-only `  ;;  …` suffixes,
//! legacy `  =>  ` before `;;`, and legacy relation ` -> …` before `;;`) is checked with **parse →
//! [`normalize_expr_query_capabilities`](crate::normalize_expr_query_capabilities) → [`type_check_expr`](crate::type_check_expr)** before emission.
//! Zero-arity pipeline methods emit **one** `…()` expression per line (each line is fully validated).
//!
//! **Load-time invariant:** [`CGS::validate`](crate::schema::CGS::validate) runs [`crate::cgs_expression_validate`],
//! which requires every non-abstract entity to produce at least one such line via the same synthesis as
//! [`collect_entity_domain_block`] (opaque symbol map in **compact**/**tsv** modes, matching eval / REPL) — see [`domain_example_line_count`].

use crate::{
    cross_entity::{choose_strategy, extract_cross_entity_predicates},
    schema::{
        capability_is_zero_arity_invoke, capability_method_label_kebab, Cardinality, EntityDef,
        InputFieldSchema, RelationMaterialization, RelationSchema,
    },
    symbol_tuning::{
        symbol_map_cache_key_federated, symbol_map_cache_key_single_catalog, DomainExposureSession,
        FocusSpec, IdentMetaKey, IdentMetadata, SymbolMap, SymbolMapCrossRequestCache,
    },
    CapabilityKind, CapabilityName, EntityName, Expr, FieldType, InputType, ParameterRole,
    ValueWireFormat, CGS,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::sync::Arc;
use std::time::Instant;

/// Prompt rendering options (entity subset + [`PromptRenderMode`] for opaque `e#`/`m#`/`p#` vs canonical names).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PromptRenderMode {
    Canonical,
    Compact,
    #[default]
    Tsv,
}

impl PromptRenderMode {
    pub const USER_FACING_VALUES: [&'static str; 1] = ["tsv"];

    pub fn parse_user_facing(raw: &str) -> Option<Self> {
        match raw {
            "verbose" | "compact" => Some(Self::Tsv),
            "tsv" => Some(Self::Tsv),
            _ => None,
        }
    }

    pub fn parse_user_facing_or_default(raw: &str) -> Self {
        Self::parse_user_facing(raw).unwrap_or_default()
    }

    pub const fn user_facing_name(self) -> Option<&'static str> {
        match self {
            Self::Canonical => None,
            Self::Compact => Some("compact"),
            Self::Tsv => Some("tsv"),
        }
    }

    pub const fn uses_symbols(self) -> bool {
        !matches!(self, Self::Canonical)
    }

    pub const fn is_tsv(self) -> bool {
        matches!(self, Self::Tsv)
    }

    pub const fn markdown_fence_info_string(self) -> &'static str {
        "tsv"
    }
}

/// TSV DOMAIN: first line of the teaching table (`plasm_expr` and `Meaning` columns) including the
/// trailing newline, matching [`render_prompt_tsv_from_bundle`].
pub const TSV_DOMAIN_TABLE_HEADER: &str = "plasm_expr\tMeaning\n";

/// Split a TSV DOMAIN string into the optional comment-prefixed Plasm language **contract** block
/// (InitialTeaching) and the **table body** (from [`TSV_DOMAIN_TABLE_HEADER`] through end).
///
/// Additive TSV (delta waves) has no contract prefix: returns [`None`] and the full input as body.
/// If the `plasm_expr`/`Meaning` header is missing, returns [`None`] and the full input (pass-through).
pub fn split_tsv_domain_contract_and_table(domain_tsv: &str) -> (Option<String>, String) {
    if let Some(idx) = domain_tsv.find(TSV_DOMAIN_TABLE_HEADER) {
        let prefix = domain_tsv[..idx].trim_end();
        let contract = if prefix.is_empty() {
            None
        } else {
            Some(prefix.to_string())
        };
        return (contract, domain_tsv[idx..].to_string());
    }
    (None, domain_tsv.to_string())
}

/// Whether DOMAIN **TSV** output includes the global Plasm contract comment block.
///
/// Execute-session **additive** waves ([`crate::prompt_pipeline::PromptPipelineConfig::render_domain_exposure_delta`])
/// must use [`Self::AdditiveWave`] so we do not re-broadcast grammar boilerplate on every capability append.
/// Full-schema / first-wave teaching uses [`Self::InitialTeaching`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DomainWaveSurface {
    /// First wave or greenfield teaching: emit global contract as leading TSV `#` comments.
    InitialTeaching,
    /// Subsequent waves: new entity rows only; keep `plasm_expr` / `Meaning` header for a self-describing fragment.
    AdditiveWave,
}

#[derive(Clone, Copy, Debug)]
pub struct RenderConfig<'a> {
    /// Subset of entities for DOMAIN / symbol map (see [`FocusSpec`]).
    pub focus: FocusSpec<'a>,
    /// Prompt render surface: canonical, verbose symbolic, compact symbolic, or TSV symbolic.
    pub render_mode: PromptRenderMode,
    /// When true, [`render_domain_prompt_bundle`] fills [`DomainPromptModel`] (cross-entity strategy, relation materialization).
    /// Reserved for product policy to omit execution metadata later.
    pub include_domain_execution_model: bool,
    /// When set (same LRU as execute-session expansion), symbolic DOMAIN renders reuse [`SymbolMap`] snapshots across invocations.
    pub symbol_map_cross_cache: Option<&'a SymbolMapCrossRequestCache>,
}

impl<'a> Default for RenderConfig<'a> {
    fn default() -> Self {
        Self {
            focus: FocusSpec::All,
            render_mode: PromptRenderMode::Tsv,
            include_domain_execution_model: true,
            symbol_map_cross_cache: None,
        }
    }
}

impl<'a> RenderConfig<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Same knob as `plasm-eval --focus` (REPL / HTTP parity). Uses default symbolic [`PromptRenderMode::Tsv`]; override with [`Self::with_render_mode`].
    pub fn for_eval(focus: Option<&'a str>) -> Self {
        Self {
            focus: FocusSpec::from_optional(focus),
            render_mode: PromptRenderMode::Tsv,
            include_domain_execution_model: true,
            symbol_map_cross_cache: None,
        }
    }

    /// Several seed entities (union of 2-hop neighbourhoods), same CGS.
    pub fn for_eval_seeds(seeds: &'a [&'a str]) -> Self {
        Self {
            focus: FocusSpec::Seeds(seeds),
            render_mode: PromptRenderMode::Tsv,
            include_domain_execution_model: true,
            symbol_map_cross_cache: None,
        }
    }

    /// Canonical entity/method/field names in DOMAIN (for tests / debugging).
    pub fn for_eval_canonical(focus: Option<&'a str>) -> Self {
        Self {
            focus: FocusSpec::from_optional(focus),
            render_mode: PromptRenderMode::Canonical,
            include_domain_execution_model: true,
            symbol_map_cross_cache: None,
        }
    }

    pub fn with_render_mode(mut self, render_mode: PromptRenderMode) -> Self {
        self.render_mode = render_mode;
        self
    }

    pub fn with_symbol_map_cross_cache(
        mut self,
        cache: Option<&'a SymbolMapCrossRequestCache>,
    ) -> Self {
        self.symbol_map_cross_cache = cache;
        self
    }

    pub const fn uses_symbols(&self) -> bool {
        self.render_mode.uses_symbols()
    }
}

/// DOMAIN prompt text plus structured execution metadata for tooling / policy (not appended to `;;`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainPromptBundle {
    pub prompt: String,
    pub model: DomainPromptModel,
}

/// Per-entity DOMAIN lines with execution hints parallel to the rendered prompt strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DomainPromptModel {
    pub entities: Vec<EntityDomainPrompt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityDomainPrompt {
    pub entity: String,
    pub lines: Vec<DomainLineMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainLineMeta {
    /// Expression only (no `;;` hints), after the same stripping/expansion as validation.
    pub expression: String,
    pub kind: DomainLineKind,
    /// When this line teaches a concrete CGS capability (get / query / search / method), its id.
    /// Omitted for relation-navigation lines and other synthesized lines without a single owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_capability: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_entity: Option<Vec<CrossEntityPlanMeta>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation_materialization: Option<RelationMaterializationSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainLineKind {
    Get,
    Query,
    Search,
    RelationNav,
    Method,
    Other,
}

impl DomainLineKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            DomainLineKind::Get => "get",
            DomainLineKind::Query => "query",
            DomainLineKind::Search => "search",
            DomainLineKind::RelationNav => "relation_nav",
            DomainLineKind::Method => "method",
            DomainLineKind::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossEntityPlanMeta {
    pub ref_field: String,
    pub foreign_entity: String,
    pub strategy: CrossEntityStrategyKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossEntityStrategyKind {
    PushLeft,
    PullRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationMaterializationSummary {
    Unavailable,
    FromParentGet,
    QueryScoped,
    QueryScopedBindings,
}

impl From<&RelationMaterialization> for RelationMaterializationSummary {
    fn from(m: &RelationMaterialization) -> Self {
        match m {
            RelationMaterialization::Unavailable => Self::Unavailable,
            RelationMaterialization::FromParentGet { .. } => Self::FromParentGet,
            RelationMaterialization::QueryScoped { .. } => Self::QueryScoped,
            RelationMaterialization::QueryScopedBindings { .. } => Self::QueryScopedBindings,
        }
    }
}

/// Render DOMAIN (many-shot examples) and structured execution metadata.
pub fn render_domain_prompt_bundle(cgs: &CGS, config: RenderConfig<'_>) -> DomainPromptBundle {
    let span = crate::spans::prompt_domain_bundle(
        &config.focus,
        config.uses_symbols(),
        config.include_domain_execution_model,
    );
    let _g = span.enter();

    if config.uses_symbols() {
        let exposure = crate::symbol_tuning::domain_exposure_session_from_focus(cgs, config.focus);
        return render_domain_prompt_bundle_for_exposure(cgs, config, &exposure, None);
    }

    let wall = Instant::now();
    let t0 = Instant::now();
    tracing::debug!("prompt: entity_slices_for_render");
    let (full_entities, dim_entities) =
        crate::symbol_tuning::entity_slices_for_render(cgs, config.focus);
    tracing::debug!(
        elapsed_ms = t0.elapsed().as_millis() as u64,
        full_entities = full_entities.len(),
        "render_domain_prompt_bundle phase=entity_slices"
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
        "render_domain_prompt_bundle phase=symbol_map"
    );

    let mut out = String::with_capacity(4096);
    if let Some(ref map) = map_opt {
        let t_leg = Instant::now();
        let legend = map.format_legend(cgs);
        if !legend.is_empty() {
            tracing::debug!("prompt: format_legend (SYMBOL MAP)");
            out.push_str(&legend);
            out.push('\n');
        }
        tracing::debug!(
            elapsed_ms = t_leg.elapsed().as_millis() as u64,
            legend_chars = legend.len(),
            "render_domain_prompt_bundle phase=format_legend"
        );
    }

    let t2 = Instant::now();
    tracing::debug!("prompt: render_domain_table");
    let mut entities_buf = Vec::new();
    let fill_model = config.include_domain_execution_model;
    render_domain_table(
        cgs,
        &full_entities,
        map_opt.as_ref(),
        &mut out,
        &mut entities_buf,
        fill_model,
        false,
        None,
    );
    tracing::debug!(
        elapsed_ms = t2.elapsed().as_millis() as u64,
        out_chars = out.len(),
        "render_domain_prompt_bundle phase=domain_table"
    );
    let model = if fill_model {
        DomainPromptModel {
            entities: entities_buf,
        }
    } else {
        DomainPromptModel::default()
    };

    tracing::debug!(
        chars = out.len(),
        total_elapsed_ms = wall.elapsed().as_millis() as u64,
        "render_domain_prompt_bundle done"
    );
    DomainPromptBundle { prompt: out, model }
}

/// Like [`render_domain_prompt_bundle_for_exposure`], but each exposed entity is rendered against its
/// owning catalog graph (`by_entry` keyed by registry `entry_id`, aligned with
/// [`crate::symbol_tuning::DomainExposureSession::entity_catalog_entry_ids`]).
pub fn render_domain_prompt_bundle_for_exposure_federated<'b>(
    by_entry: &'b IndexMap<String, &'b CGS>,
    config: RenderConfig<'_>,
    exposure: &'b crate::symbol_tuning::DomainExposureSession,
    emit_entity_blocks: Option<&[&str]>,
) -> DomainPromptBundle {
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

    let mut out = String::with_capacity(4096);
    if let Some(ref map) = map_opt {
        if let Some(legend_cgs) = cgs_layers.first().copied() {
            let legend = map.format_legend(legend_cgs);
            if !legend.is_empty() {
                out.push_str(&legend);
                out.push('\n');
            }
        }
    }

    let mut entities_buf = Vec::new();
    let fill_model = config.include_domain_execution_model;
    let mut entity_to_entry: HashMap<&str, &str> = HashMap::new();
    for (e, id) in exposure
        .entities
        .iter()
        .zip(exposure.entity_catalog_entry_ids.iter())
    {
        entity_to_entry.entry(e.as_str()).or_insert(id.as_str());
    }
    let resolve = |ename: &str| -> &CGS {
        let eid = entity_to_entry
            .get(ename)
            .expect("entity must appear in exposure session");
        by_entry
            .get(*eid)
            .copied()
            .expect("CGS for catalog entry id")
    };
    render_domain_table_resolved(
        resolve,
        &full_entities,
        map_opt.as_deref(),
        Some(exposure),
        &mut out,
        &mut entities_buf,
        fill_model,
        false,
        emit_entity_blocks,
    );
    let model = if fill_model {
        DomainPromptModel {
            entities: entities_buf,
        }
    } else {
        DomainPromptModel::default()
    };

    DomainPromptBundle { prompt: out, model }
}

/// DOMAIN bundle using [`crate::symbol_tuning::DomainExposureSession`] (monotonic `e#`/`m#`/`p#`).
/// When `emit_entity_blocks` is `Some`, only those entity blocks are rendered (incremental wave).
pub fn render_domain_prompt_bundle_for_exposure(
    cgs: &CGS,
    config: RenderConfig<'_>,
    exposure: &crate::symbol_tuning::DomainExposureSession,
    emit_entity_blocks: Option<&[&str]>,
) -> DomainPromptBundle {
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

    let mut out = String::with_capacity(4096);
    if let Some(ref map) = map_opt {
        let legend = map.format_legend(cgs);
        if !legend.is_empty() {
            out.push_str(&legend);
            out.push('\n');
        }
    }

    let mut entities_buf = Vec::new();
    let fill_model = config.include_domain_execution_model;
    render_domain_table_resolved(
        |_| cgs,
        &full_entities,
        map_opt.as_deref(),
        Some(exposure),
        &mut out,
        &mut entities_buf,
        fill_model,
        false,
        emit_entity_blocks,
    );
    let model = if fill_model {
        DomainPromptModel {
            entities: entities_buf,
        }
    } else {
        DomainPromptModel::default()
    };

    DomainPromptBundle { prompt: out, model }
}

/// Render the Plasm teaching surface for the given CGS and [`RenderConfig`].
///
/// The only prompt-facing teaching form is TSV; this wrapper is retained for older callers that
/// historically asked for the markdown DOMAIN surface.
pub fn render_prompt_with_config(cgs: &CGS, config: RenderConfig<'_>) -> String {
    render_prompt_tsv_with_config(cgs, config)
}

/// Render the DOMAIN teaching surface as TSV with stable, Plasm-expression-first rows.
///
/// Columns:
/// `plasm_expr`, `Meaning`
pub fn render_prompt_tsv_with_config(cgs: &CGS, config: RenderConfig<'_>) -> String {
    // Align with [`render_domain_prompt_bundle`]: symbolic modes render entity blocks in
    // [`DomainExposureSession::entities`] order (sorted for `FocusSpec::All`), while
    // [`entity_slices_for_render`] preserves YAML insertion order. Indexing TSV blocks by
    // `full_entities[idx]` must use the same ordering as the bundle or `p#` gloss rows resolve
    // [`IdentMetadata`] against the wrong entity (e.g. `id` typed as `str` vs `int`).
    let (full_entity_names, _) = crate::symbol_tuning::resolve_prompt_surface_entities(
        cgs,
        config.focus,
        config.uses_symbols(),
    );
    let full_entities: Vec<&str> = full_entity_names.iter().map(|s| s.as_str()).collect();
    let bundle = render_domain_prompt_bundle(cgs, config);
    let ident_meta = if config.uses_symbols() {
        let mut acc = HashMap::new();
        for &e in &full_entities {
            acc.extend(crate::symbol_tuning::build_ident_metadata(cgs, &[e]));
        }
        Some(acc)
    } else {
        None
    };
    let symbol_map =
        crate::symbol_tuning::symbol_map_for_prompt(cgs, config.focus, config.uses_symbols());
    render_prompt_tsv_from_bundle(
        &bundle,
        &full_entities,
        symbol_map.as_ref(),
        ident_meta.as_ref(),
        DomainWaveSurface::InitialTeaching,
        config.uses_symbols(),
        |_| cgs,
    )
}

pub(crate) fn render_prompt_surface_from_bundle<'b, F>(
    bundle: &DomainPromptBundle,
    render_mode: PromptRenderMode,
    full_entities: &[&str],
    symbol_map: Option<&SymbolMap>,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    resolve: F,
    wave_surface: DomainWaveSurface,
) -> String
where
    F: FnMut(&str) -> &'b CGS,
{
    render_prompt_tsv_from_bundle(
        bundle,
        full_entities,
        symbol_map,
        ident_meta,
        wave_surface,
        render_mode.uses_symbols(),
        resolve,
    )
}

#[derive(Debug, Default)]
struct ParsedHeading {
    projection: String,
    description: String,
}

#[derive(Debug)]
struct ParsedExprLine {
    expression: String,
    result_type: String,
    /// `[scope …]` fragment when present (DOMAIN / capability-input legend).
    scope: String,
    optional_params: String,
    /// `args: p# wire type req; …` when the compact DOMAIN legend includes it.
    compact_args: String,
    description: String,
}

#[derive(Debug)]
struct ParsedFieldGloss {
    symbol: String,
    field_type: String,
    allowed_values: String,
    description: String,
}

fn render_prompt_tsv_from_bundle<'b, F>(
    bundle: &DomainPromptBundle,
    full_entities: &[&str],
    symbol_map: Option<&SymbolMap>,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    wave_surface: DomainWaveSurface,
    symbolic: bool,
    mut resolve: F,
) -> String
where
    F: FnMut(&str) -> &'b CGS,
{
    let spec = prompt_contract_spec_resolved(&mut resolve, full_entities, symbolic);
    let mut out = String::new();
    if matches!(wave_surface, DomainWaveSurface::InitialTeaching) {
        out.push_str(&comment_prefix_block(&render_prompt_contract(spec)));
        out.push('\n');
    }
    out.push_str(TSV_DOMAIN_TABLE_HEADER);
    let prompt_lines: Vec<&str> = bundle.prompt.lines().collect();
    let blocks = collect_domain_blocks(&prompt_lines);
    for (idx, (heading, block_lines)) in blocks.into_iter().enumerate() {
        let canonical_entity = full_entities.get(idx).copied().unwrap_or_default();
        let catalog_entry_id = resolve(canonical_entity).entry_id.as_deref().unwrap_or("");
        let field_gloss_rows = parse_field_gloss_rows(
            &block_lines,
            canonical_entity,
            catalog_entry_id,
            symbol_map,
            ident_meta,
        );
        let mut field_gloss_by_symbol: HashMap<String, ParsedFieldGloss> = HashMap::new();
        for g in field_gloss_rows {
            field_gloss_by_symbol.insert(g.symbol.clone(), g);
        }
        let parsed_expr_rows = parse_expression_rows(&block_lines);
        let identity_idx = parsed_expr_rows
            .iter()
            .position(|row| {
                tsv_identity_expr_is_entity_get(&row.expression)
                    && !row.expression.contains('{')
                    && !row.expression.contains('~')
                    && !row.result_type.starts_with('[')
            })
            .or_else(|| {
                parsed_expr_rows.iter().position(|row| {
                    row.expression.contains('(')
                        && !row.expression.contains('{')
                        && !row.expression.contains('~')
                        && !row.result_type.starts_with('[')
                })
            })
            // Sole teaching row in the block is the entity's definition in-prompt (including query-only
            // `e#{…}`): merge heading prose via [`TsvRow::identity`]. Skip when multiple rows and no GET,
            // so we do not pick an arbitrary line as "identity".
            .or_else(|| (parsed_expr_rows.len() == 1).then_some(0));
        let mut proj = heading.projection.clone();
        if proj.is_empty() {
            if let Some(i) = identity_idx {
                if let Some(s) = parse_trailing_projection_bracket(&parsed_expr_rows[i].expression)
                {
                    proj = s;
                }
            }
        }
        let projection_symbols = parse_projection_symbols(&proj);
        for sym in &projection_symbols {
            if let Some(gloss) = field_gloss_by_symbol.get(sym.as_str()) {
                write_tsv_row(&mut out, TsvRow::field_gloss(gloss));
            }
        }

        if let Some(idx) = identity_idx {
            let row = &parsed_expr_rows[idx];
            write_tsv_row(&mut out, TsvRow::identity(row, &heading));
        }

        for (idx, row) in parsed_expr_rows.iter().enumerate() {
            if Some(idx) == identity_idx {
                continue;
            }
            write_tsv_row(&mut out, TsvRow::expression(row));
        }
    }
    out
}

struct TsvRow {
    expression: String,
    meaning: String,
}

fn join_tsv_meaning(parts: impl IntoIterator<Item = String>) -> String {
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

impl TsvRow {
    fn identity(row: &ParsedExprLine, heading: &ParsedHeading) -> Self {
        let mut parts = Vec::new();
        if !row.result_type.is_empty() {
            parts.push(format!("returns {}", row.result_type));
        }
        if !heading.projection.is_empty() {
            parts.push(format!("projection {}", heading.projection));
        }
        if !heading.description.is_empty() {
            parts.push(heading.description.clone());
        }
        // When the identity exemplar is a method or list line (fallback pick), keep per-line legend
        // (scope / optional / capability gloss) so TSV stays aligned with compact DOMAIN.
        if !row.scope.is_empty() {
            parts.push(row.scope.clone());
        }
        if !row.optional_params.is_empty() {
            parts.push(format!("optional params: {}", row.optional_params));
        }
        if !row.compact_args.is_empty() {
            parts.push(format!("args: {}", row.compact_args));
        }
        if !row.description.is_empty() {
            parts.push(row.description.clone());
        }
        Self {
            expression: row.expression.clone(),
            meaning: join_tsv_meaning(parts),
        }
    }

    fn field_gloss(gloss: &ParsedFieldGloss) -> Self {
        let mut parts = vec![gloss.field_type.clone()];
        if !gloss.allowed_values.is_empty() {
            parts.push(format!("allowed: {}", gloss.allowed_values));
        }
        if !gloss.description.is_empty() {
            parts.push(gloss.description.clone());
        }
        Self {
            expression: gloss.symbol.clone(),
            meaning: join_tsv_meaning(parts),
        }
    }

    fn expression(row: &ParsedExprLine) -> Self {
        let mut parts = Vec::new();
        if !row.result_type.is_empty() {
            parts.push(format!("returns {}", row.result_type));
        }
        if !row.scope.is_empty() {
            parts.push(row.scope.clone());
        }
        if !row.optional_params.is_empty() {
            parts.push(format!("optional params: {}", row.optional_params));
        }
        if !row.compact_args.is_empty() {
            parts.push(format!("args: {}", row.compact_args));
        }
        if !row.description.is_empty() {
            parts.push(row.description.clone());
        }
        Self {
            expression: row.expression.clone(),
            meaning: join_tsv_meaning(parts),
        }
    }
}

fn write_tsv_row(out: &mut String, row: TsvRow) {
    let expression = row.expression.replace('\t', " ");
    let meaning = row.meaning.replace('\t', " ");
    let _ = writeln!(out, "{expression}\t{meaning}");
}

fn collect_domain_blocks<'a>(prompt_lines: &'a [&'a str]) -> Vec<(ParsedHeading, Vec<&'a str>)> {
    let mut blocks = Vec::new();
    let mut idx = 0usize;
    while idx < prompt_lines.len() {
        let line = prompt_lines[idx];
        let is_heading = is_entity_heading_line(line);
        if !is_heading {
            idx += 1;
            continue;
        }
        let heading = parse_heading_line(line.trim());
        let mut end_idx = prompt_lines.len();
        for (scan, next_line) in prompt_lines.iter().enumerate().skip(idx + 1) {
            let is_next_heading = is_entity_heading_line(next_line);
            if is_next_heading {
                end_idx = scan;
                break;
            }
        }
        let mut pre_start = idx;
        while pre_start > 0 && is_field_gloss_prompt_line(prompt_lines[pre_start - 1]) {
            pre_start -= 1;
        }
        let mut block_lines = Vec::new();
        block_lines.extend_from_slice(&prompt_lines[pre_start..idx]);
        block_lines.extend_from_slice(&prompt_lines[idx + 1..end_idx]);
        blocks.push((heading, block_lines));
        idx = end_idx;
    }
    blocks
}

fn is_entity_heading_line(line: &str) -> bool {
    if !line.starts_with("  ") || line.starts_with("    ") {
        return false;
    }
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with("- ") {
        return false;
    }
    if let Some((lhs, _)) = trimmed.split_once(CAP_LEGEND_SEP) {
        return !lhs.trim().is_empty() && !lhs.trim().contains(' ');
    }
    !trimmed.contains(' ')
}

fn is_field_gloss_prompt_line(line: &str) -> bool {
    let trimmed = line.trim();
    let Some((lhs, _)) = trimmed.split_once(CAP_LEGEND_SEP) else {
        return false;
    };
    lhs.starts_with('p') && lhs.chars().skip(1).all(|c| c.is_ascii_digit())
}

fn parse_heading_line(line: &str) -> ParsedHeading {
    let mut h = ParsedHeading::default();
    let (_, legend_opt) = match line.split_once(CAP_LEGEND_SEP) {
        Some((lhs, rhs)) => (lhs.trim(), Some(rhs.trim())),
        None => (line.trim(), None),
    };
    if let Some(legend) = legend_opt {
        if let Some(start) = legend.find('[') {
            if let Some(end_rel) = legend[start + 1..].find(']') {
                h.projection = legend[start..=start + end_rel + 1].to_string();
            }
        }
        if let Some((_, desc)) = legend.split_once(" -  ") {
            h.description = desc.trim().to_string();
        } else if !legend.starts_with('[') {
            h.description = legend.to_string();
        }
    }
    h
}

fn parse_projection_symbols(projection: &str) -> Vec<String> {
    projection
        .trim()
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .map(|inner| {
            inner
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Suffix on a get expression, e.g. `e#(p#=$,…)[p1,p2,…]`, for projection teaching on the same line
/// as the primary Get (avoids a duplicate list on the entity heading).
fn parse_trailing_projection_bracket(expr: &str) -> Option<String> {
    let t = expr.trim();
    if t.len() < 3 || !t.ends_with(']') {
        return None;
    }
    let open = t.rfind('[')?;
    (open + 1 < t.len()).then_some(t[open..].to_string())
}

fn parse_field_gloss_rows(
    lines: &[&str],
    canonical_entity: &str,
    catalog_entry_id: &str,
    symbol_map: Option<&SymbolMap>,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
) -> Vec<ParsedFieldGloss> {
    let mut out = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        let Some((lhs, rhs)) = trimmed.split_once(CAP_LEGEND_SEP) else {
            continue;
        };
        if !lhs.starts_with('p') || !lhs.chars().skip(1).all(|c| c.is_ascii_digit()) {
            continue;
        }
        let symbol = lhs.trim().to_string();
        let field_name = symbol_map
            .and_then(|m| m.resolve_ident(symbol.as_str()))
            .unwrap_or(symbol.as_str())
            .to_string();
        let meta = ident_meta.and_then(|m| {
            m.get(&(
                catalog_entry_id.to_string(),
                EntityName::from(canonical_entity.to_string()),
                field_name.clone(),
            ))
        });
        let legend = rhs.trim();
        let (mut field_type, legend_tail) = legend
            .split_once(" · ")
            .map(|(ty, tail)| (ty.trim().to_string(), tail.trim().to_string()))
            .unwrap_or_else(|| (legend.to_string(), String::new()));
        // Prefer CGS-backed typing for this `(entity, wire field)` pair. The compact DOMAIN lines in
        // `bundle.prompt` can be wrong when the same `p#` token is reused across entities with the
        // same wire name (e.g. `id`) — the markdown renderer may attach the first-seen legend.
        if let Some(m) = &meta {
            let g = m.render_gloss(symbol_map);
            field_type = g
                .split_once(" \u{00b7} ")
                .map(|(a, _)| a.trim().to_string())
                .unwrap_or_else(|| g.trim().to_string());
        }
        let is_enumish = matches!(field_type.as_str(), "select" | "multiselect");
        let allowed_values = if is_enumish {
            legend_tail.clone()
        } else {
            meta.and_then(|m| m.allowed_values.as_ref())
                .filter(|vals| !vals.is_empty())
                .map(|vals| vals.join(", "))
                .unwrap_or_default()
        };
        let mut description = meta
            .map(|m| m.description.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        // DOMAIN gloss is `type · …` (wire name or CGS prose). CGS `description` may be empty; keep
        // the tail so TSV `Meaning` stays actionable (parity with compact `p#  ;;  str · owner`).
        if description.is_empty() && !is_enumish && !legend_tail.is_empty() {
            description = legend_tail;
        }
        out.push(ParsedFieldGloss {
            symbol,
            field_type,
            allowed_values,
            description,
        });
    }
    out
}

/// Returns `(scope_line, rest)` when `sig` begins with a `[scope …]` block; otherwise `("", sig)`.
fn split_leading_scope_legend(sig: &str) -> (&str, &str) {
    let t = sig.trim_start();
    if !t.starts_with("[scope ") {
        return ("", sig);
    }
    let Some(end) = t.find(']') else {
        return ("", sig);
    };
    let scope_line = t[..=end].trim();
    let rest = t[end + 1..].trim_start();
    (scope_line, rest)
}

/// Split capability signature (scope / optional params) from trailing human gloss after em dash.
fn split_sig_and_human_description(remainder: &str) -> (&str, &str) {
    remainder
        .trim()
        .split_once(LEGEND_EM_DESC_SEP)
        .map(|(a, b)| (a.trim(), b.trim()))
        .unwrap_or((remainder.trim(), ""))
}

/// Strip `args: …` (and its leading ` · ` joiner) from a capability sig fragment; remainder goes to
/// scope/optional parsing, body is the compact slot summary for TSV `Meaning` parity.
fn split_compact_args_from_sig_fragment(sig: &str) -> (String, String) {
    let t = sig.trim();
    if let Some(idx) = t.rfind(" · args:") {
        let a = t[..idx].trim();
        let b = t[idx + " · args:".len()..].trim();
        return (a.to_string(), b.to_string());
    }
    if let Some(s) = t.strip_prefix("args:") {
        return (String::new(), s.trim().to_string());
    }
    (t.to_string(), String::new())
}

/// Parse `sig` into `scope` and `optional_params`; any trailing text that is neither goes to `orphan`.
/// True when `expr` contains a symbolic method call token `.m#(` (e.g. `e6($).m14(…)`).
fn tsv_expr_has_symbolic_method_call(expr: &str) -> bool {
    let b = expr.as_bytes();
    let mut i = 0usize;
    while i + 2 < b.len() {
        if b[i] == b'.' && b[i + 1] == b'm' && b[i + 2].is_ascii_digit() {
            return true;
        }
        i += 1;
    }
    false
}

/// True when the expression is a symbolic **entity get** `e#(…)` / `e#($)` — not `e#.m#(…)` invoke
/// and not an anchored chain like `e#($).m#(…)`.
fn tsv_identity_expr_is_entity_get(expr: &str) -> bool {
    let t = expr.trim_start();
    if tsv_expr_has_symbolic_method_call(t) {
        return false;
    }
    let Some(open) = t.find('(') else {
        return false;
    };
    !t[..open].contains('.')
}

fn fill_scope_optional_from_sig(
    sig: &str,
    scope: &mut String,
    optional_params: &mut String,
    orphan: &mut String,
) {
    scope.clear();
    optional_params.clear();
    orphan.clear();
    let (sc, after_sc) = split_leading_scope_legend(sig);
    *scope = sc.to_string();
    let tail = after_sc.trim();
    if let Some(p) = tail.strip_prefix("optional params:") {
        *optional_params = p.trim().to_string();
    } else if !tail.is_empty() {
        *orphan = tail.to_string();
    }
}

fn parse_expression_rows(lines: &[&str]) -> Vec<ParsedExprLine> {
    let mut out = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('p') && trimmed.contains(CAP_LEGEND_SEP) {
            continue;
        }
        let (expr, legend_opt) = match trimmed.split_once(CAP_LEGEND_SEP) {
            Some((lhs, rhs)) => (lhs.trim().to_string(), Some(rhs.trim())),
            None => (trimmed.to_string(), None),
        };
        let mut row = ParsedExprLine {
            expression: expr,
            result_type: String::new(),
            scope: String::new(),
            optional_params: String::new(),
            compact_args: String::new(),
            description: String::new(),
        };
        if let Some(legend) = legend_opt {
            let mut remainder = legend.trim().to_string();
            if let Some(rest) = remainder.strip_prefix("=>") {
                let rest = rest.trim_start();
                let end = rest.find("  ").unwrap_or(rest.len());
                row.result_type = rest[..end].trim().to_string();
                remainder = rest[end..].trim().to_string();
            }
            let (sig_part, desc_tail) = split_sig_and_human_description(&remainder);
            let (sig_wo, compact) = split_compact_args_from_sig_fragment(sig_part);
            row.compact_args = compact;
            let mut orphan = String::new();
            fill_scope_optional_from_sig(
                &sig_wo,
                &mut row.scope,
                &mut row.optional_params,
                &mut orphan,
            );
            if !desc_tail.is_empty() {
                row.description = desc_tail.to_string();
                if !orphan.is_empty() {
                    row.description = format!("{orphan} {}", row.description).trim().to_string();
                }
            } else if !orphan.is_empty() {
                row.description = orphan;
            }
        }
        out.push(row);
    }
    out
}

/// Character and rough token counts plus prompt surface metrics for a rendered prompt.
///
/// `token_estimate` is a legacy `chars/4` rough figure. Prefer [`Self::prompt_tokens_o200k`]
/// (local `o200k_base` BPE via riptoken) for budgeting closer to OpenAI-style API usage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptSurfaceStats {
    pub prompt_chars: usize,
    /// Legacy: `prompt.chars().count() / 4`. Prefer [`Self::prompt_tokens_o200k`].
    pub token_estimate: usize,
    /// `o200k_base` ordinary token count (local, no network).
    pub prompt_tokens_o200k: usize,
    /// Capabilities whose [`CapabilitySchema::domain`](crate::schema::CapabilitySchema::domain) lies in
    /// the same **full** entity slice as DOMAIN (see [`json_tool_surface_counts`] for slice rules).
    pub capability_tools: usize,
    /// Per entity in that slice: declared relations plus `EntityRef` fields whose name is not
    /// already a relation key (same merge as DOMAIN relation / ref navigation).
    pub navigation_tools: usize,
    /// Plasm path expression lines actually emitted in DOMAIN (per-entity dedupe only: identical
    /// lines in one entity block collapse once; the same string may repeat under another entity).
    pub json_tool_estimate: usize,
}

impl PromptSurfaceStats {
    /// Shared human-readable metrics for CLI stderr: chars, o200k tokens, DOMAIN tool count.
    pub fn summary_line_body(&self) -> String {
        format!(
            "{} chars | ~{} tok (o200k) | ~{} tools (DOMAIN) | {} caps + {} nav (schema); ~{} tok (chars/4)",
            self.prompt_chars,
            self.prompt_tokens_o200k,
            self.json_tool_estimate,
            self.capability_tools,
            self.navigation_tools,
            self.token_estimate,
        )
    }
}

/// Counts capabilities and navigation edges for the same [`FocusSpec`] as [`render_prompt_with_config`].
///
/// Uses [`crate::symbol_tuning::resolve_prompt_surface_entities`] (execute-parity slice when
/// `symbol_tuning` is true — same condition as [`RenderConfig::uses_symbols`] / [`PromptRenderMode::uses_symbols`]; else [`crate::symbol_tuning::entity_slices_for_render`]).
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
            navigation_tools += navigation_edge_count(ent);
        }
    }
    (capability_tools, navigation_tools)
}

fn domain_expression_tool_count_resolved(
    cgs: &CGS,
    names: &[String],
    exposure_opt: Option<&crate::symbol_tuning::DomainExposureSession>,
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
    for &ename in &full_entities {
        let mut seen_expr: HashSet<String> = HashSet::new();
        let block = collect_entity_domain_block(
            cgs,
            ename,
            map.as_deref(),
            None,
            false,
            &mut line_valid_cache,
        );
        for line in &block.lines {
            if seen_expr.insert(line.clone()) {
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

fn navigation_edge_count(ent: &EntityDef) -> usize {
    let rel_names: HashSet<&str> = ent.relations.keys().map(|s| s.as_str()).collect();
    let mut n = ent.relations.len();
    for (fname, f) in &ent.fields {
        if matches!(f.field_type, FieldType::EntityRef { .. })
            && !rel_names.contains(fname.as_str())
        {
            n += 1;
        }
    }
    n
}

// ── DOMAIN (many-shot examples) ───────────────────────────────────────────

#[inline]
fn ent_sym(m: Option<&SymbolMap>, c: &str) -> String {
    m.and_then(|x| x.try_entity_domain_term(c))
        .map(|t| t.to_string())
        .unwrap_or_else(|| c.to_string())
}

#[inline]
fn id_sym_entity(m: Option<&SymbolMap>, entity: &str, field: &str) -> String {
    m.map(|x| x.ident_sym_entity_field(entity, field))
        .unwrap_or_else(|| field.to_string())
}

#[inline]
fn id_sym_cap(m: Option<&SymbolMap>, cap: &crate::CapabilitySchema, param: &str) -> String {
    m.map(|x| x.ident_sym_cap_param(cap.domain.as_str(), cap.name.as_str(), param))
        .unwrap_or_else(|| param.to_string())
}

#[inline]
fn id_sym_rel(m: Option<&SymbolMap>, entity: &str, rel: &str) -> String {
    m.map(|x| x.ident_sym_relation(entity, rel))
        .unwrap_or_else(|| rel.to_string())
}

#[inline]
fn met_sym(m: Option<&SymbolMap>, entity: &str, kebab: &str) -> String {
    m.map(|x| x.method_sym(entity, kebab))
        .unwrap_or_else(|| kebab.to_string())
}

const CAP_LEGEND_SEP: &str = "  ;;  ";

/// Human capability / list gloss after `[scope …]` / `optional params:` (emit parity with
/// [`format_capability_legend_line`]): Unicode em dash U+2014, spaces around it.
const LEGEND_EM_DESC_SEP: &str = " — ";

/// In DOMAIN synthetic lines, bare `$` (and search `~$`) marks a **placeholder** for the real
/// parameter value — use the corresponding `p#` gloss line; it is not a literal value to send to the API.
const DOMAIN_PARAM_VALUE_PLACEHOLDER: &str = "$";

fn truncate_inline_desc(s: &str, max: usize) -> String {
    let t = s.trim().replace('\t', " ");
    crate::utf8_trunc::truncate_utf8_bytes_with_ellipsis(&t, max)
}

/// Append ` ;;  …` only: `=> {result}` (when present) plus capability legend — no `=>` outside the comment
/// (avoids ambiguity with `=` / `>=` in predicates and keeps the expression segment clean).
fn domain_line_with_layers(
    expr: &str,
    result_gloss: Option<String>,
    cap_legend: Option<String>,
) -> String {
    let mut s = expr.to_string();
    let res = result_gloss.filter(|x| !x.is_empty());
    let cap = cap_legend.filter(|x| !x.is_empty());
    if res.is_none() && cap.is_none() {
        return s;
    }
    s.push_str(CAP_LEGEND_SEP);
    let mut parts: Vec<String> = Vec::new();
    if let Some(g) = res {
        parts.push(format!("=> {}", g.trim()));
    }
    if let Some(c) = cap {
        parts.push(c);
    }
    s.push_str(&parts.join("  "));
    s
}

/// Compound `Entity(p#=$,…)` when the target has multiple `key_vars` (per-key placeholders are still the string `$`).
///
/// Unary entity refs use the same `$` fill-in as scalars: `e#($)` in DOMAIN teaching (parseable; not a wire value).
fn entity_ref_id_example(cgs: &CGS, target: &str, map: Option<&SymbolMap>) -> String {
    let target_sym = ent_sym(map, target);
    let p = DOMAIN_PARAM_VALUE_PLACEHOLDER;
    let Some(ent) = cgs.get_entity(target) else {
        return format!("{target_sym}($)");
    };
    if ent.key_vars.len() > 1 {
        let parts: Vec<String> = ent
            .key_vars
            .iter()
            .map(|kv| format!("{}={}", id_sym_entity(map, target, kv.as_str()), p))
            .collect();
        format!("{}({})", target_sym, parts.join(", "))
    } else {
        format!("{target_sym}($)")
    }
}

/// One `p#=value` in `Entity{p#=,…}` — same placeholder discipline as [`invoke_dotted_call_arg_example`].
fn query_param_slot_example(
    f: &crate::InputFieldSchema,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
) -> String {
    if matches!(f.field_type, FieldType::Array) {
        // Array predicates in DOMAIN teaching use bare `$` so query type-check can apply
        // capability-param placeholder relaxation (`field=$`) for list-like filters.
        let n = id_sym_cap(map, cap, f.name.as_str());
        return format!("{n}={}", DOMAIN_PARAM_VALUE_PLACEHOLDER);
    }
    invoke_dotted_call_arg_example(f, cap, cgs, map).unwrap_or_else(|| {
        let n = id_sym_cap(map, cap, f.name.as_str());
        let p = DOMAIN_PARAM_VALUE_PLACEHOLDER;
        match &f.field_type {
            FieldType::Integer | FieldType::Number | FieldType::Boolean => {
                format!("{n}={p}")
            }
            FieldType::String | FieldType::Blob | FieldType::Uuid => format!("{n}={p}"),
            FieldType::Date => format!("{n}={p}", n = n, p = p),
            FieldType::Select | FieldType::MultiSelect => {
                format!("{n}={p}", n = n, p = p)
            }
            FieldType::EntityRef { target } => {
                format!("{n}={}", entity_ref_id_example(cgs, target, map))
            }
            FieldType::Array => {
                format!("{n}=[{p}]", n = n, p = p)
            }
            _ => format!("{n}={p}", n = n, p = p),
        }
    })
}

fn field_is_filter_like(f: &crate::InputFieldSchema) -> bool {
    !matches!(
        f.role,
        Some(ParameterRole::Search)
            | Some(ParameterRole::Sort)
            | Some(ParameterRole::SortDirection)
            | Some(ParameterRole::ResponseControl)
    )
}

/// One `p#=value` for a **required scope** parameter (same as filter slots).
fn scope_param_slot(
    f: &InputFieldSchema,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
) -> String {
    query_param_slot_example(f, cap, cgs, map)
}

/// `Entity(k=v,…)` for multi-`key_vars` GET examples (validated like other DOMAIN lines).
fn compound_get_expr_line(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
) -> Option<String> {
    if ent.key_vars.len() <= 1 {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    let p = DOMAIN_PARAM_VALUE_PLACEHOLDER;
    for kv in &ent.key_vars {
        let f = ent.fields.get(kv)?;
        let sym = id_sym_entity(map, ent.name.as_str(), kv.as_str());
        match &f.field_type {
            FieldType::Integer
            | FieldType::Number
            | FieldType::Boolean
            | FieldType::String
            | FieldType::Uuid
            | FieldType::Date
            | FieldType::Select
            | FieldType::MultiSelect
            | FieldType::Array
            | FieldType::Json
            | FieldType::Blob => {
                parts.push(format!("{sym}={p}"));
            }
            FieldType::EntityRef { target } => {
                parts.push(format!("{sym}={}", entity_ref_id_example(cgs, target, map)));
            }
        }
    }
    Some(format!("{es}({})", parts.join(", ")))
}

fn anchored_receiver_expr(es: &str, ent: &EntityDef, cgs: &CGS, map: Option<&SymbolMap>) -> String {
    compound_get_expr_line(es, ent, cgs, map)
        .unwrap_or_else(|| format!("{es}({})", DOMAIN_PARAM_VALUE_PLACEHOLDER))
}

/// Scope predicates + all filter-like parameters (required + optional) with CGS-derived placeholders.
fn query_expr_maximal(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
) -> Option<String> {
    let Some(is) = &cap.input_schema else {
        return Some(es.to_string());
    };
    let InputType::Object { fields, .. } = &is.input_type else {
        return None;
    };
    let fields = fields.as_slice();

    let scope_fields: Vec<&crate::InputFieldSchema> = fields
        .iter()
        .filter(|f| f.required && matches!(f.role, Some(ParameterRole::Scope)))
        .collect();

    let mut inner: Vec<String> = Vec::new();
    for sf in &scope_fields {
        inner.push(scope_param_slot(sf, cap, cgs, map));
    }

    for f in fields {
        if matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        inner.push(query_param_slot_example(f, cap, cgs, map));
    }

    if inner.is_empty() {
        return Some(es.to_string());
    }
    Some(format!("{es}{{{}}}", inner.join(", ")))
}

/// Filter predicates only (no scope) — one `Entity{p#=…}` line per query cap so DOMAIN shows **filter**
/// field symbols even when scope+filters are merged on the maximal line.
fn query_expr_filters_only(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
) -> Option<String> {
    let Some(is) = &cap.input_schema else {
        return None;
    };
    let InputType::Object { fields, .. } = &is.input_type else {
        return None;
    };
    let mut inner: Vec<String> = Vec::new();
    for f in fields {
        if matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        inner.push(query_param_slot_example(f, cap, cgs, map));
    }
    if inner.is_empty() {
        return None;
    }
    Some(format!("{es}{{{}}}", inner.join(", ")))
}

/// Only scope predicates (for a distinct structural example when maximal adds filters).
fn query_expr_scope_only(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
) -> Option<String> {
    let Some(is) = &cap.input_schema else {
        return None;
    };
    let InputType::Object { fields, .. } = &is.input_type else {
        return None;
    };
    let scope_fields: Vec<&crate::InputFieldSchema> = fields
        .iter()
        .filter(|f| f.required && matches!(f.role, Some(ParameterRole::Scope)))
        .collect();
    if scope_fields.is_empty() {
        return None;
    }
    let mut inner: Vec<String> = Vec::new();
    for sf in &scope_fields {
        inner.push(scope_param_slot(sf, cap, cgs, map));
    }
    Some(format!("{es}{{{}}}", inner.join(", ")))
}

#[inline]
fn path_vars_empty(cap: &crate::CapabilitySchema) -> bool {
    !cap.domain_exemplar_requires_entity_anchor()
}

/// Cardinality-many relation nav `Source(id).rel` parses to [`Expr::Chain`] when `materialize` is set;
/// with [`RelationMaterialization::Unavailable`], parse fails — omit DOMAIN lines that cannot validate.
fn many_relation_nav_emittable(rel_schema: &crate::RelationSchema) -> bool {
    if rel_schema.cardinality != Cardinality::Many {
        return true;
    }
    !matches!(
        rel_schema
            .materialize
            .as_ref()
            .unwrap_or(&RelationMaterialization::Unavailable),
        RelationMaterialization::Unavailable
    )
}

/// DOMAIN line metadata from an already type-checked [`Expr`] (avoids a second parse in the render hot path).
fn domain_line_execution_meta_from_validated(
    cgs: &CGS,
    work: String,
    relation: Option<&RelationSchema>,
    source_capability: Option<&CapabilityName>,
    expr: &Expr,
) -> DomainLineMeta {
    let relation_materialization = relation.map(|r| {
        RelationMaterializationSummary::from(
            r.materialize
                .as_ref()
                .unwrap_or(&RelationMaterialization::Unavailable),
        )
    });

    let (kind, cross_entity) = if relation.is_some() {
        (DomainLineKind::RelationNav, None)
    } else if work.contains('~') {
        (DomainLineKind::Search, None)
    } else {
        let kind = match expr {
            Expr::Get(_) => DomainLineKind::Get,
            Expr::Query(_) => DomainLineKind::Query,
            Expr::Create(_) | Expr::Delete(_) | Expr::Invoke(_) => DomainLineKind::Method,
            Expr::Chain(_) | Expr::Page(_) => DomainLineKind::Other,
        };
        let cross_entity = if let Expr::Query(q) = expr {
            if let (Some(pred), Some(ent_def)) = (&q.predicate, cgs.get_entity(q.entity.as_str())) {
                let crosses = extract_cross_entity_predicates(pred, ent_def, cgs);
                if crosses.is_empty() {
                    None
                } else {
                    Some(
                        crosses
                            .iter()
                            .map(|c| {
                                let strat = choose_strategy(c, q.entity.as_str(), cgs);
                                CrossEntityPlanMeta {
                                    ref_field: c.ref_field.clone(),
                                    foreign_entity: c.foreign_entity.clone(),
                                    strategy: match strat {
                                        crate::cross_entity::CrossEntityStrategy::PushLeft {
                                            ..
                                        } => CrossEntityStrategyKind::PushLeft,
                                        crate::cross_entity::CrossEntityStrategy::PullRight {
                                            ..
                                        } => CrossEntityStrategyKind::PullRight,
                                    },
                                }
                            })
                            .collect(),
                    )
                }
            } else {
                None
            }
        } else {
            None
        };
        (kind, cross_entity)
    };

    DomainLineMeta {
        expression: work,
        kind,
        source_capability: source_capability.map(|n| n.to_string()),
        cross_entity,
        relation_materialization,
    }
}

type DomainLineValidCacheKey = (usize, String);

#[inline]
fn domain_line_cache_key(cgs: &CGS, work: &str) -> DomainLineValidCacheKey {
    ((cgs as *const CGS).addr(), work.to_string())
}

#[allow(clippy::too_many_arguments)]
fn try_push_domain_example(
    lines: &mut Vec<String>,
    line_metas: &mut Vec<DomainLineMeta>,
    collect_meta: bool,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    expr: &str,
    gloss: Option<String>,
    cap_leg: Option<String>,
    relation: Option<&RelationSchema>,
    source_capability: Option<&CapabilityName>,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, bool>,
) -> bool {
    let work = domain_line_work_string(expr, map);

    if collect_meta {
        let Some(parsed) = domain_line_validate_full(cgs, &work) else {
            return false;
        };
        let rendered = domain_line_with_layers(expr, gloss, cap_leg);
        lines.push(rendered);
        line_metas.push(domain_line_execution_meta_from_validated(
            cgs,
            work,
            relation,
            source_capability,
            &parsed.expr,
        ));
        return true;
    }

    let cache_key = domain_line_cache_key(cgs, &work);
    let ok = *line_valid_cache
        .entry(cache_key)
        .or_insert_with(|| domain_line_valid_work(cgs, &work));
    if !ok {
        return false;
    }
    let rendered = domain_line_with_layers(expr, gloss, cap_leg);
    lines.push(rendered);
    true
}

#[inline]
fn domain_line_work_string(line: &str, map: Option<&SymbolMap>) -> String {
    let stripped = crate::symbol_tuning::strip_prompt_expression_annotations(line);
    map.map(|m| crate::symbol_tuning::expand_path_symbols(&stripped, m))
        .unwrap_or(stripped)
}

fn domain_line_validate_full(cgs: &CGS, work: &str) -> Option<crate::expr_parser::ParsedExpr> {
    let mut r = crate::expr_parser::parse(work, cgs).ok()?;
    if crate::normalize_expr_query_capabilities(&mut r.expr, cgs).is_err() {
        return None;
    }
    crate::type_check_expr(&r.expr, cgs).ok()?;
    Some(r)
}

#[inline]
fn domain_line_valid_work(cgs: &CGS, work: &str) -> bool {
    domain_line_validate_full(cgs, work).is_some()
}

/// Same rule as `Parser::can_bind_create_path_vars`: path template binds `{anchor}_id` from `Get(anchor)`.
fn can_bind_create_from_anchor(cap: &crate::CapabilitySchema, anchor: &str) -> bool {
    let path_vars = crate::schema::path_var_names_from_mapping_json(&cap.mapping.template.0);
    if path_vars.is_empty() {
        return false;
    }
    let expected = format!("{}_id", anchor.to_lowercase());
    path_vars.iter().all(|pv| pv == &expected)
}

/// Omit `team_id`-style path keys from explicit `(…)` — parser injects them from `Entity($)` / `Entity(42)` on the left.
fn field_omitted_from_path_inject(
    anchor_entity: &str,
    cap: &crate::CapabilitySchema,
    field_name: &str,
) -> bool {
    let path_vars = crate::schema::path_var_names_from_mapping_json(&cap.mapping.template.0);
    if !path_vars.iter().any(|pv| pv == field_name) {
        return false;
    }
    let expected = format!("{}_id", anchor_entity.to_lowercase());
    field_name == expected
}

fn collect_capability_compact_arg_glosses(
    anchor_entity: &str,
    cap: &crate::CapabilitySchema,
    map: &SymbolMap,
    ident_meta: &HashMap<IdentMetaKey, IdentMetadata>,
    catalog_entry_id: &str,
) -> Vec<crate::symbol_tuning::CompactArgSlotGloss> {
    let Some(is) = cap.input_schema.as_ref() else {
        return Vec::new();
    };
    let InputType::Object { fields, .. } = &is.input_type else {
        return Vec::new();
    };
    let en = cap.domain.as_str();
    let capn = cap.name.as_str();
    let mut out = Vec::new();
    for f in fields {
        if !field_is_filter_like(f) {
            continue;
        }
        if field_omitted_from_path_inject(anchor_entity, cap, f.name.as_str()) {
            continue;
        }
        let sym = map.ident_sym_cap_param(en, capn, f.name.as_str());
        let mkey = (
            catalog_entry_id.to_string(),
            EntityName::from(en.to_string()),
            f.name.clone(),
        );
        let Some(meta) = ident_meta.get(&mkey) else {
            continue;
        };
        out.push(crate::symbol_tuning::build_compact_arg_slot_gloss(
            &sym,
            f.name.as_str(),
            f.required,
            meta,
            map,
        ));
    }
    out
}

/// DOMAIN `;;` suffix: `[scope …]` and, when present, compact `args: p# … req|opt; …` (required slots
/// before optional). Omits the separate `optional params: p#,…` list whenever `args:` is emitted.
/// Falls back to `[scope …] optional params: …` only when compact `args:` is unavailable. Then ` — ` + description.
fn format_capability_legend_line(
    map: &SymbolMap,
    cap: &crate::CapabilitySchema,
    anchor_entity: &str,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    catalog_entry_id: &str,
) -> String {
    const MAX_DESC: usize = 100;
    let kebab = capability_method_label_kebab(cap);
    let raw = cap.description.as_str().trim();
    let gloss = if raw.is_empty() {
        kebab
    } else {
        truncate_inline_desc(raw, MAX_DESC)
    };
    let sig = if let Some(im) = ident_meta {
        let frags =
            collect_capability_compact_arg_glosses(anchor_entity, cap, map, im, catalog_entry_id);
        if let Some(args_s) = crate::symbol_tuning::join_compact_invocation_arg_fragments(frags) {
            // `args:` already tags each slot req/opt — omit the redundant `optional params: p#,…` list.
            let scope_only = map.capability_scope_legend_gloss(cap);
            if scope_only.is_empty() {
                format!("args: {args_s}")
            } else {
                format!("{scope_only} · args: {args_s}")
            }
        } else {
            map.capability_input_signature_gloss(cap)
        }
    } else {
        map.capability_input_signature_gloss(cap)
    };
    if sig.is_empty() {
        gloss
    } else if gloss.is_empty() {
        sig
    } else {
        format!("{sig}{LEGEND_EM_DESC_SEP}{gloss}")
    }
}

#[inline]
fn capability_legend_for_domain(
    map: Option<&SymbolMap>,
    cap: &crate::CapabilitySchema,
    anchor_entity: &str,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    catalog_entry_id: &str,
) -> Option<String> {
    map.map(|m| format_capability_legend_line(m, cap, anchor_entity, ident_meta, catalog_entry_id))
}

/// One `key=value` for dotted-call `method(k=v,…)` — equality/entity forms parse as invoke args (not query `>=` predicates).
fn invoke_dotted_call_arg_example(
    f: &crate::InputFieldSchema,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
) -> Option<String> {
    let n = id_sym_cap(map, cap, f.name.as_str());
    let p = DOMAIN_PARAM_VALUE_PLACEHOLDER;
    match &f.field_type {
        FieldType::Boolean
        | FieldType::String
        | FieldType::Blob
        | FieldType::Json
        | FieldType::Uuid
        | FieldType::Integer
        | FieldType::Number => Some(format!("{n}={p}")),
        FieldType::Select | FieldType::MultiSelect => Some(format!("{n}={p}")),
        FieldType::EntityRef { target } => {
            Some(format!("{n}={}", entity_ref_id_example(cgs, target, map)))
        }
        FieldType::Date => match &f.value_format {
            // Same placeholder as strings — avoid teaching ISO literals in DOMAIN dotted-call invokes.
            Some(ValueWireFormat::Temporal(_)) => Some(format!(
                "{n}={p}",
                n = n,
                p = DOMAIN_PARAM_VALUE_PLACEHOLDER
            )),
            _ => None,
        },
        FieldType::Array => match f.array_items.as_ref() {
            Some(_items) => Some(format!("{n}=[{p}]", n = n, p = p)),
            None => Some(format!(r#"{n}=[]"#)),
        },
    }
}

fn build_dotted_call_paren_args(
    anchor_entity: &str,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
) -> Option<String> {
    let is = cap.input_schema.as_ref()?;
    let InputType::Object { fields, .. } = &is.input_type else {
        return None;
    };
    let mut has_optional = false;
    for f in fields {
        if matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        if field_omitted_from_path_inject(anchor_entity, cap, f.name.as_str()) {
            continue;
        }
        if !f.required {
            has_optional = true;
        }
    }
    let mut parts: Vec<String> = Vec::new();
    let mut required_example_failed = false;
    for f in fields {
        if !f.required || !matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if field_omitted_from_path_inject(anchor_entity, cap, f.name.as_str()) {
            continue;
        }
        parts.push(scope_param_slot(f, cap, cgs, map));
    }
    for f in fields {
        if matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        if field_omitted_from_path_inject(anchor_entity, cap, f.name.as_str()) {
            continue;
        }
        if !f.required {
            continue;
        }
        match invoke_dotted_call_arg_example(f, cap, cgs, map) {
            Some(a) => parts.push(a),
            None => required_example_failed = true,
        }
    }
    if required_example_failed {
        return None;
    }
    if parts.is_empty() && has_optional {
        return Some("..".to_string());
    }
    if parts.is_empty() {
        return None;
    }
    if has_optional {
        Some(format!("{},..", parts.join(", ")))
    } else {
        Some(parts.join(", "))
    }
}

/// Parentheses for **standalone** `Entity.create(…)` when the capability has required `role: scope`
/// parameters (no anchor to inject them). [`build_dotted_call_paren_args`] skips scope fields;
/// without scope slots, lines like `Comment.create(text=…)` fail validation for nested REST creates.
fn build_standalone_create_paren_args(
    ename: &str,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
) -> Option<String> {
    if cap.kind != CapabilityKind::Create {
        return build_dotted_call_paren_args(ename, cap, cgs, map);
    }
    let is = cap.input_schema.as_ref()?;
    let InputType::Object { fields, .. } = &is.input_type else {
        return None;
    };
    let has_required_scope = fields
        .iter()
        .any(|f| f.required && matches!(f.role, Some(ParameterRole::Scope)));
    if !has_required_scope {
        return build_dotted_call_paren_args(ename, cap, cgs, map);
    }

    let mut has_optional = false;
    for f in fields {
        if matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        if field_omitted_from_path_inject(ename, cap, f.name.as_str()) {
            continue;
        }
        if !f.required {
            has_optional = true;
        }
    }

    let mut parts: Vec<String> = Vec::new();
    let mut required_failed = false;
    for f in fields {
        if !f.required {
            continue;
        }
        if matches!(f.role, Some(ParameterRole::Scope)) {
            parts.push(scope_param_slot(f, cap, cgs, map));
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        if field_omitted_from_path_inject(ename, cap, f.name.as_str()) {
            continue;
        }
        match invoke_dotted_call_arg_example(f, cap, cgs, map) {
            Some(a) => parts.push(a),
            None => required_failed = true,
        }
    }
    if required_failed {
        return None;
    }
    if parts.is_empty() && has_optional {
        return Some("..".to_string());
    }
    if parts.is_empty() {
        return None;
    }
    if has_optional {
        Some(format!("{},..", parts.join(", ")))
    } else {
        Some(parts.join(", "))
    }
}

fn format_dotted_call_line(
    anchor_entity: &str,
    cap: &crate::CapabilitySchema,
    ent: &EntityDef,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
) -> Option<String> {
    let args = build_dotted_call_paren_args(anchor_entity, cap, cgs, map)?;
    let label = capability_method_label_kebab(cap);
    let ms = met_sym(map, cap.domain.as_str(), &label);
    let recv = anchored_receiver_expr(es, ent, cgs, map);
    Some(format!("{recv}.{ms}({args})"))
}

const MAX_MULTI_ARITY_METHOD_LINES: usize = 48;

/// Non–zero-arity invoke/create/update: `e#($).m#(p#=…)` (same rules as parser dotted-call capability resolution).
fn collect_multi_arity_method_lines(
    cgs: &CGS,
    ename: &str,
    es: &str,
    map: Option<&SymbolMap>,
) -> Vec<(CapabilityName, String)> {
    let mut out: Vec<(CapabilityName, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let Some(ent) = cgs.get_entity(ename) else {
        return out;
    };

    for cap in cgs.find_capabilities(ename, CapabilityKind::Action) {
        if capability_is_zero_arity_invoke(cap) {
            continue;
        }
        if !seen.insert(cap.name.to_string()) {
            continue;
        }
        if let Some(line) = format_dotted_call_line(ename, cap, ent, es, cgs, map) {
            out.push((cap.name.clone(), line));
        }
    }
    for cap in cgs.find_capabilities(ename, CapabilityKind::Update) {
        if capability_is_zero_arity_invoke(cap) {
            continue;
        }
        if !seen.insert(cap.name.to_string()) {
            continue;
        }
        if let Some(line) = format_dotted_call_line(ename, cap, ent, es, cgs, map) {
            out.push((cap.name.clone(), line));
        }
    }
    for cap in cgs.find_capabilities(ename, CapabilityKind::Delete) {
        if capability_is_zero_arity_invoke(cap) {
            continue;
        }
        if !seen.insert(cap.name.to_string()) {
            continue;
        }
        if let Some(line) = format_dotted_call_line(ename, cap, ent, es, cgs, map) {
            out.push((cap.name.clone(), line));
        }
    }
    // Anchored creates: `Parent($).create-child(args)` — cap.domain is the child,
    // but the CML path binds `{ename}_id` from the anchor.
    for cap in cgs.capabilities.values() {
        if cap.kind != CapabilityKind::Create {
            continue;
        }
        if !can_bind_create_from_anchor(cap, ename) {
            continue;
        }
        if !seen.insert(cap.name.to_string()) {
            continue;
        }
        if let Some(line) = format_dotted_call_line(ename, cap, ent, es, cgs, map) {
            out.push((cap.name.clone(), line));
        }
    }

    // Standalone creates: `Entity.create(args)` — cap.domain == ename, no anchor needed.
    for cap in cgs.find_capabilities(ename, CapabilityKind::Create) {
        if seen.contains(cap.name.as_str()) {
            continue;
        }
        if !seen.insert(cap.name.to_string()) {
            continue;
        }
        let label = capability_method_label_kebab(cap);
        let ms = met_sym(map, ename, &label);
        let line = match build_standalone_create_paren_args(ename, cap, cgs, map) {
            Some(args) => format!("{es}.{ms}({args})"),
            None => format!("{es}.{ms}()"),
        };
        out.push((cap.name.clone(), line));
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.into_iter().take(MAX_MULTI_ARITY_METHOD_LINES).collect()
}

/// CGS-synthesized DOMAIN block — one block per entity — ordered expression lines (get, methods, query, search, relations).
struct EntityDomainBlock {
    entity_sym: String,
    /// Full `[p#,…]` / `[field,…]` from primary Get when projection teaching applies (same gate as former indented exemplar).
    heading_projection_bracket: Option<String>,
    lines: Vec<String>,
    /// Parallel to `lines` when `collect_meta` was true at build time; otherwise empty.
    line_metas: Vec<DomainLineMeta>,
}

/// Heading body after the outer `  ` indent (entity sym, optional `;;` bracket + description).
fn format_entity_domain_heading_line(
    entity_sym: &str,
    projection_bracket: Option<&str>,
    desc: Option<&str>,
) -> String {
    match (projection_bracket, desc) {
        (Some(b), Some(d)) => format!("{}{}{b} -  {}", entity_sym, CAP_LEGEND_SEP, d),
        (Some(b), None) => format!("{}{}{b}", entity_sym, CAP_LEGEND_SEP),
        (None, Some(d)) => format!("{}{}{}", entity_sym, CAP_LEGEND_SEP, d),
        (None, None) => entity_sym.to_string(),
    }
}

fn collect_entity_domain_block(
    cgs: &CGS,
    ename: &str,
    map: Option<&SymbolMap>,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    collect_meta: bool,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, bool>,
) -> EntityDomainBlock {
    let mut lines: Vec<String> = Vec::new();
    let mut line_metas: Vec<DomainLineMeta> = Vec::new();

    let Some(ent) = cgs.get_entity(ename) else {
        return EntityDomainBlock {
            entity_sym: String::new(),
            heading_projection_bracket: None,
            lines,
            line_metas,
        };
    };
    let es = ent_sym(map, ename);
    let catalog_entry_id = cgs.entry_id.as_deref().unwrap_or("");

    let primary_get_projection_bracket: Option<String> =
        cgs.domain_projection_heading_fields(ename, ent).map(|f| {
            let syms: Vec<String> = f
                .iter()
                .map(|k| id_sym_entity(map, ename, k.as_str()))
                .collect();
            format!("[{}]", syms.join(","))
        });
    let mut inline_bracket_onto_primary_get = false;

    let get_caps: Vec<_> = cgs.find_capabilities(ename, CapabilityKind::Get);
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

    let mut seen_singleton_cap: HashSet<String> = HashSet::new();
    for cap in &singleton_get_caps {
        if !seen_singleton_cap.insert(cap.name.to_string()) {
            continue;
        }
        let label = capability_method_label_kebab(cap);
        let ms = met_sym(map, ename, &label);
        let expr = format!("{es}.{ms}()");
        let result_gloss = crate::result_gloss::result_gloss_for_capability(cap, cgs, map);
        let cap_leg = capability_legend_for_domain(map, cap, ename, ident_meta, catalog_entry_id);
        try_push_domain_example(
            &mut lines,
            &mut line_metas,
            collect_meta,
            cgs,
            map,
            &expr,
            result_gloss,
            cap_leg,
            None,
            Some(&cap.name),
            line_valid_cache,
        );
    }

    let get_gloss = Some(crate::result_gloss::result_gloss_for_get_entity(ename, map));
    let primary_get_cap = cgs.resolved_primary_get_for_projection(ename, ent);
    if primary_get_cap.is_some() && !only_singleton_gets {
        let primary_name = primary_get_cap.map(|c| &c.name);
        let br_suffix: Option<&str> = primary_get_projection_bracket
            .as_deref()
            .filter(|b| !b.is_empty());
        let mut emitted_primary_get = false;
        if let Some(cmp) = compound_get_expr_line(&es, ent, cgs, map) {
            if let Some(b) = br_suffix {
                let with_br = format!("{cmp}{b}");
                if try_push_domain_example(
                    &mut lines,
                    &mut line_metas,
                    collect_meta,
                    cgs,
                    map,
                    &with_br,
                    get_gloss.clone(),
                    None,
                    None,
                    primary_name,
                    line_valid_cache,
                ) {
                    emitted_primary_get = true;
                    inline_bracket_onto_primary_get = true;
                }
            }
            if !emitted_primary_get
                && try_push_domain_example(
                    &mut lines,
                    &mut line_metas,
                    collect_meta,
                    cgs,
                    map,
                    &cmp,
                    get_gloss.clone(),
                    None,
                    None,
                    primary_name,
                    line_valid_cache,
                )
            {
                emitted_primary_get = true;
            }
        }
        if !emitted_primary_get {
            let line_base = format!("{es}({})", DOMAIN_PARAM_VALUE_PLACEHOLDER);
            let mut line_g = line_base.clone();
            if let Some(b) = br_suffix {
                line_g.push_str(b);
            }
            if br_suffix.is_some() && line_g != line_base {
                if try_push_domain_example(
                    &mut lines,
                    &mut line_metas,
                    collect_meta,
                    cgs,
                    map,
                    &line_g,
                    get_gloss.clone(),
                    None,
                    None,
                    primary_name,
                    line_valid_cache,
                ) {
                    inline_bracket_onto_primary_get = true;
                } else if try_push_domain_example(
                    &mut lines,
                    &mut line_metas,
                    collect_meta,
                    cgs,
                    map,
                    &line_base,
                    get_gloss.clone(),
                    None,
                    None,
                    primary_name,
                    line_valid_cache,
                ) {
                    // bracket stays on heading
                }
            } else {
                let _ = try_push_domain_example(
                    &mut lines,
                    &mut line_metas,
                    collect_meta,
                    cgs,
                    map,
                    &line_g,
                    get_gloss.clone(),
                    None,
                    None,
                    primary_name,
                    line_valid_cache,
                );
            }
        }
    }

    let mut zero_arity_method_caps: Vec<&crate::CapabilitySchema> = Vec::new();
    for kind in [
        CapabilityKind::Action,
        CapabilityKind::Update,
        CapabilityKind::Delete,
    ] {
        for cap in cgs.find_capabilities(ename, kind) {
            if capability_is_zero_arity_invoke(cap) {
                zero_arity_method_caps.push(cap);
            }
        }
    }
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
            let label = capability_method_label_kebab(cap);
            let ms = met_sym(map, ename, &label);
            let recv = anchored_receiver_expr(&es, ent, cgs, map);
            let expr = if path_vars_empty(cap) {
                format!("{es}.{ms}()")
            } else {
                format!("{recv}.{ms}()")
            };
            let result_gloss = crate::result_gloss::result_gloss_for_capability(cap, cgs, map);
            let cap_leg =
                capability_legend_for_domain(map, cap, ename, ident_meta, catalog_entry_id);
            try_push_domain_example(
                &mut lines,
                &mut line_metas,
                collect_meta,
                cgs,
                map,
                &expr,
                result_gloss,
                cap_leg,
                None,
                Some(&cap.name),
                line_valid_cache,
            );
        }
    }
    for (cap_name, line) in collect_multi_arity_method_lines(cgs, ename, &es, map) {
        let cap_ref = cgs.capabilities.get(&cap_name);
        let cap_leg = cap_ref.and_then(|c| {
            capability_legend_for_domain(map, c, ename, ident_meta, catalog_entry_id)
        });
        let gloss =
            cap_ref.and_then(|c| crate::result_gloss::result_gloss_for_capability(c, cgs, map));
        try_push_domain_example(
            &mut lines,
            &mut line_metas,
            collect_meta,
            cgs,
            map,
            &line,
            gloss,
            cap_leg,
            None,
            Some(&cap_name),
            line_valid_cache,
        );
    }

    let mut query_caps: Vec<_> = cgs.find_capabilities(ename, CapabilityKind::Query);
    query_caps.sort_by(|a, b| a.name.cmp(&b.name));

    if !query_caps.is_empty() {
        let mut local_seen: HashSet<String> = HashSet::new();
        let mut query_line_count: usize = 0;
        const MAX_QUERY_LINES: usize = 32;
        for cap in &query_caps {
            if query_line_count >= MAX_QUERY_LINES {
                break;
            }
            let qgloss = crate::result_gloss::result_gloss_for_capability(cap, cgs, map);
            let cap_leg =
                capability_legend_for_domain(map, cap, ename, ident_meta, catalog_entry_id);
            let mut added = false;
            if let Some(line) = query_expr_maximal(cap, &es, cgs, map) {
                let work = domain_line_work_string(&line, map);
                let parsed_opt = if collect_meta {
                    domain_line_validate_full(cgs, &work)
                } else {
                    None
                };
                let passes = if collect_meta {
                    parsed_opt.is_some()
                } else {
                    *line_valid_cache
                        .entry(domain_line_cache_key(cgs, &work))
                        .or_insert_with(|| domain_line_valid_work(cgs, &work))
                };
                if passes && local_seen.insert(line.clone()) {
                    let rendered = domain_line_with_layers(&line, qgloss.clone(), cap_leg.clone());
                    lines.push(rendered.clone());
                    if collect_meta {
                        let parsed = parsed_opt.expect("query line passed validation");
                        line_metas.push(domain_line_execution_meta_from_validated(
                            cgs,
                            work,
                            None,
                            Some(&cap.name),
                            &parsed.expr,
                        ));
                    }
                    added = true;
                    query_line_count += 1;
                }
            }
            if !added {
                if let Some(line) = query_expr_scope_only(cap, &es, cgs, map) {
                    let work = domain_line_work_string(&line, map);
                    let parsed_opt = if collect_meta {
                        domain_line_validate_full(cgs, &work)
                    } else {
                        None
                    };
                    let passes = if collect_meta {
                        parsed_opt.is_some()
                    } else {
                        *line_valid_cache
                            .entry(domain_line_cache_key(cgs, &work))
                            .or_insert_with(|| domain_line_valid_work(cgs, &work))
                    };
                    if passes && local_seen.insert(line.clone()) {
                        let rendered =
                            domain_line_with_layers(&line, qgloss.clone(), cap_leg.clone());
                        lines.push(rendered.clone());
                        if collect_meta {
                            let parsed = parsed_opt.expect("query line passed validation");
                            line_metas.push(domain_line_execution_meta_from_validated(
                                cgs,
                                work,
                                None,
                                Some(&cap.name),
                                &parsed.expr,
                            ));
                        }
                        added = true;
                        query_line_count += 1;
                    }
                }
            }
            if !added {
                if let Some(line) = query_expr_filters_only(cap, &es, cgs, map) {
                    let work = domain_line_work_string(&line, map);
                    let parsed_opt = if collect_meta {
                        domain_line_validate_full(cgs, &work)
                    } else {
                        None
                    };
                    let passes = if collect_meta {
                        parsed_opt.is_some()
                    } else {
                        *line_valid_cache
                            .entry(domain_line_cache_key(cgs, &work))
                            .or_insert_with(|| domain_line_valid_work(cgs, &work))
                    };
                    if passes && local_seen.insert(line.clone()) {
                        let rendered =
                            domain_line_with_layers(&line, qgloss.clone(), cap_leg.clone());
                        lines.push(rendered.clone());
                        if collect_meta {
                            let parsed = parsed_opt.expect("query line passed validation");
                            line_metas.push(domain_line_execution_meta_from_validated(
                                cgs,
                                work,
                                None,
                                Some(&cap.name),
                                &parsed.expr,
                            ));
                        }
                        query_line_count += 1;
                    }
                }
            }
        }
    }

    if !cgs
        .find_capabilities(ename, CapabilityKind::Search)
        .is_empty()
    {
        let line = format!("{es}~{}", DOMAIN_PARAM_VALUE_PLACEHOLDER);
        let mut search_caps = cgs.find_capabilities(ename, CapabilityKind::Search);
        search_caps.sort_by(|a, b| a.name.cmp(&b.name));
        let scap = cgs
            .primary_search_capability(ename)
            .or_else(|| search_caps.first().copied());
        let sg =
            scap.and_then(|cap| crate::result_gloss::result_gloss_for_capability(cap, cgs, map));
        let cap_leg = scap.and_then(|cap| {
            capability_legend_for_domain(map, cap, ename, ident_meta, catalog_entry_id)
        });
        try_push_domain_example(
            &mut lines,
            &mut line_metas,
            collect_meta,
            cgs,
            map,
            &line,
            sg,
            cap_leg,
            None,
            scap.map(|c| &c.name),
            line_valid_cache,
        );
    }

    let mut nav_keys: Vec<String> = ent
        .relations
        .keys()
        .map(|k| k.as_str().to_string())
        .collect();
    let rel_names: HashSet<&str> = ent.relations.keys().map(|s| s.as_str()).collect();
    for fname in ent.fields.keys() {
        if let Some(f) = ent.fields.get(fname) {
            if matches!(f.field_type, FieldType::EntityRef { .. })
                && !rel_names.contains(fname.as_str())
            {
                nav_keys.push(fname.as_str().to_string());
            }
        }
    }
    nav_keys.sort();
    const MAX_REL_NAV_LINES: usize = 16;
    for rel in nav_keys.iter().take(MAX_REL_NAV_LINES) {
        let (target_entity, skip_many_unresolved, rel_for_meta) =
            if let Some(rel_schema) = ent.relations.get(rel.as_str()) {
                let skip = rel_schema.cardinality == Cardinality::Many
                    && !many_relation_nav_emittable(rel_schema);
                (rel_schema.target_resource.clone(), skip, Some(rel_schema))
            } else if let Some(f) = ent.fields.get(rel.as_str()) {
                match &f.field_type {
                    FieldType::EntityRef { target } => (target.clone(), false, None),
                    _ => continue,
                }
            } else {
                continue;
            };
        if skip_many_unresolved {
            continue;
        }
        let recv = anchored_receiver_expr(&es, ent, cgs, map);
        let rel_expr = format!("{}.{}", recv, id_sym_rel(map, ename, rel.as_str()));
        let rel_desc = if let Some(rel_schema) = ent.relations.get(rel.as_str()) {
            rel_schema.description.as_str().trim()
        } else if let Some(f) = ent.fields.get(rel.as_str()) {
            f.description.as_str().trim()
        } else {
            ""
        };
        let rel_desc_opt = if rel_desc.is_empty() {
            None
        } else {
            Some(truncate_inline_desc(rel_desc, 120))
        };
        let cardinality_many = ent
            .relations
            .get(rel.as_str())
            .map(|r| r.cardinality == Cardinality::Many)
            .unwrap_or(false);
        let result_gloss = crate::result_gloss::result_gloss_for_relation_nav(
            target_entity.as_str(),
            map,
            cardinality_many,
        );
        try_push_domain_example(
            &mut lines,
            &mut line_metas,
            collect_meta,
            cgs,
            map,
            &rel_expr,
            Some(result_gloss),
            rel_desc_opt,
            rel_for_meta,
            None,
            line_valid_cache,
        );
    }

    let heading_projection_bracket = if inline_bracket_onto_primary_get {
        None
    } else {
        primary_get_projection_bracket
    };

    EntityDomainBlock {
        entity_sym: es,
        heading_projection_bracket,
        lines,
        line_metas,
    }
}

/// Count of synthesized DOMAIN example lines for an entity (same pipeline as emission). Used by
/// [`crate::cgs_expression_validate`] so `CGS::validate` fails if this is zero.
pub(crate) fn domain_example_line_count(cgs: &CGS, ename: &str, map: Option<&SymbolMap>) -> usize {
    let mut line_valid_cache = HashMap::new();
    collect_entity_domain_block(cgs, ename, map, None, false, &mut line_valid_cache)
        .lines
        .len()
}

/// Raw DOMAIN lines for an entity (for per-capability witness checks).
#[cfg(test)]
pub(crate) fn domain_example_lines(cgs: &CGS, ename: &str, map: Option<&SymbolMap>) -> Vec<String> {
    let mut line_valid_cache = HashMap::new();
    collect_entity_domain_block(cgs, ename, map, None, false, &mut line_valid_cache).lines
}

/// Primary-get projection bracket for the DOMAIN entity heading (when enabled); test-only helper.
#[cfg(test)]
fn domain_heading_projection_bracket(
    cgs: &CGS,
    ename: &str,
    map: Option<&SymbolMap>,
) -> Option<String> {
    let mut line_valid_cache = HashMap::new();
    collect_entity_domain_block(cgs, ename, map, None, false, &mut line_valid_cache)
        .heading_projection_bracket
}

/// Full scalar projection list `[p#,…]` (heading or **primary get** line); test-only helper.
#[cfg(test)]
fn domain_projection_bracket_exemplar(
    cgs: &CGS,
    ename: &str,
    map: Option<&SymbolMap>,
) -> Option<String> {
    if let Some(b) = domain_heading_projection_bracket(cgs, ename, map) {
        return Some(b);
    }
    for line in domain_example_lines(cgs, ename, map) {
        let head = line
            .split_once(CAP_LEGEND_SEP)
            .map(|(a, _)| a.trim())
            .unwrap_or(line.trim());
        if let Some(b) = parse_trailing_projection_bracket(head) {
            return Some(b);
        }
    }
    None
}

/// Turn a DOMAIN scope variant into the **same shape as a path expression**: bare `e#` when unscoped,
/// else `e#{p#=e#(id),…}` with `*` stripped from scope hints (DOMAIN-only marker).
#[cfg(test)]
pub(crate) fn query_construct_display(es: &str, scope_variant: &str) -> String {
    if scope_variant == es {
        return es.to_string();
    }
    let inner: String = scope_variant
        .split_whitespace()
        .map(|tok| tok.strip_prefix('*').unwrap_or(tok))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{es}{{{inner}}}")
}

/// Marker substring for tests; must appear once at the start of the rendered prompt contract.
pub const DOMAIN_VALID_EXPR_MARKER: &str =
    "This defines valid Plasm syntax for this prompt; reply with one valid plasm_program:";

#[derive(Clone, Copy, Debug)]
struct PromptContractSpec {
    symbolic: bool,
    include_search_line: bool,
    include_rich_string_guidance: bool,
}

/// Render the agent-facing Plasm language guide used by MCP initialize instructions.
///
/// Catalogue-specific `plasm_context` results still provide the active symbol table and may return
/// a narrower frontmatter for the exposed entity slice. This guide is deliberately generated by the
/// same renderer as those per-session TSV frontmatters so MCP tool docs do not carry a hand-written
/// copy of the grammar.
pub fn render_plasm_mcp_language_frontmatter() -> String {
    render_prompt_contract(PromptContractSpec {
        symbolic: true,
        include_search_line: true,
        include_rich_string_guidance: true,
    })
}

fn cgs_slice_has_search_capability(cgs: &CGS, full_entities: &[&str]) -> bool {
    full_entities
        .iter()
        .any(|e| !cgs.find_capabilities(e, CapabilityKind::Search).is_empty())
}

/// True when any string field or capability parameter in the slice uses non-`short` semantics
/// (`markdown`, `html`, `document`, `json_text`, `blob`, …).
fn cgs_slice_has_structured_string_semantics(cgs: &CGS, full_entities: &[&str]) -> bool {
    let full_set: HashSet<&str> = full_entities.iter().copied().collect();
    for &e in full_entities {
        if let Some(ent) = cgs.get_entity(e) {
            for f in ent.fields.values() {
                if matches!(f.field_type, FieldType::Blob) {
                    return true;
                }
                if matches!(f.field_type, FieldType::String)
                    && f.effective_string_semantics().is_structured_or_multiline()
                {
                    return true;
                }
            }
        }
    }
    for cap in cgs.capabilities.values() {
        if !full_set.contains(cap.domain.as_str()) {
            continue;
        }
        let Some(is) = &cap.input_schema else {
            continue;
        };
        let InputType::Object { fields, .. } = &is.input_type else {
            continue;
        };
        for f in fields {
            if matches!(f.field_type, FieldType::Blob) {
                return true;
            }
            if matches!(f.field_type, FieldType::String)
                && f.effective_string_semantics().is_structured_or_multiline()
            {
                return true;
            }
        }
    }
    false
}

fn prompt_contract_spec_resolved<'b, F>(
    resolve: &mut F,
    full_entities: &[&str],
    symbolic: bool,
) -> PromptContractSpec
where
    F: FnMut(&str) -> &'b CGS,
{
    let include_search_line = full_entities.iter().any(|e| {
        let name = *e;
        cgs_slice_has_search_capability(resolve(name), &[name])
    });
    let include_rich_string_guidance = full_entities.iter().any(|e| {
        let name = *e;
        cgs_slice_has_structured_string_semantics(resolve(name), &[name])
    });
    PromptContractSpec {
        symbolic,
        include_search_line,
        include_rich_string_guidance,
    }
}

fn symbolic_entity_form(symbolic: bool) -> &'static str {
    if symbolic {
        "e#"
    } else {
        "Entity"
    }
}

fn symbolic_method_form(symbolic: bool) -> &'static str {
    if symbolic {
        "m#"
    } else {
        "method"
    }
}

fn symbolic_field_form(symbolic: bool) -> &'static str {
    if symbolic {
        "p#"
    } else {
        "field"
    }
}

fn render_prompt_contract(spec: PromptContractSpec) -> String {
    let entity = symbolic_entity_form(spec.symbolic);
    let method = symbolic_method_form(spec.symbolic);
    let field = symbolic_field_form(spec.symbolic);
    let projection = if spec.symbolic {
        "[p#,…]"
    } else {
        "[field,…]"
    };
    let get_form = if spec.symbolic {
        "e#(id) or e#(p#=v, p#=v)"
    } else {
        "Entity(id) or Entity(name=value, name=value)"
    };
    let query_form = if spec.symbolic {
        "e#{p#=v, …}"
    } else {
        "Entity{name=value, …}"
    };
    let query_all_form = entity;
    let nav_form = if spec.symbolic {
        "e#(id).p#"
    } else {
        "Entity(id).field"
    };
    let method_form = if spec.symbolic {
        "e#(id).m#() or e#(id).m#(p#=v,..)"
    } else {
        "Entity(id).method() or Entity(id).method(name=value,..)"
    };
    let create_form = if spec.symbolic {
        "e#.m#(args)"
    } else {
        "Entity.method(args)"
    };
    let projection_form = if spec.symbolic {
        "e#(id)[p#,…]"
    } else {
        "Entity(id)[field,…]"
    };
    let scoped_form = if spec.symbolic {
        "X{p#=AnchorEntity(id)}"
    } else {
        "X{scope_param=AnchorEntity(id)}"
    };
    let array_form = if spec.symbolic {
        "p#=[e#($)]"
    } else {
        "name=[Entity($)]"
    };
    let search_form = if spec.symbolic {
        "e#~\"text\""
    } else {
        "Entity~\"text\""
    };

    let entity_expr_rhs: &str = if spec.include_search_line {
        "query_all | get | query | relation | method | create_action | search"
    } else {
        "query_all | get | query | relation | method | create_action"
    };

    let mut s = String::new();
    s.push_str(DOMAIN_VALID_EXPR_MARKER);
    s.push_str("\n\n");

    s.push_str("Output:\n");
    s.push_str(
        "- Single `plasm_expr` iff one lookup/query/search/relation/method/action is the whole answer.\n\
- Else one multi-line `plasm_program`: bind → narrow (projection/postfix/bracket-render) → small bare roots—not many one-line tool calls.\n\n",
    );

    let _ = writeln!(
        s,
        "Grammar (pseudo-EBNF; DOMAIN rows instantiate `plasm_expr` atoms):"
    );
    let _ = writeln!(s, "plasm_program ::= plasm_roots | binding+ plasm_roots");
    let _ = writeln!(s, "binding       ::= ident \"=\" plasm_node");
    let _ = writeln!(s, "plasm_roots   ::= plasm_return (\",\" plasm_return)*");
    let _ = writeln!(s, "plasm_return  ::= node_ref | plasm_expr");
    let _ = writeln!(
        s,
        "plasm_node    ::= ( plasm_expr | node_ref ) postfix* bracket_render_tail? | node_ref \"=>\" plasm_value | node_ref \"=>\" effect_expr"
    );
    let _ = writeln!(s, "plasm_expr    ::= entity_expr [projection]");
    let _ = writeln!(s, "entity_expr   ::= {}", entity_expr_rhs);
    let _ = writeln!(s, "query_all     ::= {}", query_all_form);
    let _ = writeln!(s, "get           ::= {}", get_form);
    let _ = writeln!(s, "query         ::= {}", query_form);
    let _ = writeln!(s, "relation      ::= {}", nav_form);
    let _ = writeln!(s, "method        ::= {}", method_form);
    let _ = writeln!(s, "create_action ::= {}", create_form);
    if spec.include_search_line {
        let _ = writeln!(s, "search        ::= {}", search_form);
    }
    let _ = writeln!(
        s,
        "projection    ::= {} | \"[\" fields \"]\"",
        projection_form
    );
    let _ = writeln!(s, "postfix       ::= transform | \"[\" fields \"]\"");
    let _ = writeln!(
        s,
        "bracket_render_tail ::= ( \"[\" fields \"]\" )? \"<<TAG\" heredoc_body \"TAG\""
    );
    let _ = writeln!(s, "positive_int  ::= non-zero decimal integer literal");
    let _ = writeln!(s, "agg_func      ::= \"sum\" | \"avg\" | \"min\" | \"max\"");
    let _ = writeln!(
        s,
        "agg_spec      ::= ident \"=\" ( \"count\" | agg_func \"(\" field \")\" )"
    );
    let _ = writeln!(s, "agg_specs     ::= agg_spec (\",\" agg_spec)*");
    let _ = writeln!(
        s,
        "sort_dir      ::= omitted | \"desc\" | \"asc\" | \"ascending\" | \"descending\""
    );
    let _ = writeln!(
        s,
        "transform     ::= \".limit(\" positive_int \")\" | \".sort(\" field [\",\" sort_dir] \")\" | \".aggregate(\" agg_specs \")\" | \".group_by(\" field \",\" agg_specs \")\" | \".singleton()\" | \".page_size(\" positive_int \")\""
    );
    let _ = writeln!(s, "fields        ::= {}", projection);
    let _ = writeln!(
        s,
        "plasm_value   ::= literal | node_ref | node_ref.field | _.field | [v, …]"
    );
    let _ = writeln!(
        s,
        "effect_expr   ::= taught write/update/delete/action surface from DOMAIN (same line may template rows)"
    );
    let _ = writeln!(
        s,
        "ident/node_ref/field ::= agent-chosen names for bound nodes and fields"
    );
    let _ = writeln!(
        s,
        "literal       ::= quoted string | number | bool | null | heredoc"
    );
    s.push_str("\nAggregates (examples): `.aggregate(n=count)`, `.aggregate(total=sum(amount), lo=min(price))`, `.group_by(author, n=count)`.\n\n");

    s.push_str("Continuation:\n");
    s.push_str(
        "- Postfix (transforms, `[fields]`, `<<TAG…TAG`) may chain on one surface `plasm_expr`.\n\
- `ident.<relation>` only after surface row bindings (get/query/relation); after projection/transform/`=>`, repeat full DOMAIN expr or rebind. Bracket-render = Minijinja `rows`; optional `[fields]` narrows columns → row `{\"content\":\"…\"}` — use **`binding.content`** for string/body args.\n\n",
    );

    s.push_str("MCP `program` (JSON string):\n");
    s.push_str(
        "- Literal U+000A between statements (one `ident = …` per line; bare roots last). Spaces never separate statements.\n\
- Heredoc: newline after `<<TAG` opener; close line trims to `TAG` / `TAG)` / `TAG,` / `TAG}`; first matching trimmed interior line closes—opaque `TAG` for blobs. Commas inside heredocs never split roots (e.g. `…<<B` / `Hello` / `B` / `), e2`).\n\n",
    );

    s.push_str("Discipline:\n");
    s.push_str(
        "- Plan shape first; small roots unless raw rows requested; bracket-render row-derived text, static `<<TAG` for fixed payloads.\n\
- `page(sN_pgM)` continues a chosen list only; DOMAIN is the shape contract—no probe calls.\n\n",
    );

    s.push_str("DOMAIN / catalogue:\n");
    let _ = writeln!(
        s,
        "- `plasm_expr` = atoms; `Meaning` = how to pick/fill—never paste `Meaning`. Bare comma-separated roots (no `return`); every symbol taught here."
    );
    let _ = writeln!(
        s,
        "- Projection: minimal non-empty `{}`; identity row lists full fields once. Leading `{field}` rows = slot metadata when method `args:` is insufficient.",
        projection,
        field = field
    );

    if spec.symbolic {
        s.push_str(
            "- e#, m#, p# are session-local aliases — copy them exactly; new prompts supersede old meanings.\n\
- Pick by `Meaning`, copy `plasm_expr`, bind keyed slot (`p12=value`) only; never positional or renumbered args.\n",
        );
    } else {
        s.push_str(
            "- Entity names, methods, and slots — use exactly as shown; a slot may be field, relation, or parameter by context.\n",
        );
    }

    s.push_str(&format!(
        "- Instantiate grammar lines from DOMAIN only; keyed `{field}=v` never positional. `v=$` in tables is a fill cue.\n\
- `{query_form}` / `{query_all_form}` list/filter; `{get_form}` fetch one; `{nav_form}` relate; `{method_form}` call; `{create_form}` without id; `{projection_form}` projects—after `{entity}(id)`, `.` is relation or taught `{method}` only.\n",
        field = field,
        entity = entity,
        method = method,
        query_form = query_form,
        query_all_form = query_all_form,
        get_form = get_form,
        nav_form = nav_form,
        method_form = method_form,
        create_form = create_form,
        projection_form = projection_form,
    ));
    let _ = writeln!(
        s,
        "- Scoped child lists: `{scoped_form}` from the child entity’s DOMAIN rows; never invent `Y(id).{field}`.",
        field = field
    );
    let _ = writeln!(
        s,
        "- Arrays in args: `[v, …]` (e.g. `{array_form}`). Quoted strings: only `\\\"` and `\\\\` escapes."
    );
    s.push_str("- `select` one allowed value; `multiselect` / multi-value lists as `[v, …]`.\n");
    if spec.include_rich_string_guidance {
        s.push_str(&render_rich_string_guidance_tsv());
    }
    if spec.include_search_line {
        let _ = writeln!(
            s,
            "- Search `{search_form}` only where DOMAIN teaches `~` on that entity.",
            search_form = search_form
        );
    }
    let _ = writeln!(
        s,
        "- `{field}`-only rows declare slots (metadata), not expressions to emit.",
        field = field
    );
    let _ = writeln!(
        s,
        "- `$` marks fill-ins — substitute real values; never send `$`."
    );
    let _ = writeln!(
        s,
        "- `..` — optional trailing params (`optional params:` lists them); `..` alone means all args optional."
    );
    s.push('\n');

    s
}

/// Heredoc rules for TSV prompts: same semantics as markdown — one minimal tagged exemplar.
fn render_rich_string_guidance_tsv() -> String {
    "- `markdown`/`html`/`document`/`json_text`/`blob` values (per `Meaning`): `<<TAG` … `TAG` only; e.g. `m#(..., p#=<<TXT` + newline body + `TXT` newline `)`.\n"
        .to_string()
}

fn comment_prefix_block(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if line.is_empty() {
            out.push_str("#\n");
        } else {
            let _ = writeln!(out, "# {line}");
        }
    }
    out
}

/// True when `sym` is the **terminal** relation segment (`… .p#`) and the DOMAIN line already carries
/// a `;;  => …` result gloss — the standalone `p#  ;;  => e# · Target` line would duplicate that.
fn skip_redundant_terminal_relation_sym_gloss(
    full_line: &str,
    sym: &str,
    meta: &crate::symbol_tuning::IdentMetadata,
) -> bool {
    let relation_like = matches!(meta.role, crate::symbol_tuning::IdentRole::Relation { .. })
        || matches!(meta.field_type, FieldType::EntityRef { .. });
    if !relation_like {
        return false;
    }
    let Some((expr_leg, legend_leg)) = full_line.split_once(CAP_LEGEND_SEP) else {
        return false;
    };
    let expr = crate::symbol_tuning::strip_prompt_expression_annotations(expr_leg.trim());
    let expr = expr.trim_end();
    let Some((_, last_seg)) = expr.rsplit_once('.') else {
        return false;
    };
    if last_seg != sym {
        return false;
    }
    let legend = legend_leg.trim();
    legend.starts_with("=>")
}

/// Emit `p#  ;;  …` lines for each field symbol’s **first** appearance in `line` (expression + `;;` legend, e.g. `optional params: p18, p19`).
fn emit_field_def_lines_before_example(
    out: &mut String,
    line: &str,
    map: &SymbolMap,
    entity: &str,
    catalog_entry_id: &str,
    ident_meta: &HashMap<IdentMetaKey, IdentMetadata>,
    defined: &mut HashMap<String, IdentMetadata>,
) {
    let en = EntityName::from(entity.to_string());
    let cid = catalog_entry_id.to_string();
    for sym in crate::symbol_tuning::field_syms_for_domain_line(line) {
        let field_name = map.resolve_ident(&sym).unwrap_or(&sym);
        let meta = map
            .capability_param_key_for_p_sym(&sym)
            .as_ref()
            .and_then(|(dom, w)| ident_meta.get(&(cid.clone(), dom.clone(), w.clone())))
            .or_else(|| ident_meta.get(&(cid.clone(), en.clone(), field_name.to_string())));
        let should_emit = match (meta, defined.get(&sym)) {
            (Some(m), None) => {
                defined.insert(sym.clone(), m.clone());
                true
            }
            (Some(m), Some(prev))
                if prev.field_type != m.field_type
                    || prev.string_semantics != m.string_semantics
                    || prev.array_items != m.array_items
                    || prev.allowed_values != m.allowed_values
                    || prev.role != m.role
                    || prev.description.trim() != m.description.trim() =>
            {
                defined.insert(sym.clone(), m.clone());
                true
            }
            (None, None) => {
                defined.insert(
                    sym.clone(),
                    IdentMetadata {
                        catalog_entry_id: cid.clone(),
                        field_type: FieldType::String,
                        string_semantics: None,
                        array_items: None,
                        allowed_values: None,
                        role: crate::symbol_tuning::IdentRole::EntityField,
                        wire_name: field_name.to_string(),
                        description: field_name.to_string(),
                        entity: en.clone(),
                    },
                );
                true
            }
            _ => false,
        };
        if should_emit {
            if let Some(m) = meta {
                if skip_redundant_terminal_relation_sym_gloss(line, sym.as_str(), m) {
                    defined.remove(&sym);
                    continue;
                }
                if matches!(
                    m.role,
                    crate::symbol_tuning::IdentRole::CapabilityParam { .. }
                ) {
                    if let Some(sup) =
                        crate::symbol_tuning::args_line_suppressible_capability_syms(line)
                    {
                        if sup.get(sym.as_str()) == Some(&true) {
                            defined.remove(&sym);
                            continue;
                        }
                    }
                }
            }
            let gloss = match meta {
                Some(m) => m.render_gloss(Some(map)),
                None => field_name.to_string(),
            };
            let _ = writeln!(out, "    {}{}{}", sym, CAP_LEGEND_SEP, gloss);
        }
    }
}

/// Per-entity many-shot examples — `focus` still subsets *which* entities appear.
#[allow(clippy::too_many_arguments)]
fn render_domain_table_resolved<'b, F>(
    mut resolve: F,
    full_entities: &[&str],
    map: Option<&SymbolMap>,
    exposure_for_ident: Option<&DomainExposureSession>,
    out: &mut String,
    model_out: &mut Vec<EntityDomainPrompt>,
    fill_model: bool,
    _include_contract_preamble: bool,
    emit_entity_blocks: Option<&[&str]>,
) where
    F: FnMut(&str) -> &'b CGS,
{
    let ident_meta = match (map, exposure_for_ident) {
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

    let mut defined_field_syms: HashMap<String, IdentMetadata> = HashMap::new();
    let mut line_valid_cache: HashMap<DomainLineValidCacheKey, bool> = HashMap::with_capacity(8192);

    let block_iter: Vec<&str> = if let Some(e) = emit_entity_blocks {
        e.to_vec()
    } else {
        full_entities.to_vec()
    };

    for &ename in &block_iter {
        let cgs = resolve(ename);
        let mut seen_expr: HashSet<String> = HashSet::new();
        let collect_meta = fill_model;
        let block = collect_entity_domain_block(
            cgs,
            ename,
            map,
            ident_meta.as_ref(),
            collect_meta,
            &mut line_valid_cache,
        );
        if block.lines.is_empty() {
            debug_assert!(
                false,
                "DOMAIN block empty for entity {ename} — CGS::validate should have rejected this via cgs_expression_validate"
            );
            tracing::warn!(
                target: "plasm_core::prompt_render",
                entity = ename,
                "empty DOMAIN block; schema should have failed CGS::validate"
            );
            continue;
        }
        let ent_desc = cgs
            .get_entity(ename)
            .map(|ent| ent.description.as_str().trim())
            .filter(|d| !d.is_empty())
            .map(|d| truncate_inline_desc(d, 200));
        let heading_line = format_entity_domain_heading_line(
            &block.entity_sym,
            block.heading_projection_bracket.as_deref(),
            ent_desc.as_deref(),
        );
        if let (Some(m), Some(meta)) = (map, ident_meta.as_ref()) {
            let catalog_entry_id = cgs.entry_id.as_deref().unwrap_or("");
            emit_field_def_lines_before_example(
                out,
                &heading_line,
                m,
                ename,
                catalog_entry_id,
                meta,
                &mut defined_field_syms,
            );
        }
        let _ = writeln!(out, "  {}", heading_line);
        let mut emitted_metas: Vec<DomainLineMeta> = Vec::new();
        for (i, line) in block.lines.iter().enumerate() {
            if seen_expr.insert(line.clone()) {
                if collect_meta {
                    if let Some(m) = block.line_metas.get(i) {
                        emitted_metas.push(m.clone());
                    }
                }
                if let (Some(m), Some(meta)) = (map, ident_meta.as_ref()) {
                    let catalog_entry_id = cgs.entry_id.as_deref().unwrap_or("");
                    emit_field_def_lines_before_example(
                        out,
                        line,
                        m,
                        ename,
                        catalog_entry_id,
                        meta,
                        &mut defined_field_syms,
                    );
                }
                let _ = writeln!(out, "    {line}");
            }
        }
        if fill_model {
            model_out.push(EntityDomainPrompt {
                entity: block.entity_sym,
                lines: emitted_metas,
            });
        }
    }
}

/// Per-entity many-shot examples using a single [`CGS`].
#[allow(clippy::too_many_arguments)]
fn render_domain_table(
    cgs: &CGS,
    full_entities: &[&str],
    map: Option<&SymbolMap>,
    out: &mut String,
    model_out: &mut Vec<EntityDomainPrompt>,
    fill_model: bool,
    include_contract_preamble: bool,
    emit_entity_blocks: Option<&[&str]>,
) {
    render_domain_table_resolved(
        |_| cgs,
        full_entities,
        map,
        None,
        out,
        model_out,
        fill_model,
        include_contract_preamble,
        emit_entity_blocks,
    );
}

/// `p#  ;;  …` field gloss lines (not Plasm expressions).
#[cfg(test)]
fn is_field_gloss_line(trimmed: &str) -> bool {
    let Some(rest) = trimmed.strip_prefix('p') else {
        return false;
    };
    let mut len = 0usize;
    for c in rest.chars() {
        if c.is_ascii_digit() {
            len += c.len_utf8();
        } else {
            break;
        }
    }
    if len == 0 {
        return false;
    }
    rest[len..].trim_start().starts_with(";;")
}

/// Extract expression strings from the rendered DOMAIN section: **tsv** uses the `plasm_expr` column
/// after the `plasm_expr\tMeaning` header.
#[cfg(test)]
fn example_expressions_from_prompt(prompt: &str) -> Vec<String> {
    if prompt.contains(TSV_DOMAIN_TABLE_HEADER) {
        return example_expressions_from_prompt_tsv(prompt);
    }
    let mut out = Vec::new();
    let mut in_domain = false;
    for line in prompt.lines() {
        if line.contains(DOMAIN_VALID_EXPR_MARKER) {
            in_domain = true;
            continue;
        }
        if in_domain {
            if line.trim_start().starts_with("---") {
                break;
            }
            let t = line.trim_start();
            if t.starts_with("--") {
                continue;
            }
            if t.starts_with('(') {
                continue;
            }
            // Plasm examples live under `    ` (four-space indent under each entity header).
            if !line.starts_with("    ") {
                continue;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if is_field_gloss_line(trimmed) {
                continue;
            }
            let expr_only = crate::symbol_tuning::strip_prompt_expression_annotations(trimmed);
            if !expr_only.is_empty() {
                out.push(expr_only);
            }
        }
    }
    out
}

#[cfg(test)]
fn is_tsv_expression_column_slot_def(expr_cell: &str) -> bool {
    let s = expr_cell.trim();
    let Some(rest) = s.strip_prefix('p') else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
fn example_expressions_from_prompt_tsv(prompt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_table = false;
    for line in prompt.lines() {
        if line == TSV_DOMAIN_TABLE_HEADER.trim_end() {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        if line.trim_start().starts_with("---") {
            break;
        }
        let Some((expr_cell, _meaning)) = line.split_once('\t') else {
            continue;
        };
        let trimmed = expr_cell.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_tsv_expression_column_slot_def(trimmed) {
            continue;
        }
        let expr_only = crate::symbol_tuning::strip_prompt_expression_annotations(trimmed);
        if !expr_only.is_empty() {
            out.push(expr_only);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use crate::prompt_pipeline::PromptPipelineConfig;
    use crate::schema::{
        CapabilityMapping, CapabilitySchema, FieldSchema, RelationSchema, ResourceSchema,
    };
    use crate::symbol_tuning::{
        entity_slices_for_render, resolve_prompt_surface_entities, symbol_map_for_prompt,
        DomainExposureSession, FocusSpec,
    };
    use crate::CapabilityKind;
    use crate::Cardinality;
    use crate::FieldType;
    use crate::CGS;

    /// [`Path::new`] relative segments are resolved against the **test process** current
    /// directory, which is not always `crates/plasm-core` (e.g. it may be a workspace root).
    /// Build paths from [`CARGO_MANIFEST_DIR`] so `apis/…` and `fixtures/…` resolve correctly in
    /// `cargo test` and CI the same as local `cd plasm-oss && cargo test`.
    fn repo_path(components: &[&str]) -> std::path::PathBuf {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for c in components {
            p.push(c);
        }
        p
    }
    fn apis_dir(name: &str) -> std::path::PathBuf {
        repo_path(&["..", "..", "apis", name])
    }
    fn fixtures_schemas_dir(name: &str) -> std::path::PathBuf {
        repo_path(&["..", "..", "fixtures", "schemas", name])
    }

    /// Insta resolves the default `snapshots/` path from `file!()`. In the parent
    /// `plasm/` virtual workspace, path remaps can make that resolve under a spurious
    /// `plasm-oss/plasm-oss/...` tree, so the committed `.snap` is not found. Anchor to
    /// [`CARGO_MANIFEST_DIR`], which is always the `plasm-core` crate root.
    fn with_insta_snapshots<R>(f: impl FnOnce() -> R) -> R {
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_path(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/snapshots"),
        );
        settings.bind(f)
    }

    #[test]
    fn redundant_relation_sym_gloss_skipped_for_terminal_chain_line() {
        use crate::symbol_tuning::{IdentMetadata, IdentRole};
        use crate::EntityName;
        let user = EntityName::from("User".to_string());
        let issue = EntityName::from("Issue".to_string());
        let rel_meta = IdentMetadata {
            catalog_entry_id: String::new(),
            field_type: FieldType::EntityRef {
                target: user.clone(),
            },
            string_semantics: None,
            array_items: None,
            allowed_values: None,
            role: IdentRole::Relation {
                target: user.clone(),
            },
            wire_name: "reporter".into(),
            description: String::new(),
            entity: issue.clone(),
        };
        assert!(skip_redundant_terminal_relation_sym_gloss(
            "e5(p64=$, p80=$, p59=$).p101  ;;  => e18",
            "p101",
            &rel_meta
        ));
        assert!(!skip_redundant_terminal_relation_sym_gloss(
            "e5{p101=$, p64=$, p80=$}  ;;  => [e5]",
            "p101",
            &rel_meta
        ));
        let title_meta = IdentMetadata {
            catalog_entry_id: String::new(),
            field_type: FieldType::String,
            string_semantics: None,
            array_items: None,
            allowed_values: None,
            role: IdentRole::EntityField,
            wire_name: "title".into(),
            description: String::new(),
            entity: issue,
        };
        assert!(!skip_redundant_terminal_relation_sym_gloss(
            "e5(p64=$, p80=$, p59=$)[p96]  ;;  => [p96]",
            "p96",
            &title_meta
        ));
    }

    #[test]
    fn bundled_github_petstore_clickup_full_entities_emit_domain_lines() {
        for p in [
            apis_dir("github"),
            fixtures_schemas_dir("petstore"),
            apis_dir("clickup"),
        ] {
            if !p.exists() {
                continue;
            }
            let cgs = load_schema_dir(&p).unwrap_or_else(|e| panic!("load {}: {e}", p.display()));
            let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
            let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true);
            for ename in &full {
                let n = domain_example_line_count(&cgs, ename, map.as_ref());
                assert!(
                    n > 0,
                    "{}: entity `{ename}` is in full_entities but collect_entity_domain_block emitted no lines",
                    p.display()
                );
            }
        }
    }

    #[test]
    fn google_sheets_compound_get_entity_ref_key_var_emits_valid_domain_line() {
        let dir = apis_dir("google-sheets");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let lines = domain_example_lines(&cgs, "ValueRange", None);
        let expected = "ValueRange(spreadsheetId=Spreadsheet($), range=$)";
        assert!(
            lines.iter().any(|l| l.starts_with(expected)),
            "missing compound dotted-call-safe get witness for entity_ref key var: expected prefix `{expected}` in {:?}",
            lines
        );
        let work = domain_line_work_string(expected, None);
        assert!(
            domain_line_valid_work(&cgs, &work),
            "expected synthesized compound get witness to parse+typecheck: `{expected}`"
        );
    }

    /// Regression: Issue DOMAIN teaches **one** full scalar projection list (all `provides` fields),
    /// on the **identity** primary get or on the heading when the get is singleton-only — not a
    /// prefix ladder or a duplicate extra exemplar line.
    #[test]
    fn github_issue_domain_emits_single_full_projection_exemplar() {
        let dir = apis_dir("github");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let Some(ent) = cgs.get_entity("Issue") else {
            panic!("missing Issue entity");
        };
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true);
        let prefixes = cgs.projection_prompt_field_prefixes("Issue", ent);
        assert_eq!(
            prefixes.len(),
            1,
            "expected one full projection exemplar vector; got {}",
            prefixes.len()
        );
        assert!(
            prefixes[0].len() >= 10,
            "Issue primary get should expose many response fields for teaching; got {}",
            prefixes[0].len()
        );
        let br = domain_projection_bracket_exemplar(&cgs, "Issue", map.as_ref())
            .expect("Issue should carry a full projection bracket (heading or primary get)");
        assert!(
            br.starts_with('[') && br.contains('p'),
            "unexpected projection bracket: {br}"
        );
        let lines = domain_example_lines(&cgs, "Issue", map.as_ref());
        let bracket_lines = lines
            .iter()
            .filter(|l| l.contains("[p") && l.contains(']'))
            .count();
        assert_eq!(
            bracket_lines, 1,
            "expect exactly one DOMAIN example line with a full scalar projection list (bracket_lines={})",
            bracket_lines,
        );
        let out = render_prompt_with_config(
            &cgs,
            RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
        );
        assert!(
            out.contains(br.as_str()),
            "full prompt should include the full projection list `{br}` (heading or primary get)"
        );
        assert!(
            out.len() > 8_000,
            "full apis/github DOMAIN+legend should be substantial (got {}); compare `github_api_full_prompt_symbolic` snapshot",
            out.len()
        );
    }

    /// Linear uses zero-arity method-style Get exemplars (`e2.m8()`); heading projection must still
    /// teach scalar fields from `issue_get.provides` (see [`CGS::domain_projection_heading_fields`]).
    #[test]
    fn linear_issue_heading_projection_despite_method_style_get() {
        let dir = apis_dir("linear");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true);
        let br = domain_projection_bracket_exemplar(&cgs, "Issue", map.as_ref())
            .expect("Linear Issue should carry a full projection bracket (heading or primary get)");
        assert!(
            br.starts_with('[') && br.contains('p'),
            "unexpected projection bracket: {br}"
        );
        let lines = domain_example_lines(&cgs, "Issue", map.as_ref());
        let bracket_lines = lines
            .iter()
            .filter(|l| l.contains("[p") && l.contains(']'))
            .count();
        assert_eq!(
            bracket_lines, 1,
            "expect exactly one DOMAIN example line with a full scalar projection list (bracket_lines={})",
            bracket_lines,
        );
        let out = render_prompt_with_config(
            &cgs,
            RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
        );
        assert!(
            out.contains(br.as_str()),
            "full prompt should include the full projection list `{br}` (heading or primary get)"
        );
    }

    #[test]
    fn heading_projection_symbols_are_declared_before_heading_use() {
        let dir = apis_dir("github");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true);
        let br = domain_projection_bracket_exemplar(&cgs, "Issue", map.as_ref())
            .expect("Issue should carry a projection list");
        let out = render_prompt_with_config(
            &cgs,
            RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
        );
        let lines: Vec<&str> = out.lines().collect();
        let use_idx = lines
            .iter()
            .position(|l| l.contains(br.as_str()))
            .expect("full projection list should appear on heading or primary get line");
        let inner = br
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .expect("bracket");
        let symbols: Vec<&str> = inner
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        assert!(
            !symbols.is_empty(),
            "Issue projection should include at least one p# symbol"
        );
        for sym in symbols {
            let def = format!("{sym}\t");
            let def_idx = lines
                .iter()
                .position(|l| l.starts_with(&def))
                .unwrap_or_else(|| panic!("missing gloss definition line for `{sym}`"));
            assert!(
                def_idx < use_idx,
                "projection symbol `{sym}` must be declared before the line that uses the list (def_idx={def_idx}, use_idx={use_idx})"
            );
        }
    }

    #[test]
    fn tsv_additive_wave_omits_global_contract_but_keeps_column_header() {
        let dir = fixtures_schemas_dir("petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let pipeline = PromptPipelineConfig::default();
        let mut exp = DomainExposureSession::new(&cgs, "", &["Pet"]);
        let first = pipeline.render_domain_first_wave_for_session(&cgs, &exp, None);
        assert!(
            first.lines().any(|l| l.contains(DOMAIN_VALID_EXPR_MARKER)),
            "initial teaching TSV should include global contract marker"
        );
        let (c, table) = split_tsv_domain_contract_and_table(&first);
        assert!(
            c.is_some()
                && c.as_ref()
                    .is_some_and(|s| s.contains(DOMAIN_VALID_EXPR_MARKER)),
            "split should return contract block"
        );
        assert!(
            table.starts_with(TSV_DOMAIN_TABLE_HEADER),
            "table body should start with plasm_expr/Meaning"
        );
        let (c2, t2) = split_tsv_domain_contract_and_table(&table);
        assert_eq!(c2, None, "table-only TSV has no contract prefix");
        assert_eq!(t2, table);
        exp.expose_entities(&[&cgs], std::sync::Arc::new(cgs.clone()), "", &["Order"]);
        let delta = pipeline.render_domain_exposure_delta(&cgs, &exp, &["Order"], None);
        assert!(
            !delta.contains(DOMAIN_VALID_EXPR_MARKER),
            "additive TSV must not repeat global contract comments"
        );
        assert!(
            delta.contains(TSV_DOMAIN_TABLE_HEADER.trim_end()),
            "additive TSV should keep column header"
        );
    }

    #[test]
    fn split_tsv_domain_contract_and_table_table_only() {
        let t = "plasm_expr\tMeaning\na\tb\n";
        let (c, b) = split_tsv_domain_contract_and_table(t);
        assert_eq!(c, None);
        assert_eq!(b, t);
    }

    #[test]
    fn split_tsv_domain_contract_and_table_with_comment_prefix() {
        let t = "# Plasm contract line\n# second\n\nplasm_expr\tMeaning\na\tb\n";
        let (c, b) = split_tsv_domain_contract_and_table(t);
        assert_eq!(c.as_deref(), Some("# Plasm contract line\n# second"));
        assert_eq!(b, "plasm_expr\tMeaning\na\tb\n");
    }

    /// Regression: TSV `p#` gloss rows must use [`IdentMetadata`] for the entity owning the DOMAIN
    /// block, not `full_entities[idx]` by YAML insertion order (symbolic bundle uses sorted
    /// [`DomainExposureSession::entities`]). Overshow has `RecordedContent.id` (string) and
    /// `CaptureItem.id` (integer); mis-alignment produced `str · id` for CaptureItem's block.
    #[test]
    fn tsv_symbolic_blocks_align_ident_gloss_with_exposure_entity_order() {
        let dir = fixtures_schemas_dir("overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let (names, _) = resolve_prompt_surface_entities(&cgs, FocusSpec::All, true);
        assert_eq!(
            names.first().map(|s| s.as_str()),
            Some("CaptureItem"),
            "exposure order should sort entities alphabetically; CaptureItem first"
        );
        let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let after_header = tsv
            .split(TSV_DOMAIN_TABLE_HEADER)
            .nth(1)
            .expect("tsv plasm_expr/Meaning table");
        let first_block: String = after_header
            .lines()
            .take_while(|l| {
                let t = l.trim_start();
                !(t.starts_with("e2.") || t.starts_with("e2("))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let id_gloss = first_block
            .lines()
            .find(|l| {
                let mut cols = l.split('\t');
                let Some(sym) = cols.next() else {
                    return false;
                };
                let Some(meaning) = cols.next() else {
                    return false;
                };
                sym.starts_with('p') && meaning.contains("int")
            })
            .unwrap_or_else(|| {
                panic!(
                    "expected a p# gloss row for CaptureItem integer `id` in first block, got:\n{first_block}"
                );
            });
        assert!(
            id_gloss.contains("int"),
            "CaptureItem `id` should gloss as int, not str from mis-paired entity: {id_gloss:?}"
        );
    }

    /// `Profile.recorded_matches` targets `RecordedContent`, which has Search/Query but no Get — DOMAIN
    /// must still teach `e7($).p#` chain nav for `query_scoped` many relations.
    #[test]
    fn overshow_tsv_includes_query_scoped_profile_relation_nav() {
        let dir = fixtures_schemas_dir("overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        assert!(
            tsv.lines()
                .any(|l| { l.contains("e7($).p") && l.contains("Content scoped to this profile") }),
            "expected Profile → RecordedContent relation nav line; e7 lines:\n{}",
            tsv.lines()
                .filter(|l| l.contains("e7"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// Regression: compound-key `CaptureItem` get witness must be taught (covers `capture_item_get`).
    #[test]
    fn overshow_tsv_includes_compound_capture_item_get_witness() {
        let dir = fixtures_schemas_dir("overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let p_id = map.ident_sym_entity_field("CaptureItem", "id");
        let p_ct = map.ident_sym_entity_field("CaptureItem", "content_type");
        assert!(
            tsv.lines().any(|line| {
                line.starts_with("e1(")
                    && line.contains(&format!("{p_id}=$"))
                    && line.contains(&format!("{p_ct}=$"))
                    && line.contains("returns e1")
            }),
            "expected compound-key capture-item get witness in TSV; e1 lines:\n{}",
            tsv.lines()
                .filter(|l| l.starts_with("e1"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn tsv_prompt_uses_plasm_expr_and_meaning_columns() {
        let dir = apis_dir("github");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let mut lines = tsv.lines();
        let first = lines.next().expect("tsv frontmatter");
        assert!(
            first.starts_with("# ") && first.contains(DOMAIN_VALID_EXPR_MARKER),
            "TSV output should begin with comment-prefixed frontmatter"
        );
        let header = tsv
            .lines()
            .find(|line| *line == TSV_DOMAIN_TABLE_HEADER.trim_end())
            .expect("tsv header");
        assert_eq!(header, TSV_DOMAIN_TABLE_HEADER.trim_end());
        let issue_identity = tsv
            .lines()
            .find(|l| {
                let cols: Vec<&str> = l.split('\t').collect();
                cols.len() == 2 && cols[0].starts_with("e5(") && cols[1].contains("GitHub issue")
            })
            .expect("Issue identity row");
        let cols: Vec<&str> = issue_identity.split('\t').collect();
        assert_eq!(cols.len(), 2, "identity row should have 2 columns");
        assert!(cols[0].starts_with("e5("));
        assert!(
            (cols[0].contains('[') && cols[0].contains(']'))
                || (cols[1].contains("projection [") && cols[1].contains('p')),
            "identity row should teach full projection on the plasm_expr get line, or in Meaning if the list is not on the expression: row={issue_identity:?}"
        );
        if cols[0].contains('[') {
            assert!(
                !cols[1].contains("projection ["),
                "do not repeat full projection in Meaning when plasm_expr already carries the list"
            );
        }
        assert!(
            cols[1].contains("GitHub issue"),
            "identity row description should carry entity prose"
        );
        let select_row = tsv
            .lines()
            .find(|l| {
                let cols: Vec<&str> = l.split('\t').collect();
                cols.len() == 2
                    && cols[0].starts_with('p')
                    && cols[1] == "select · allowed: open, closed"
            })
            .expect("Issue state select field row");
        let select_cols: Vec<&str> = select_row.split('\t').collect();
        assert_eq!(select_cols.len(), 2);
        assert_eq!(select_cols[1], "select · allowed: open, closed");
        let body = tsv
            .lines()
            .skip_while(|line| *line != TSV_DOMAIN_TABLE_HEADER.trim_end())
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !body.contains(";;") && !body.contains("=>"),
            "2-column TSV surface should remove compact gloss tokens entirely"
        );
        let owner_slot_rows: Vec<&str> = tsv
            .lines()
            .filter(|l| {
                l.split('\t').nth(1).is_some_and(|m| {
                    m.contains("owner") && (m.contains("str") || m.contains("int"))
                })
            })
            .collect();
        assert!(
            owner_slot_rows.iter().any(|row| row.starts_with('p')),
            "expected a p# slot row whose Meaning mentions owner (wire field), got {owner_slot_rows:?}"
        );
        let issue_comment_create_row = tsv
            .lines()
            .find(|l| {
                let cols: Vec<&str> = l.split('\t').collect();
                cols.len() == 2
                    && cols[0].contains(".m")
                    && cols[0].starts_with('e')
                    && cols[1].to_lowercase().contains("comment")
            })
            .expect("IssueComment invoke DOMAIN row");
        assert!(
            issue_comment_create_row.contains("[scope")
                || issue_comment_create_row.contains("scope"),
            "invoke row should reference scoping, got {issue_comment_create_row:?}"
        );
        let contrib = tsv
            .lines()
            .find(|l| {
                let m = l.to_lowercase();
                m.contains("contributors")
                    && m.contains("commit count")
                    && (m.contains("repo") || m.contains("repository"))
            })
            .expect("Contributor list DOMAIN row");
        assert!(
            contrib.starts_with('e') && contrib.contains("{p"),
            "contributor query row should be a brace-query exemplar: {contrib:?}"
        );
        assert!(
            contrib.contains("args:") && contrib.contains(" opt"),
            "contributor query Meaning should carry compact args (req/opt), not a duplicate optional list: {contrib:?}"
        );
    }

    /// Full `apis/github` TSV teaching prompt (symbolic). Output is **deterministic** for the
    /// tree’s `apis/github` catalog. When the catalog or renderer changes, run
    /// `INSTA_UPDATE=1 cargo test -p plasm-core github_api_full_prompt_symbolic_snapshot` and review the diff.
    #[test]
    fn github_api_full_prompt_symbolic_snapshot() {
        let dir = apis_dir("github");
        if !dir.is_dir() {
            eprintln!(
                "skip: apis/github not at {} (incomplete plasm-oss tree?)",
                dir.display()
            );
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt = render_prompt_with_config(
            &cgs,
            RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
        );
        with_insta_snapshots(|| {
            insta::assert_snapshot!("github_api_full_prompt_symbolic", prompt);
        });
    }

    #[test]
    fn prompt_render_mode_user_surface_helpers_cover_public_modes() {
        assert_eq!(PromptRenderMode::USER_FACING_VALUES, ["tsv"]);
        assert_eq!(
            PromptRenderMode::parse_user_facing("verbose"),
            Some(PromptRenderMode::Tsv)
        );
        assert_eq!(
            PromptRenderMode::parse_user_facing("compact"),
            Some(PromptRenderMode::Tsv)
        );
        assert_eq!(
            PromptRenderMode::parse_user_facing("tsv"),
            Some(PromptRenderMode::Tsv)
        );
        assert_eq!(PromptRenderMode::parse_user_facing("canonical"), None);
        assert_eq!(
            PromptRenderMode::parse_user_facing_or_default("unknown"),
            PromptRenderMode::Tsv
        );
        assert_eq!(PromptRenderMode::Canonical.user_facing_name(), None);
        assert_eq!(
            PromptRenderMode::Compact.user_facing_name(),
            Some("compact")
        );
        assert_eq!(PromptRenderMode::Tsv.markdown_fence_info_string(), "tsv");
        assert_eq!(
            PromptRenderMode::Compact.markdown_fence_info_string(),
            "tsv"
        );
    }

    /// Contract text for MCP / TSV frontmatter; update with `INSTA_UPDATE=1 cargo test -p plasm-core plasm_mcp_language_frontmatter_snapshot`.
    #[test]
    fn plasm_mcp_language_frontmatter_snapshot() {
        with_insta_snapshots(|| {
            insta::assert_snapshot!(
                "plasm_mcp_language_frontmatter",
                render_plasm_mcp_language_frontmatter()
            );
        });
    }

    /// Full `apis/linear` prompt. Deterministic for the checked-in catalog; use `INSTA_UPDATE=1`
    /// with `linear_api_full_prompt` when the catalog or renderer changes.
    #[test]
    fn linear_api_full_prompt_includes_rich_string_preamble_snapshot() {
        let dir = apis_dir("linear");
        if !dir.is_dir() {
            eprintln!(
                "skip: apis/linear not at {} (incomplete plasm-oss tree?)",
                dir.display()
            );
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        with_insta_snapshots(|| {
            insta::assert_snapshot!("linear_api_full_prompt", prompt);
        });
    }

    /// Pokeapi `Type`-only slice. Deterministic for the checked-in `apis/pokeapi` + slice config;
    /// update the snapshot with `INSTA_UPDATE=1` when inputs change.
    #[test]
    fn pokeapi_type_only_slice_prompt_snapshot() {
        let dir = apis_dir("pokeapi");
        if !dir.is_dir() {
            eprintln!(
                "skip: apis/pokeapi not at {} (incomplete plasm-oss tree?)",
                dir.display()
            );
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let out = render_prompt_with_config(&cgs, RenderConfig::for_eval_seeds(&["Type"]));
        with_insta_snapshots(|| {
            insta::assert_snapshot!("pokeapi_type_only_slice_prompt", out);
        });
    }

    #[test]
    fn domain_prompt_bundle_tags_relation_nav_materialization() {
        let dir = apis_dir("pokeapi");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let bundle = render_domain_prompt_bundle(&cgs, RenderConfig::for_eval_seeds(&["Type"]));
        let found = bundle
            .model
            .entities
            .iter()
            .flat_map(|e| &e.lines)
            .any(|l| {
                l.kind == DomainLineKind::RelationNav
                    && matches!(
                        l.relation_materialization,
                        Some(RelationMaterializationSummary::FromParentGet)
                    )
            });
        assert!(
            found,
            "expected a relation DOMAIN line with FromParentGet metadata"
        );
        let mut cfg = RenderConfig::for_eval_canonical(None);
        cfg.include_domain_execution_model = false;
        let bundle2 = render_domain_prompt_bundle(&cgs, cfg);
        assert!(bundle2.model.entities.is_empty());
    }

    #[test]
    fn petstore_domain_lists_capabilities() {
        let dir = fixtures_schemas_dir("petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let output = render_prompt_with_config(&cgs, RenderConfig::for_eval_canonical(None));
        assert!(
            output.contains("Pet") && output.contains("plasm_expr\tMeaning"),
            "TSV prompt should list Pet"
        );
        assert!(
            !output.contains("shape:"),
            "TSV prompt should not prefix every line with shape:"
        );
        assert!(
            output.contains("Pet{") && output.contains("status"),
            "domain should surface query brace form with status from CGS"
        );
    }

    #[test]
    fn petstore_domain_line_meta_includes_source_capability() {
        let dir = fixtures_schemas_dir("petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let bundle = render_domain_prompt_bundle(
            &cgs,
            RenderConfig {
                focus: FocusSpec::All,
                render_mode: PromptRenderMode::Canonical,
                include_domain_execution_model: true,
                symbol_map_cross_cache: None,
            },
        );
        let pet = bundle
            .model
            .entities
            .iter()
            .find(|e| e.entity == "Pet")
            .expect("Pet DOMAIN block");
        let bound = pet
            .lines
            .iter()
            .filter(|l| l.source_capability.is_some())
            .count();
        assert!(
            bound > 0,
            "expected at least one DOMAIN line bound to a CGS capability id"
        );
        assert!(pet
            .lines
            .iter()
            .all(|l| { l.kind != DomainLineKind::RelationNav || l.source_capability.is_none() }));
    }

    #[test]
    fn focus_subsetting_shows_full_and_dim() {
        let dir = fixtures_schemas_dir("petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let output =
            render_prompt_with_config(&cgs, RenderConfig::for_eval_canonical(Some("Order")));
        assert!(output.contains("Order"));
        assert!(output.contains("User") || output.contains("Pet"));
    }

    #[test]
    fn pokeapi_bundle_is_reasonable_size() {
        let dir = apis_dir("pokeapi");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let out = render_prompt_with_config(&cgs, RenderConfig::for_eval_canonical(None));
        assert!(out.len() < 50_000, "bundle should stay bounded");
        assert!(!out.contains("EXAMPLES:") && out.contains("plasm_expr\tMeaning"));
    }

    /// `Team(id).spaces` uses `query_scoped` materialization — it parses as [`Expr::Chain`]; DOMAIN shows
    /// anchored relation nav plus scoped `Space{…}` under Space.
    #[test]
    fn clickup_domain_includes_materialized_team_spaces_nav() {
        let dir = apis_dir("clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let sym = render_prompt_with_config(
            &cgs,
            RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
        );
        let raw = render_prompt_with_config(&cgs, RenderConfig::for_eval_canonical(None));
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let team_sym = map.entity_sym("Team");
        assert!(
            raw.contains("Team($).spaces"),
            "expected canonical Team→spaces relation line (chain materialization)"
        );
        assert!(
            sym.contains(&format!("{}($).", team_sym)) && sym.contains("spaces"),
            "expected symbol-tuned Team→spaces relation line"
        );
        assert!(
            raw.contains("Space{") && raw.contains("team_id"),
            "Space scoped query with team_id should remain in DOMAIN (canonical)"
        );
        assert!(
            sym.contains("Space{")
                || (sym.contains("{p") && sym.contains(&format!("={}(", team_sym)))
                || raw.contains("Space{"),
            "Space scoped query should remain in DOMAIN"
        );
    }

    /// `team_query` is query-shaped (`e1` in DOMAIN); description + gloss inline after `;;` (no QUERIES table).
    #[test]
    fn clickup_domain_gloss_and_symbol_map_queries() {
        let dir = apis_dir("clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let sym = render_prompt_with_config(
            &cgs,
            RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
        );
        assert!(
            !sym.contains("FIELDS\n"),
            "global FIELDS block removed — p# gloss is inline before first use"
        );
        // `p#` indices depend on sorted entity exposure — pick any gloss token that also appears in `[…,p#]`.
        let (gloss, p_tok, bracket_use) = sym
            .lines()
            .find_map(|line| {
                let (expr, meaning) = line.split_once('\t')?;
                let rest = expr.strip_prefix('p')?;
                if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                if !meaning.contains('·') {
                    return None;
                }
                let gloss_pos = sym.find(line)?;
                sym[gloss_pos..]
                    .find(&format!("{expr}]"))
                    .map(|off| (gloss_pos, expr.to_string(), gloss_pos + off))
            })
            .expect("p# gloss with matching bracket projection");
        assert!(
            gloss < bracket_use,
            "{} gloss should appear before bracket use",
            p_tok
        );
        assert!(
            !sym.contains("QUERIES\n"),
            "QUERIES table removed — capability text lives on DOMAIN lines"
        );
        assert!(
            !sym.contains("METHODS\n"),
            "METHODS table removed — invoke glosses live on DOMAIN lines"
        );
        let domain_start = sym
            .find(DOMAIN_VALID_EXPR_MARKER)
            .expect("valid expressions preamble");
        let domain_block = &sym[domain_start..];
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let team_sym = map.entity_sym("Team");
        assert!(
            domain_block.contains(super::DOMAIN_VALID_EXPR_MARKER),
            "TSV contract should open with valid-expression rules"
        );
        assert!(
            domain_block.lines().any(|line| {
                line.contains("List all accessible workspaces")
                    && line.contains(&format!("[{}]", team_sym))
            }),
            "TSV team_query should describe list workspaces returning [{}]",
            team_sym
        );
        assert!(
            !domain_block.contains(" -> "),
            "relation / field nav lines must use `;;  => e#` (or `[e#]`), not `expr -> e#` before ;;"
        );
        let task_sym = map.entity_sym("Task");
        let p_team_id = map.ident_sym_cap_param("Task", "task_query", "team_id");
        assert!(
            domain_block.contains(&format!(
                "{}{{{}={}($)",
                task_sym, p_team_id, team_sym
            )),
            "workspace-scoped task query should teach scope with unary entity-ref fill-in (p#=e#($)), not bare team id literals"
        );
        assert!(
            !domain_block.contains("2000-01-01") && !domain_block.contains("p10>=\""),
            "query DOMAIN brace form must not teach concrete ISO datetimes or `>=` date literals"
        );
        assert!(
            domain_block.contains("List all accessible workspaces"),
            "TSV Meaning should carry capability description without duplicating m#"
        );
    }

    /// User has only pathless singleton `user_get_me` — DOMAIN must show `e#.m#()` (get-me) and not mislead with `e#(42)`.
    #[test]
    fn clickup_user_singleton_get_me_line_in_domain() {
        let dir = apis_dir("clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let sym = render_prompt_with_config(
            &cgs,
            RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
        );
        let domain_start = sym
            .find(DOMAIN_VALID_EXPR_MARKER)
            .expect("valid expressions preamble");
        let domain_block = &sym[domain_start..];
        assert!(
            domain_block.contains("currently authenticated"),
            "User DOMAIN should describe singleton get-me"
        );
        assert!(
            sym.lines().any(|l| {
                l.contains("currently authenticated")
                    && l.contains(".m")
                    && l.contains("()")
                    && !l.contains("(42)")
            }),
            "User TSV should teach singleton get-me as e#.m#(), not id-based e#(42)"
        );
    }

    /// Book —(shelf)—> Shelf; two query caps; one navigation edge from Book.
    fn prompt_stats_fixture_cgs() -> CGS {
        let mut cgs = CGS::new();
        let id_field = FieldSchema {
            name: "id".into(),
            description: String::new(),
            field_type: FieldType::String,
            value_format: None,
            allowed_values: None,
            required: true,
            array_items: None,
            string_semantics: None,
            agent_presentation: None,
            mime_type_hint: None,
            attachment_media: None,
            wire_path: None,
            derive: None,
        };
        cgs.add_resource(ResourceSchema {
            name: "Book".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![id_field.clone()],
            relations: vec![RelationSchema {
                name: "shelf".into(),
                description: String::new(),
                target_resource: "Shelf".into(),
                cardinality: Cardinality::Many,
                materialize: None,
            }],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
        })
        .unwrap();
        cgs.add_resource(ResourceSchema {
            name: "Shelf".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![id_field],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
        })
        .unwrap();
        let tmpl =
            serde_json::json!({"method": "GET", "path": [{"type": "literal", "value": "x"}]});
        for (name, domain) in [("book_query", "Book"), ("shelf_query", "Shelf")] {
            cgs.add_capability(CapabilitySchema {
                name: name.into(),
                description: String::new(),
                kind: CapabilityKind::Query,
                domain: domain.into(),
                mapping: CapabilityMapping {
                    template: tmpl.clone().into(),
                },
                input_schema: None,
                output_schema: None,
                provides: vec![],
                scope_aggregate_key_policy: Default::default(),
                invoke_preflight: None,
            })
            .unwrap();
        }
        cgs.validate().unwrap();
        cgs
    }

    #[test]
    fn prompt_surface_stats_counts_caps_nav_and_domain_tools() {
        let cgs = prompt_stats_fixture_cgs();
        // Symbolic render modes — same entity slice as execute / [`domain_exposure_session_from_focus`]
        // (seed-only for Single/Seeds; no 2-hop union).
        let (c_all, n_all) = json_tool_surface_counts(&cgs, FocusSpec::All, true);
        assert_eq!((c_all, n_all), (2, 1));

        let (c_book, n_book) = json_tool_surface_counts(&cgs, FocusSpec::Single("Book"), true);
        assert_eq!((c_book, n_book), (1, 1));

        let (c_shelf, n_shelf) = json_tool_surface_counts(&cgs, FocusSpec::Single("Shelf"), true);
        assert_eq!((c_shelf, n_shelf), (1, 0));

        // Legacy 2-hop neighbourhood when render mode is canonical.
        let (c_book_2hop, n_book_2hop) =
            json_tool_surface_counts(&cgs, FocusSpec::Single("Book"), false);
        assert_eq!((c_book_2hop, n_book_2hop), (2, 1));

        let cfg = RenderConfig::for_eval(None);
        let (names, exposure_opt) =
            resolve_prompt_surface_entities(&cgs, cfg.focus, cfg.uses_symbols());
        let domain_tools = super::domain_expression_tool_count_resolved(
            &cgs,
            &names,
            exposure_opt.as_ref(),
            cfg.uses_symbols(),
        );
        // Book: one query line; Shelf: one. Many `shelf` relation is Unmaterialized → no nav line in DOMAIN.
        assert_eq!(domain_tools, 2);

        let prompt = "αβγδε"; // 5 chars → legacy est 1; o200k is model-based
        let st = prompt_surface_stats(&cgs, cfg, prompt);
        assert_eq!(st.prompt_chars, 5);
        assert_eq!(st.token_estimate, 1);
        assert_eq!(
            st.prompt_tokens_o200k,
            crate::o200k_token_count::o200k_token_count(prompt)
        );
        assert_eq!(st.capability_tools, 2);
        assert_eq!(st.navigation_tools, 1);
        assert_eq!(st.json_tool_estimate, domain_tools);
        let sum = st.summary_line_body();
        assert!(sum.contains("tok (o200k)"));
        assert!(sum.contains("chars/4)"));
    }

    fn string_id_field(description: &str) -> FieldSchema {
        FieldSchema {
            name: "id".into(),
            description: description.to_string(),
            field_type: FieldType::String,
            value_format: None,
            allowed_values: None,
            required: true,
            array_items: None,
            string_semantics: None,
            agent_presentation: None,
            mime_type_hint: None,
            attachment_media: None,
            wire_path: None,
            derive: None,
        }
    }

    /// Two entities, same wire field `id` (maps to one `p#`), optional distinct descriptions — for
    /// [`emit_field_def_lines_before_example`] identity tests.
    fn p_slot_redefinition_fixture_cgs(id_desc_a: &str, id_desc_b: &str) -> CGS {
        let mut cgs = CGS::new();
        for (name, desc) in [("Anvil", id_desc_a), ("Beryl", id_desc_b)] {
            cgs.add_resource(ResourceSchema {
                name: name.into(),
                description: String::new(),
                id_field: "id".into(),
                id_format: None,
                id_from: None,
                fields: vec![string_id_field(desc)],
                relations: vec![],
                expression_aliases: vec![],
                implicit_request_identity: false,
                key_vars: vec![],
                abstract_entity: false,
                domain_projection_examples: true,
                primary_read: None,
            })
            .unwrap();
            let cap_name: String = format!("{}_get", name.to_lowercase());
            cgs.add_capability(CapabilitySchema {
                name: cap_name.into(),
                description: String::new(),
                kind: CapabilityKind::Get,
                domain: name.into(),
                mapping: CapabilityMapping {
                    template: serde_json::json!({
                        "method": "GET",
                        "path": [
                            {"type": "literal", "value": name.to_lowercase()},
                            {"type": "var", "name": "id"},
                        ],
                    })
                    .into(),
                },
                input_schema: None,
                output_schema: None,
                provides: vec![],
                scope_aggregate_key_policy: Default::default(),
                invoke_preflight: None,
            })
            .unwrap();
        }
        cgs.validate()
            .expect("p_slot_redefinition fixture must validate");
        cgs
    }

    /// Same `p#` for wire `id`, same structural type — description change forces a second gloss line.
    #[test]
    fn compact_domain_re_emits_p_slot_gloss_when_description_identity_changes() {
        let cgs = p_slot_redefinition_fixture_cgs("P_SLOT_REIDENT_ALPHA", "P_SLOT_REIDENT_BETA");
        let prompt = render_prompt_with_config(
            &cgs,
            RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
        );
        let domain = prompt
            .find(DOMAIN_VALID_EXPR_MARKER)
            .map(|i| &prompt[i..])
            .unwrap_or(&prompt);
        let gloss_hits: Vec<_> = domain
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                t.starts_with("p") && t.contains('\t') && t.contains("P_SLOT_REIDENT_")
            })
            .collect();
        assert!(
            gloss_hits
                .iter()
                .any(|l| l.contains("P_SLOT_REIDENT_ALPHA")),
            "expected first-entity id gloss with ALPHA marker; gloss lines: {gloss_hits:?}"
        );
        assert!(
            gloss_hits.iter().any(|l| l.contains("P_SLOT_REIDENT_BETA")),
            "expected second-entity id re-gloss with BETA marker; gloss lines: {gloss_hits:?}"
        );
    }

    /// Same-shaped `id` slots on different entities are distinct opaque `p#` rows (qualified symbols).
    #[test]
    fn compact_domain_emits_distinct_p_slot_glosses_for_same_shaped_id_fields() {
        let same = "P_SLOT_REIDENT_SAME";
        let cgs = p_slot_redefinition_fixture_cgs(same, same);
        let prompt = render_prompt_with_config(
            &cgs,
            RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
        );
        let domain = prompt
            .find(DOMAIN_VALID_EXPR_MARKER)
            .map(|i| &prompt[i..])
            .unwrap_or(&prompt);
        let count = domain
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                t.starts_with("p") && t.contains('\t') && t.contains("P_SLOT_REIDENT_SAME")
            })
            .count();
        assert_eq!(
            count, 2,
            "expected two p# gloss rows for the same wire name on two entities; domain excerpt:\n{domain}"
        );
    }

    fn assert_prompt_examples_parse(dir: &std::path::Path) {
        assert_prompt_examples_valid(dir, RenderConfig::for_eval(None));
    }

    /// DOMAIN lines must **parse**, **resolve** query capabilities where applicable, and **type-check**
    /// — the same baseline as execution (not merely syntactic validity).
    fn assert_prompt_examples_valid(dir: &std::path::Path, config: RenderConfig<'_>) {
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let map =
            crate::symbol_tuning::symbol_map_for_prompt(&cgs, config.focus, config.uses_symbols());
        let prompt = if config.render_mode.is_tsv() {
            render_prompt_tsv_with_config(&cgs, config)
        } else {
            render_prompt_with_config(&cgs, config)
        };
        let exprs = example_expressions_from_prompt(&prompt);
        assert!(
            !exprs.is_empty(),
            "expected DOMAIN section with expressions for {}",
            dir.display()
        );
        for expr in &exprs {
            let work = map
                .as_ref()
                .map(|m| crate::symbol_tuning::expand_path_symbols(expr, m))
                .unwrap_or_else(|| expr.clone());
            let mut r = crate::expr_parser::parse(&work, &cgs).unwrap_or_else(|e| {
                panic!(
                    "DOMAIN expr should parse for {}: {expr:?} (expanded {work:?})\n{e}",
                    dir.display()
                );
            });
            if let Err(e) = crate::normalize_expr_query_capabilities(&mut r.expr, &cgs) {
                panic!(
                    "DOMAIN expr should resolve query capability for {}: {expr:?} (expanded {work:?})\n{e}",
                    dir.display()
                );
            }
            if let Err(e) = crate::type_check_expr(&r.expr, &cgs) {
                panic!(
                    "DOMAIN expr should type-check for {}: {expr:?} (expanded {work:?})\n{e}",
                    dir.display()
                );
            }
        }
    }

    #[test]
    fn petstore_rendered_examples_parse() {
        assert_prompt_examples_parse(&fixtures_schemas_dir("petstore"));
    }

    #[test]
    fn clickup_rendered_examples_parse() {
        assert_prompt_examples_parse(&apis_dir("clickup"));
    }

    #[test]
    fn github_rendered_examples_parse() {
        assert_prompt_examples_parse(&apis_dir("github"));
    }

    /// Writes `apis/<name>/eval/prompt_symbol_tuning.txt` for inspection (eval/REPL bundle).
    /// Does not run in normal `cargo test`; use:  
    /// `cargo test -p plasm-core write_clickup_prompt_fixture -- --ignored --exact --nocapture`
    #[test]
    #[ignore = "manual: dumps prompt bundle to apis/.../eval/prompt_symbol_tuning.txt"]
    fn write_clickup_prompt_fixture() {
        let dir = apis_dir("clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let s = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let out = dir.join("eval/prompt_symbol_tuning.txt");
        std::fs::write(&out, &s).unwrap();
        eprintln!("wrote {} bytes to {}", s.len(), out.display());
    }

    #[test]
    fn query_domain_lines_match_expr_shape() {
        assert_eq!(super::query_construct_display("e4", "e4"), "e4");
        assert_eq!(
            super::query_construct_display("e4", "*p41=e2(id) *p25=e3(id)"),
            "e4{p41=e2(id), p25=e3(id)}"
        );
        assert_eq!(
            super::query_construct_display("e4", "*p41=e2(id)"),
            "e4{p41=e2(id)}"
        );
    }

    /// Locks compact DOMAIN + symbol preamble for `fixtures/schemas/overshow_tools`.
    /// Update with `INSTA_UPDATE=always cargo test -p plasm-core overshow_tools_compact_prompt_snapshot -- --exact`.
    #[test]
    fn overshow_tools_compact_prompt_snapshot() {
        let dir = fixtures_schemas_dir("overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt = render_prompt_with_config(
            &cgs,
            RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
        );
        with_insta_snapshots(|| {
            insta::assert_snapshot!("overshow_tools_compact_prompt", prompt);
        });
    }

    /// Locks TSV DOMAIN render for the same fixture (review diffs with compact snapshot above).
    #[test]
    fn overshow_tools_prompt_tsv_snapshot() {
        let dir = fixtures_schemas_dir("overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        with_insta_snapshots(|| {
            insta::assert_snapshot!("overshow_tools_prompt_tsv", tsv);
        });
    }
}
