//! CGS prompt renderer — TSV **Plasm** many-shot examples: each teaching row is `plasm_expr`, **one tab (U+0009)**,
//! then `Meaning` (middle-dot ` · ` joins gloss **inside** Meaning only). Synthesis builds structured
//! [`EntityTeachingBlock`] rows and emits TSV directly ([`render_prompt_tsv_from_bundle`]); synthesis stays structured
//! (model → [`TeachingExprLine`] / [`TeachingFieldGloss`]) without re-parsing a compact teaching transcript in production.
//! Symbolic prompts use `p#` / `v#` glosses emitted before first use (`v#` = shared `values:` domain;
//! each distinct taught `p#` meaning teaches **`v# · wire`** (and optional point-of-use prose) when the slot uses a `value_ref`, with typing on the `v#` row only).
//!
//! This is the prompt string for `plasm-eval` / BAML, REPL startup / `:schema`, HTTP execute session `prompt`, and MCP teaching table after `plasm_context`.
//! Build via [`render_prompt_with_config`] or [`render_prompt_tsv_with_config`]. Both now emit the
//! TSV teaching surface. [`RenderConfig::for_eval`] defaults to [`PromptRenderMode::Tsv`] (`e#` /
//! `m#` / `p#`); legacy compact/canonical modes affect symbol naming only, not the output format.
//! The Plasm language grammar (composition / postfix / heredoc / row-to-text) lives **statically** in
//! the MCP `plasm` tool description ([`PLASM_TOOL_DESCRIPTION`]); the prompt rendered here is the
//! **table-only** teaching TSV — no grammar contract is interleaved per wave. Catalogue-specific
//! teaching rows act as many-shot semantic instantiations: they teach which concrete `e#` / `m#` /
//! `p#` symbols, fields, methods, scoped filters, and relations are valid for this catalogue wave.
//! The `~` search form and tagged `<<TAG` heredocs are taught unconditionally by the static grammar;
//! per-entity teaching rows still witness the concrete search / string-valued slots for each entity.
//!
//! **teaching table** is **per-entity blocks** of **valid Plasm expressions only** (CGS-validated before emit).
//! In the teaching TSV, the entity `description` is attached to the **first projection witness** for that
//! entity when one exists, otherwise to the **identity** get row. Rows are phased per block: **`v#` gloss**
//! (except the deferred synthetic union summary), **`p#` gloss**, **`r#` gloss** (relation alias → wire name),
//! **union constructor exemplars**
//! (`vN{p#=…}`), **union summary** (`union · v101 | …` on an allocator-chosen `v#`), then remaining
//! teaching expressions (**projection witnesses first** among them). The canonical `[p#,…]` field set is
//! taught **once** on the witness row; query/search row-producer lines omit the same bracket and Meaning
//! `rows:` contract (parser treats projection as optional — full rows when omitted). Divergent capability
//! `provides` still attach an explicit bracket/`rows:`. Value domain once per `v#`, then each distinct
//! `v# · wire` teaching once per shared `p#`; point-of-use prose is omitted when it duplicates the shared
//! `values:` row description.
//! Model output must be those expression shapes—not prose.
//! Use [`RenderConfig::focus`] to subset entities.
//!
//! **Relations** lines teach `Get(id).relation` when that path **parses and type-checks**. With an
//! [`ExposureSurface`](crate::symbol_tuning::ExposureSurface) filter (incremental teaching waves), **outgoing**
//! navigation teaches only targets in the surface entity set, and **incoming** projection-witness bases
//! (`ParentRecv…[p#,…]`) require the parent entity on the surface plus the same slot checks as outgoing nav;
//! field gloss rows and `ref:*` typing are unchanged.
//! Meaning uses
//! `relation e#_src → [e#_tgt]` (many) or `relation e#_src → e#_tgt` (one) in **Meaning** only; executable nav is `<receiver>.r#` or wire in `plasm_expr`.
//! For terminal relation chains, the example line already carries a **result gloss** (`relation …`);
//! relation hops use the **`r#` pool** in exemplars (`.r#` in `plasm_expr`) with standalone **`r#` gloss rows**
//! mapping alias → wire name (parallel to `p#` / `v# · wire`).
//! Splitting relations out of the `p#` pool renumbers `p#` in snapshots but does **not** add duplicate
//! teaching rows (GitHub full prompt stays ~flat; diff churn is mostly `p#` renumbering).
//! For cardinality-many
//! edges with `materialize` (`from_parent_get`, `query_scoped`, …) the IR is [`Expr::Chain`](crate::Expr);
//! many-relations without materialization **fail parse** and are omitted from teaching table.
//!
//! **Validation:** every **single-expression** teaching example (after stripping human-only `  ;;  …` suffixes,
//! legacy `  =>  ` before `;;`, and legacy relation ` -> …` before `;;`) is checked with **parse →
//! [`normalize_expr_query_capabilities`](crate::normalize_expr_query_capabilities) → [`type_check_expr`](crate::type_check_expr)** before emission.
//! Zero-arity pipeline methods emit **one** `…()` expression per line (each line is fully validated).
//!
//! **Load-time invariant:** [`CGS::validate`](crate::schema::CGS::validate) runs [`crate::cgs_expression_validate`],
//! which requires every non-abstract entity to produce at least one such line via the same synthesis as
//! [`collect_entity_teaching_block`] (opaque symbol map in **compact**/**tsv** modes, matching eval / REPL).

use crate::{
    cross_entity::{choose_strategy, extract_cross_entity_predicates},
    schema::{
        capability_is_zero_arity_invoke, capability_method_label_kebab, Cardinality, EntityDef,
        InputFieldSchema, RelationMaterialization, RelationSchema,
    },
    symbol_tuning::{
        symbol_map_cache_key_federated, symbol_map_cache_key_single_catalog, ExposureCapabilityKey,
        ExposureEntityKey, ExposureSlotKey, ExposureSurface, FocusSpec, IdentMetaKey,
        IdentMetadata, SymbolMap, SymbolMapCrossRequestCache, TeachingExposureSession,
    },
    CapabilityKind, CapabilityName, EntityFieldName, EntityName, Expr, FieldType, InputType,
    ParameterRole, RelationName, ValueWireFormat, CGS,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
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

/// teaching TSV: first line of the teaching table (`plasm_expr` and `Meaning` columns) including the
/// trailing newline, matching [`render_prompt_tsv_from_bundle`].
pub const TSV_TEACHING_TABLE_HEADER: &str = "plasm_expr\tMeaning\n";

/// TSV Meaning legend token for optional invoke/query slots (not program syntax).
pub(crate) const TEACHING_OPTIONAL_LEGEND_MARK: &str = "optional";

mod input_legend;
pub use input_legend::{CapabilityInputLegend, OptionalLegend, TeachingExprLine};

mod capability_delta;
mod contract;
mod gloss_dedup;
mod line_validate;
mod mcp_prompt_fragments;
mod mcp_tool_descriptions;
mod row_producer;
mod stats;
mod teaching_gloss_emit;
mod tsv_emit;

#[cfg(test)]
mod query_teaching_tests;

use line_validate::{
    domain_line_validate_cached, domain_line_work_valid_cached, prompt_line_valid_cache_seed_cgs,
    prompt_line_valid_cache_seed_exposure, DomainLineValidCacheKey, DomainLineValidEntry,
};
use row_producer::RowProducerProjection;

pub use capability_delta::{
    render_teaching_new_capabilities_delta_tsv,
    render_teaching_new_capabilities_delta_tsv_federated,
};
pub use contract::{
    markdown_fence_body_inner, split_tsv_teaching_contract_and_table,
    teaching_tsv_agent_body_from_wrapped_prompt, teaching_tsv_from_wrapped_prompt,
    teaching_tsv_table_from_wrapped_prompt, TeachingFenceSlice, ROW_COMPUTE_EXEMPLAR_THRESHOLD,
};
pub use mcp_prompt_fragments::{
    format_ranked_replay_diagnostics, render_active_mutator_surface_recap,
    render_compact_exposure_symbol_map, DISCOVER_DECISION_CLARIFY, DISCOVER_DECISION_MATCH,
    DISCOVER_DECISION_NO_MATCH, DISCOVER_TSV_LANGUAGE_PREAMBLE,
};
pub use mcp_tool_descriptions::{
    DISCOVER_TOOL_DESCRIPTION, MCP_INITIALIZE_WORKFLOW, MCP_TOOL_SEQUENCING_MARKER,
    MCP_TOOL_SYNTAX_CONTRACT_MARKER, PLASM_CONTEXT_TOOL_DESCRIPTION,
    PLASM_PROGRAM_PARAM_DESCRIPTION, PLASM_READ_RUN_ARTIFACT_TOOL_DESCRIPTION,
    PLASM_RUN_TOOL_ARTIFACT_RESOURCES, PLASM_RUN_TOOL_ARTIFACT_TOOL, PLASM_RUN_TOOL_DESCRIPTION,
    PLASM_RUN_TOOL_DESCRIPTION_BASE, PLASM_TOOL_DESCRIPTION, TEACHING_VALID_EXPR_MARKER,
};
#[cfg(test)]
pub(crate) use stats::domain_expression_tool_count_resolved;
pub use stats::{
    grammar_frontmatter_section_bytes, grammar_frontmatter_stats_from_contract,
    grammar_frontmatter_stats_from_prompt, json_tool_surface_counts, prompt_surface_stats,
    strip_tsv_comment_contract_prefix, GrammarFrontmatterStats,
};

#[cfg(test)]
pub(crate) use contract::validate_teaching_tsv_teaching_table;
pub(crate) use gloss_dedup::*;
pub(crate) use teaching_gloss_emit::*;
pub(crate) use tsv_emit::{
    is_union_ctor_teaching_surface_line, parse_trailing_projection_bracket,
    relation_sym_shown_in_query_teaching_rows, render_prompt_tsv_from_bundle,
    teaching_relation_field_gloss, write_teaching_tsv_row, DomainTsvRow,
};
#[cfg(test)]
pub(crate) use tsv_emit::{projection_bracket_from_teaching_rows, teaching_row_meaning_text};

#[derive(Clone, Copy, Debug)]
pub struct RenderConfig<'a> {
    /// Subset of entities for teaching table / symbol map (see [`FocusSpec`]).
    pub focus: FocusSpec<'a>,
    /// Prompt render surface: canonical, verbose symbolic, compact symbolic, or TSV symbolic.
    pub render_mode: PromptRenderMode,
    /// When true, [`render_teaching_prompt_bundle`] fills [`TeachingPromptModel`] (cross-entity strategy, relation materialization).
    /// Reserved for product policy to omit execution metadata later.
    pub include_domain_execution_model: bool,
    /// When set (same LRU as execute-session expansion), symbolic teaching table renders reuse [`SymbolMap`] snapshots across invocations.
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

    /// Full-schema teaching synthesis for [`crate::cgs_expression_validate::validate_cgs_expression_surface`].
    ///
    /// Uses [`FocusSpec::All`], [`PromptRenderMode::Tsv`], and [`Self::include_domain_execution_model`] `true`
    /// so [`TeachingPromptModel`] lines carry [`TeachingLineMeta::source_capability`] metadata the validator
    /// relies on for per-capability coverage (keep renderer and validator in agreement).
    pub fn for_expression_surface_validation() -> Self {
        Self {
            focus: FocusSpec::All,
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

    /// Canonical entity/method/field names in teaching table (for tests / debugging).
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

/// Product-facing **where** teaching symbols are seeded from: catalog [`FocusSpec`] vs execute [`TeachingExposureSession`].
#[derive(Clone, Copy, Debug)]
pub enum TeachingPromptSource<'a> {
    Catalog {
        focus: FocusSpec<'a>,
    },
    ExecuteWave {
        exposure: &'a TeachingExposureSession,
    },
}

/// Product-facing knobs for the teaching bundle / TSV (prefer over assembling [`RenderConfig`] at new call sites).
#[derive(Clone, Copy, Debug)]
pub struct TeachingPromptSettings<'a> {
    pub include_domain_execution_model: bool,
    /// When false, teaching rows use canonical names (tool explorer / narrow tests); when true, `e#`/`p#`/`m#` symbolic TSV.
    pub symbolic: bool,
    pub symbol_map_cross_cache: Option<&'a SymbolMapCrossRequestCache>,
}

impl<'a> Default for TeachingPromptSettings<'a> {
    fn default() -> Self {
        Self {
            include_domain_execution_model: true,
            symbolic: true,
            symbol_map_cross_cache: None,
        }
    }
}

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

/// Per-entity teaching lines with execution hints parallel to the rendered prompt strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TeachingPromptModel {
    pub entities: Vec<EntityTeachingPrompt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityTeachingPrompt {
    /// Canonical CGS entity name (`Issue`, `Zone`, …) — not the session-local `e#` alias.
    pub entity: String,
    pub lines: Vec<TeachingLineMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachingLineMeta {
    /// Expression only (no `;;` hints), after the same stripping/expansion as validation.
    pub expression: String,
    pub kind: DomainLineKind,
    /// When this line teaches a concrete CGS capability (get / query / search / method), its id.
    /// Omitted for relation-navigation lines and other synthesized lines without a single owner.
    ///
    /// **Schema validation contract:** [`crate::cgs_expression_validate::validate_cgs_expression_surface`]
    /// treats populated values as evidence that the corresponding capability is teachable on the expression
    /// surface; omitting this on a capability-backed teaching line can fail load-time coverage checks.
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
    /// Legacy bucket; validated projection witness rows are typed as get/query/method from parse.
    Projection,
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
            DomainLineKind::Projection => "projection",
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
    PreferFromParentGet,
    QueryScoped,
    QueryScopedBindings,
    GetScopedBindings,
}

impl From<&RelationMaterialization> for RelationMaterializationSummary {
    fn from(m: &RelationMaterialization) -> Self {
        match m {
            RelationMaterialization::Unavailable => Self::Unavailable,
            RelationMaterialization::FromParentGet { .. } => Self::FromParentGet,
            RelationMaterialization::PreferFromParentGet { .. } => Self::PreferFromParentGet,
            RelationMaterialization::QueryScoped { .. } => Self::QueryScoped,
            RelationMaterialization::QueryScopedBindings { .. } => Self::QueryScopedBindings,
            RelationMaterialization::GetScopedBindings { .. } => Self::GetScopedBindings,
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

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachingHeading {
    /// Human prose merged into TSV identity Meaning for this entity block (typically the CGS entity `description`).
    /// Projection bracket for the heading is inferred from teaching rows, not from this string.
    pub description: String,
}

impl TeachingHeading {
    fn from_entity_banner_description(desc: Option<&str>) -> Self {
        Self {
            description: desc
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("")
                .to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachingFieldGloss {
    pub symbol: String,
    pub field_type: String,
    pub allowed_values: String,
    pub description: String,
    /// Synthetic `union · v101 | …` summary row: defer in TSV until after variant ctor exemplars.
    #[serde(default)]
    pub is_inline_union_summary: bool,
}

/// teaching table teaching slices plus structured execution metadata for tooling / HTTP/MCP TSV emission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachingPromptBundle {
    pub teaching_blocks: Vec<EntityTeachingBlock>,
    pub model: TeachingPromptModel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityTeachingBlock {
    pub heading: TeachingHeading,
    pub field_gloss_rows: Vec<TeachingFieldGloss>,
    pub teaching_rows: Vec<EntityTeachingExprRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityTeachingExprRow {
    /// Synthesized teaching exemplar (not [`crate::expr_parser`] output).
    #[serde(rename = "parsed")]
    pub teaching_expr: TeachingExprLine,
    pub meta: TeachingLineMeta,
    #[serde(skip, default)]
    dedupe_key: TeachingRowDedupeKey,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
struct TeachingRowDedupeKey {
    expr: String,
    gloss: Option<String>,
    cap: Option<String>,
}

impl TeachingRowDedupeKey {
    fn new(expr: &str, gloss: Option<&String>, cap: Option<&String>) -> Self {
        Self {
            expr: expr.trim().to_string(),
            gloss: gloss
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            cap: cap.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        }
    }
}

/// Capability sig / human prose tail after result gloss — shared when assembling [`TeachingExprLine`] tails.
fn apply_compact_legend_remainder(row: &mut TeachingExprLine, remainder: &str) {
    let (sig_part, desc_tail) = split_sig_and_human_description(remainder);
    let (sig_wo, compact) = split_compact_args_from_sig_fragment(sig_part);
    row.legend.compact_args = compact;
    let mut orphan = String::new();
    fill_scope_optional_from_sig(
        &sig_wo,
        &mut row.legend.scope,
        &mut row.legend.optional,
        &mut orphan,
    );
    if !desc_tail.is_empty() {
        row.legend.description = desc_tail.to_string();
        if !orphan.is_empty() {
            row.legend.description = format!("{orphan} {}", row.legend.description)
                .trim()
                .to_string();
        }
    } else if !orphan.is_empty() {
        row.legend.description = orphan;
    }
}

/// Build [`TeachingExprLine`] from structured gloss layers (model → row; no compact `;;` wire).
fn teaching_expr_line_from_layers(
    expr: &str,
    result_gloss: Option<&str>,
    cap_legend: Option<&str>,
) -> TeachingExprLine {
    let expr = expr.trim().to_string();
    let gloss = result_gloss.map(str::trim).filter(|s| !s.is_empty());
    let cap = cap_legend.map(str::trim).filter(|s| !s.is_empty());
    let legend_present = gloss.is_some() || cap.is_some();
    if !legend_present {
        return TeachingExprLine::empty_legend(expr);
    }
    let is_projection_teaching = gloss.is_some_and(|g| g.contains(PROJECTION_WITNESS_LEGEND_MARK))
        && parse_trailing_projection_bracket(expr.trim()).is_some();
    let mut row = TeachingExprLine {
        expression: expr,
        result_type: gloss.map(|s| s.to_string()).unwrap_or_default(),
        legend: CapabilityInputLegend::default(),
        is_projection_teaching,
    };
    apply_compact_legend_remainder(&mut row, cap.unwrap_or(""));
    row
}

fn values_row_description_trimmed_for_ident(meta: &IdentMetadata, cgs: &CGS) -> String {
    match meta {
        IdentMetadata::RegistryBacked {
            value_registry_key, ..
        } => cgs
            .values
            .get(value_registry_key.as_str())
            .map(|nv| {
                crate::symbol_tuning::trim_description_for_agent_gloss(nv.description.as_str())
                    .to_string()
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Compact `p#` Meaning when the slot shares a `values:` row.
///
/// Registry-backed slots use **`v# · wire`**: entity fields and top-level capability params use the
/// wire name; nested capability inputs use the **leaf** key only (omit union path prefixes).
/// Point-of-use prose appends as **` · …`** when it adds information beyond the shared `values:` row.
fn compact_p_slot_registry_description(
    sym_m: &SymbolMap,
    p_sym: &str,
    meta: &IdentMetadata,
    cgs: &CGS,
) -> Option<String> {
    let vsym = sym_m.value_sym_for_p_sym(p_sym)?;
    let nv_desc = values_row_description_trimmed_for_ident(meta, cgs);
    let slot_norm = crate::symbol_tuning::trim_description_for_agent_gloss(meta.description());
    let wire = crate::symbol_tuning::registry_backed_compact_wire_label(meta);
    let mut description = format!("{vsym} · {wire}");
    if !slot_norm.is_empty() && slot_norm != nv_desc.as_str() {
        let t = crate::symbol_tuning::gloss_description_truncated(meta.description());
        description = format!("{vsym} · {wire} · {t}");
    }
    Some(description)
}

#[allow(clippy::too_many_arguments)]
fn push_teaching_field_gloss_row(
    out: &mut Vec<TeachingFieldGloss>,
    symbol: String,
    legend_rhs: &str,
    canonical_entity: &str,
    catalog_entry_id: &str,
    symbol_map: Option<&SymbolMap>,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    cgs: Option<&CGS>,
    is_inline_union_summary: bool,
) {
    let mut cs = symbol.chars();
    let first = match cs.next() {
        Some(c @ ('p' | 'r' | 'v')) => c,
        _ => return,
    };
    let rest: String = cs.collect();
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return;
    }
    let field_name = if first == 'r' {
        symbol_map
            .and_then(|m| m.resolve_relation_ident(symbol.as_str()))
            .map(str::to_string)
            .unwrap_or_else(|| symbol.to_string())
    } else {
        symbol_map
            .and_then(|m| m.wire_for_opaque_p_sym(symbol.as_str()))
            .unwrap_or_else(|| symbol.to_string())
    };
    // Leaf expand keys (e.g. `blocks`) collide with relation wire names — prefer full capability path.
    // `IdentMetaKey` is only `(catalog, entity, path)`; distinct capabilities can share the same path
    // (e.g. two different `operations` arrays). When CGS is present, resolve via the full `(cap, path)` quad.
    let meta = match (symbol_map, cgs) {
        (Some(sym_m), Some(cgs_ref)) => sym_m
            .capability_param_quad_for_p_sym_on_entity(
                symbol.as_str(),
                catalog_entry_id,
                canonical_entity,
            )
            .and_then(|(eid, dom, cap, path)| {
                if !eid.is_empty() && eid.as_str() != catalog_entry_id {
                    return None;
                }
                crate::symbol_tuning::ident_metadata_for_capability_input_path(
                    cgs_ref,
                    dom.as_str(),
                    cap.as_str(),
                    path.as_str(),
                )
            }),
        _ => None,
    }
    .or_else(|| match (symbol_map, ident_meta) {
        (Some(sym_m), Some(im)) => sym_m
            .capability_param_quad_for_p_sym_on_entity(
                symbol.as_str(),
                catalog_entry_id,
                canonical_entity,
            )
            .and_then(|(eid, dom, _cap, path)| {
                if !eid.is_empty() && eid.as_str() != catalog_entry_id {
                    return None;
                }
                im.get(&(catalog_entry_id.to_string(), dom.clone(), path.clone()))
                    .cloned()
            })
            .or_else(|| {
                im.get(&(
                    catalog_entry_id.to_string(),
                    EntityName::from(canonical_entity.to_string()),
                    field_name.clone(),
                ))
                .cloned()
            }),
        (_, Some(im)) => im
            .get(&(
                catalog_entry_id.to_string(),
                EntityName::from(canonical_entity.to_string()),
                field_name.clone(),
            ))
            .cloned(),
        _ => None,
    });
    let legend = legend_rhs.trim();
    if first == 'v' {
        out.push(TeachingFieldGloss {
            symbol,
            field_type: String::new(),
            allowed_values: String::new(),
            description: legend.to_string(),
            is_inline_union_summary,
        });
        return;
    }
    if first == 'r' {
        let wire = if field_name == symbol {
            legend.to_string()
        } else {
            field_name.clone()
        };
        let description = meta
            .as_ref()
            .map(|m| m.description().trim())
            .filter(|d| !d.is_empty())
            .map(crate::symbol_tuning::gloss_description_truncated)
            .unwrap_or_default();
        out.push(TeachingFieldGloss {
            symbol,
            field_type: wire,
            allowed_values: String::new(),
            description,
            is_inline_union_summary,
        });
        return;
    }
    if let Some(sym_m) = symbol_map {
        if let Some(vsym) = sym_m.value_sym_for_p_sym(symbol.as_str()) {
            let wire = meta
                .as_ref()
                .map(crate::symbol_tuning::registry_backed_compact_wire_label)
                .unwrap_or_else(|| field_name.clone());
            let description = if let (Some(m), Some(cgs_ref)) = (&meta, cgs) {
                compact_p_slot_registry_description(sym_m, symbol.as_str(), m, cgs_ref)
                    .unwrap_or_else(|| format!("{vsym} · {wire}"))
            } else {
                let mut description = format!("{vsym} · {wire}");
                if let Some(m) = &meta {
                    let d = m.description().trim();
                    if !d.is_empty() {
                        let t = crate::symbol_tuning::gloss_description_truncated(d);
                        description = format!("{vsym} · {wire} · {t}");
                    }
                }
                description
            };
            out.push(TeachingFieldGloss {
                symbol,
                field_type: String::new(),
                allowed_values: String::new(),
                description,
                is_inline_union_summary,
            });
            return;
        }
    }
    let typing_gloss: String = match (meta.as_ref(), symbol_map) {
        (Some(m), Some(sym)) => {
            if let Some(vs) = sym.value_sym_for_p_sym(symbol.as_str()) {
                sym.value_domain_gloss_for_v_sym(&vs)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| m.render_gloss_with_cgs(Some(sym), cgs))
            } else {
                m.render_gloss_with_cgs(Some(sym), cgs)
            }
        }
        (Some(m), None) => m.render_gloss_with_cgs(None, cgs),
        (None, _) => legend.to_string(),
    };
    let (mut field_type, legend_tail) = typing_gloss
        .split_once(" · ")
        .map(|(ty, tail)| (ty.trim().to_string(), tail.trim().to_string()))
        .unwrap_or_else(|| (typing_gloss.trim().to_string(), String::new()));
    if let Some(m) = &meta {
        let g = m.render_gloss_with_cgs(symbol_map, cgs);
        field_type = g
            .split_once(" \u{00b7} ")
            .map(|(a, _)| a.trim().to_string())
            .unwrap_or_else(|| g.trim().to_string());
    }
    let is_enumish = matches!(field_type.as_str(), "select" | "multiselect");
    let allowed_values = if is_enumish {
        legend_tail.clone()
    } else {
        meta.as_ref()
            .and_then(|m| m.allowed_values())
            .filter(|vals| !vals.is_empty())
            .map(|vals: &Vec<String>| vals.join(", "))
            .unwrap_or_default()
    };
    let mut description = meta
        .as_ref()
        .map(|m| m.description().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    if description.is_empty() && !is_enumish && !legend_tail.is_empty() {
        description = legend_tail;
    }
    out.push(TeachingFieldGloss {
        symbol,
        field_type,
        allowed_values,
        description,
        is_inline_union_summary,
    });
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

fn fill_scope_optional_from_sig(
    sig: &str,
    scope: &mut String,
    optional: &mut OptionalLegend,
    orphan: &mut String,
) {
    scope.clear();
    *optional = OptionalLegend::Absent;
    orphan.clear();
    let (sc, after_sc) = split_leading_scope_legend(sig);
    *scope = sc.to_string();
    let tail = after_sc.trim();
    if let Some(p) = tail
        .strip_prefix("optional params:")
        .or_else(|| tail.strip_prefix("opt:"))
    {
        if !p.trim().is_empty() {
            *optional = OptionalLegend::Present;
        }
    } else if !tail.is_empty() {
        *orphan = tail.to_string();
    }
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
    /// the same **full** entity slice as teaching table (see [`json_tool_surface_counts`] for slice rules).
    pub capability_tools: usize,
    /// Per entity in that slice: declared relations plus `EntityRef` fields whose name is not
    /// already a relation key (same merge as teaching table relation / ref navigation).
    pub navigation_tools: usize,
    /// Plasm path expression lines actually emitted in teaching table (per-entity dedupe only: identical
    /// lines in one entity block collapse once; the same string may repeat under another entity).
    pub json_tool_estimate: usize,
}

impl PromptSurfaceStats {
    /// Shared human-readable metrics for CLI stderr: chars, o200k tokens, teaching tool count.
    pub fn summary_line_body(&self) -> String {
        format!(
            "{} chars | ~{} tok (o200k) | ~{} tools (teaching table) | {} caps + {} nav (schema); ~{} tok (chars/4)",
            self.prompt_chars,
            self.prompt_tokens_o200k,
            self.json_tool_estimate,
            self.capability_tools,
            self.navigation_tools,
            self.token_estimate,
        )
    }
}

// ── Teaching table (many-shot examples) ───────────────────────────────────────────

/// Owning `entry_id` for an exposed entity wire name when it appears under exactly one catalog row.
#[inline]
fn catalog_entry_id_for_exposed_entity<'a>(
    qualified: &IndexMap<(&'a str, &'a str), ()>,
    entity: &str,
) -> Option<&'a str> {
    let mut matches: Vec<_> = qualified.keys().filter(|(_, e)| *e == entity).collect();
    match matches.len() {
        1 => Some(matches.pop().expect("len 1").0),
        _ => None,
    }
}

#[inline]
fn exposure_qualified_catalog_ids(
    exposure: &crate::symbol_tuning::TeachingExposureSession,
) -> IndexMap<(&str, &str), ()> {
    exposure
        .entities
        .iter()
        .zip(exposure.entity_catalog_entry_ids.iter())
        .map(|(entity, entry_id)| ((entry_id.as_str(), entity.as_str()), ()))
        .collect()
}

#[inline]
fn ent_sym(m: Option<&SymbolMap>, catalog_entry_id: &str, c: &str) -> String {
    m.map(|x| x.entity_sym_for(catalog_entry_id, c))
        .unwrap_or_else(|| c.to_string())
}

#[inline]
fn id_sym_entity(
    m: Option<&SymbolMap>,
    catalog_entry_id: &str,
    entity: &str,
    field: &str,
) -> String {
    m.map(|x| x.ident_sym_entity_field_for(catalog_entry_id, entity, field))
        .unwrap_or_else(|| field.to_string())
}

#[inline]
fn id_sym_cap(
    m: Option<&SymbolMap>,
    catalog_entry_id: &str,
    cap: &crate::CapabilitySchema,
    param: &str,
) -> String {
    m.map(|x| {
        x.ident_sym_cap_param_for(
            catalog_entry_id,
            cap.domain.as_str(),
            cap.name.as_str(),
            param,
        )
    })
    .unwrap_or_else(|| param.to_string())
}

#[inline]
fn id_sym_rel(m: Option<&SymbolMap>, catalog_entry_id: &str, entity: &str, rel: &str) -> String {
    m.map(|x| x.ident_sym_relation_for(catalog_entry_id, entity, rel))
        .unwrap_or_else(|| rel.to_string())
}

#[inline]
fn met_sym(
    m: Option<&SymbolMap>,
    catalog_entry_id: &str,
    entity: &str,
    cap: &crate::CapabilitySchema,
) -> String {
    m.map(|x| x.method_sym_for(catalog_entry_id, entity, cap.name.as_str()))
        .unwrap_or_else(|| capability_method_label_kebab(cap))
}

/// Human capability / list gloss after `[scope …]` / `optional params:` (emit parity with
/// [`format_capability_legend_line`]): Unicode em dash U+2014, spaces around it.
const LEGEND_EM_DESC_SEP: &str = " — ";

const PROJECTION_WITNESS_LEGEND_MARK: &str = "· projection";

/// Ordered receiver bases for teaching table dotted calls / relation nav on `ent` (`es` = entity symbol).
fn nav_receiver_candidates(
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
fn receiver_for_dotted_suffix(
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

const MAX_INCOMING_REL_NAV_PROJECTION_BASES: usize = 16;

/// `ParentRecv.rel` expressions that type-check and return `target_ename` (incoming edges).
///
/// With `surface_filter: Some`, only edges whose **parent** (`src_name`) is in
/// [`ExposureSurface::entities`] and passes [`surface_allows_relation_nav`] for that slot are kept —
/// symmetric with outgoing relation-nav rows on the parent entity block.
#[allow(clippy::too_many_arguments)]
fn incoming_relation_nav_bases_to_entity(
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
            if rel_s.cardinality == Cardinality::Many && !many_relation_nav_emittable(rel_s) {
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
fn projection_witness_source_capability<'a>(
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
fn try_push_projection_witness_row(
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
        );
    }
    false
}

/// In teaching table synthetic lines, bare `$` marks a **placeholder** for the real parameter value — use the
/// corresponding `p#` gloss line; it is not a literal value to send to the API. Search rows teach
/// `e#~"text"` (quoted meta-literal); never `e#~$`.
const TEACHING_PARAM_VALUE_PLACEHOLDER: &str = "$";

fn truncate_inline_desc(s: &str, max: usize) -> String {
    let t = crate::symbol_tuning::trim_description_for_agent_gloss(s).replace('\t', " ");
    crate::utf8_trunc::truncate_utf8_bytes_with_ellipsis(&t, max)
}

/// Strip authoring noise like ``(constructor `v101`)`` from variant descriptions before teaching table Meaning.
fn strip_union_constructor_authoring_noise(raw: &str) -> String {
    let mut s = raw.to_string();
    while let Some(start) = s.find("(constructor ") {
        let Some(close_rel) = s[start..].find(')') else {
            break;
        };
        let close = start + close_rel;
        let inner = s[start + "(constructor ".len()..close].trim();
        let noise = inner.contains('v') && inner.chars().any(|c| c.is_ascii_digit());
        if !noise {
            break;
        }
        let before = s[..start].trim_end();
        let after = s[close + 1..].trim_start();
        s = if before.is_empty() {
            after.to_string()
        } else if after.is_empty() {
            before.to_string()
        } else {
            format!("{before} {after}")
        };
    }
    s.trim().to_string()
}

/// Receiver token for relation-nav teaching: symbolic leading `e#`, else canonical entity name before `(` / `{`.
fn relation_receiver_teaching_hint(expr: &str, map: Option<&SymbolMap>) -> Option<String> {
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

fn relation_nav_meaning_result_gloss(
    expr: &str,
    map: Option<&SymbolMap>,
    target_gloss: String,
) -> String {
    match relation_receiver_teaching_hint(expr, map) {
        Some(h) => format!("relation {h} → {target_gloss}"),
        None => target_gloss,
    }
}

/// Thin relation-hop rows for expand/federate waves (parent entity already exposed; target just seeded).
pub(crate) fn render_relation_edge_delta_rows(
    exposure: &crate::symbol_tuning::TeachingExposureSession,
    new_relation_slots: &[crate::symbol_tuning::ExposureSlotKey],
    map: Option<&SymbolMap>,
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
        if !many_relation_nav_emittable(rel_schema) {
            continue;
        }
        let Some(es) =
            exposure.qualified_entity_symbol(source.entry_id.as_str(), source.entity.as_str())
        else {
            continue;
        };
        let r_sym = id_sym_rel(
            map,
            source.entry_id.as_str(),
            source.entity.as_str(),
            relation.as_str(),
        );
        if !r_sym.starts_with('r') {
            continue;
        }
        let plasm_expr = format!("{es}.{r_sym}");
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
        if let Some(m) = map {
            if let Some(gloss) = teaching_relation_field_gloss(m, r_sym.as_str(), &description) {
                if seen_r_gloss.insert(r_sym.clone()) {
                    write_teaching_tsv_row(&mut out, DomainTsvRow::FieldGloss(&gloss));
                }
            }
        }
        let cardinality_many = rel_schema.cardinality == Cardinality::Many;
        let target_gloss = crate::result_gloss::result_gloss_for_relation_nav(
            rel_schema.target_resource.as_str(),
            map,
            cardinality_many,
        );
        let result_type = relation_nav_meaning_result_gloss(&plasm_expr, map, target_gloss);
        let line = TeachingExprLine::empty_legend(plasm_expr);
        let line = TeachingExprLine {
            expression: line.expression,
            result_type,
            legend: line.legend,
            is_projection_teaching: false,
        };
        write_teaching_tsv_row(
            &mut out,
            DomainTsvRow::TeachingExpr {
                line: &line,
                identity_returns_row: false,
                attach_entity_heading: false,
                heading: &empty_heading,
            },
        );
    }
    out
}

/// Compound `Entity(p#=$,…)` when the target has multiple `key_vars` (per-key placeholders are still the string `$`).
///
/// Unary entity refs use [`unary_entity_id_teaching_expr_line`] / `$` fallback like scalar identity GET teaching.
fn entity_ref_id_example(
    cgs: &CGS,
    catalog_entry_id: &str,
    target: &str,
    map: Option<&SymbolMap>,
) -> String {
    let target_sym = ent_sym(map, catalog_entry_id, target);
    let p = TEACHING_PARAM_VALUE_PLACEHOLDER;
    let Some(ent) = cgs.get_entity(target) else {
        return format!("{target_sym}({})", TEACHING_PARAM_VALUE_PLACEHOLDER);
    };
    if ent.key_vars.len() > 1 {
        let parts: Vec<String> = ent
            .key_vars
            .iter()
            .map(|kv| {
                format!(
                    "{}={}",
                    id_sym_entity(map, catalog_entry_id, target, kv.as_str()),
                    p
                )
            })
            .collect();
        format!("{}({})", target_sym, parts.join(", "))
    } else {
        unary_entity_id_teaching_expr_line(&target_sym, ent, map, catalog_entry_id)
    }
}

fn entity_ref_target_in_session(
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    target: &str,
) -> bool {
    map.is_some_and(|m| {
        m.try_entity_teaching_term_for(catalog_entry_id, target)
            .is_some()
    })
}

fn unseeded_entity_ref_invocation_gloss(
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    let mut hints = Vec::new();
    for f in cap.object_params()? {
        let Ok(nv) = f.named_value(cgs) else {
            continue;
        };
        let FieldType::EntityRef { target } = &nv.field_type else {
            continue;
        };
        if entity_ref_target_in_session(map, catalog_entry_id, target.as_str()) {
            continue;
        }
        let param = id_sym_cap(map, catalog_entry_id, cap, f.name.as_str());
        hints.push(format!(
            "{param} takes {} — discover/seed it first",
            target.as_str()
        ));
    }
    if hints.is_empty() {
        None
    } else {
        Some(format!("· {}", hints.join("; ")))
    }
}

/// One `p#=value` in `Entity{p#=,…}` — same placeholder discipline as [`invoke_dotted_call_arg_example`].
fn query_param_slot_example(
    f: &crate::InputFieldSchema,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> String {
    let Ok(nv) = f.named_value(cgs) else {
        let n = id_sym_cap(map, catalog_entry_id, cap, f.name.as_str());
        return format!("{n}={}", TEACHING_PARAM_VALUE_PLACEHOLDER);
    };
    if matches!(nv.field_type, FieldType::Array) {
        // Array predicates in teaching table teaching use bare `$` so query type-check can apply
        // capability-param placeholder relaxation (`field=$`) for list-like filters.
        let n = id_sym_cap(map, catalog_entry_id, cap, f.name.as_str());
        return format!("{n}={}", TEACHING_PARAM_VALUE_PLACEHOLDER);
    }
    invoke_dotted_call_arg_example(f, cap, cgs, map, catalog_entry_id).unwrap_or_else(|| {
        let n = id_sym_cap(map, catalog_entry_id, cap, f.name.as_str());
        let p = TEACHING_PARAM_VALUE_PLACEHOLDER;
        match &nv.field_type {
            FieldType::Integer | FieldType::Number | FieldType::Boolean => {
                format!("{n}={p}")
            }
            FieldType::String | FieldType::Blob | FieldType::Uuid => format!("{n}={p}"),
            FieldType::Date => format!("{n}={p}", n = n, p = p),
            FieldType::Select | FieldType::MultiSelect => {
                format!("{n}={p}", n = n, p = p)
            }
            FieldType::EntityRef { target } => {
                format!(
                    "{n}={}",
                    entity_ref_id_example(cgs, catalog_entry_id, target, map)
                )
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

/// Parse `[p#,…]` into ordered symbols (empty when not a bracket).
fn projection_bracket_syms(bracket: &str) -> Vec<String> {
    bracket
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Order-independent equality of projection field sets (Get vs Query may differ only in order).
fn projection_field_sets_equal(a: &[String], b: &[String]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut aa = a.to_vec();
    let mut bb = b.to_vec();
    aa.sort();
    bb.sort();
    aa == bb
}

/// When the entity projection witness already taught `canonical_bracket`, omit the same
/// `[p#,…]` / `rows:` contract on row-producer lines (parser treats projection as optional).
#[allow(clippy::too_many_arguments)]
fn enrich_row_producer_teaching_line(
    cgs: &CGS,
    cap: &crate::CapabilitySchema,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    ename: &str,
    surface_filter: Option<&ExposureSurface>,
    base_expr: &str,
    base_gloss: Option<String>,
    projection: RowProducerProjection,
    canonical_bracket: Option<&str>,
    witness_taught: bool,
) -> (String, Option<String>) {
    let bracket = match projection {
        RowProducerProjection::BareQueryListAll => None,
        RowProducerProjection::CapabilityProvides => {
            let b = capability_row_projection_bracket(
                cgs,
                cap,
                map,
                catalog_entry_id,
                ename,
                surface_filter,
            );
            if witness_taught {
                if let (Some(br), Some(canon)) = (b.as_deref(), canonical_bracket) {
                    let br_syms = projection_bracket_syms(br);
                    let canon_syms = projection_bracket_syms(canon);
                    if projection_field_sets_equal(&br_syms, &canon_syms) {
                        None
                    } else {
                        b
                    }
                } else {
                    b
                }
            } else {
                b
            }
        }
    };
    let row_syms = bracket
        .as_ref()
        .map(|b| {
            b.trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let input_syms = input_param_syms_from_teaching_expr(base_expr, cap, map, catalog_entry_id);
    let expr = if let Some(b) = bracket {
        format!("{}{}", base_expr.trim(), b)
    } else {
        base_expr.trim().to_string()
    };
    let gloss = merge_result_gloss_with_row_contract(base_gloss, &input_syms, &row_syms);
    (expr, gloss)
}

fn row_producer_projection_for_query_line(
    cap: &crate::CapabilitySchema,
    entity_sym: &str,
    line: &str,
) -> RowProducerProjection {
    if cap.kind == crate::CapabilityKind::Query && line == entity_sym {
        RowProducerProjection::BareQueryListAll
    } else {
        RowProducerProjection::CapabilityProvides
    }
}

/// Bracket `[p#,…]` from a capability's ordered `provides` (row contract), when non-empty.
fn capability_row_projection_bracket(
    cgs: &CGS,
    cap: &crate::CapabilitySchema,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    ename: &str,
    surface_filter: Option<&ExposureSurface>,
) -> Option<String> {
    let fields = cgs.effective_ordered_response_fields(cap);
    if fields.is_empty() {
        return None;
    }
    let syms: Vec<String> = fields
        .iter()
        .filter(|k| {
            surface_allows_entity_field(surface_filter, catalog_entry_id, ename, k.as_str())
        })
        .map(|k| id_sym_entity(map, catalog_entry_id, ename, k.as_str()))
        .collect();
    if syms.is_empty() {
        return None;
    }
    Some(format!("[{}]", syms.join(",")))
}

/// Opaque `p#` symbols for params appearing in `{…}` / `~"…"{…}` teaching exemplars.
fn input_param_syms_from_teaching_expr(
    expr: &str,
    cap: &crate::CapabilitySchema,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Vec<String> {
    let Some(open) = expr.find('{') else {
        return Vec::new();
    };
    let rest = &expr[open + 1..];
    let close = rest.find('}').unwrap_or(rest.len());
    let inner = rest[..close].trim();
    if inner.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for slot in split_top_level_commas(inner) {
        let slot = slot.trim();
        let Some((lhs, _)) = slot.split_once('=') else {
            continue;
        };
        let lhs = lhs.trim();
        if lhs.starts_with('p') && lhs.chars().skip(1).all(|c| c.is_ascii_digit()) {
            if !out.iter().any(|s: &String| s == lhs) {
                out.push(lhs.to_string());
            }
            continue;
        }
        if let Some(is) = &cap.input_schema {
            if let InputType::Object { fields, .. } = &is.input_type {
                for f in fields {
                    let sym = id_sym_cap(map, catalog_entry_id, cap, f.name.as_str());
                    if sym == lhs && !out.iter().any(|s| s == &sym) {
                        out.push(sym);
                    }
                }
            }
        }
    }
    out
}

fn split_top_level_commas(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, ch) in input.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(input[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(input[start..].to_string());
    out
}

fn merge_result_gloss_with_row_contract(
    base_gloss: Option<String>,
    input_syms: &[String],
    row_syms: &[String],
) -> Option<String> {
    let mut suffix_parts = Vec::new();
    if !input_syms.is_empty() {
        suffix_parts.push(format!("inputs: {}", input_syms.join(",")));
    }
    if !row_syms.is_empty() {
        suffix_parts.push(format!("rows: {}", row_syms.join(",")));
    }
    let suffix = suffix_parts.join(" · ");
    match (base_gloss.filter(|s| !s.is_empty()), suffix.is_empty()) {
        (Some(base), false) => Some(format!("{base} · {suffix}")),
        (Some(base), true) => Some(base),
        (None, false) => Some(suffix),
        (None, true) => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn try_push_row_producer_teaching_example(
    gloss_emit: &mut Option<GlossScratch<'_>>,
    teaching_rows: &mut Vec<EntityTeachingExprRow>,
    collect_meta: bool,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    ename: &str,
    surface_filter: Option<&ExposureSurface>,
    cap: &crate::CapabilitySchema,
    base_expr: &str,
    base_gloss: Option<String>,
    cap_leg: Option<String>,
    relation: Option<&RelationSchema>,
    omit_capability_prose: bool,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
    projection: RowProducerProjection,
    canonical_bracket: Option<&str>,
    witness_taught: bool,
) -> bool {
    let (expr, gloss) = enrich_row_producer_teaching_line(
        cgs,
        cap,
        map,
        catalog_entry_id,
        ename,
        surface_filter,
        base_expr,
        base_gloss,
        projection,
        canonical_bracket,
        witness_taught,
    );
    try_push_teaching_example(
        gloss_emit,
        teaching_rows,
        collect_meta,
        cgs,
        &expr,
        gloss,
        cap_leg,
        relation,
        Some(&cap.name),
        omit_capability_prose,
        line_valid_cache,
        line_valid_cache_seed,
        map_arc,
    )
}

/// One `p#=value` for a **required scope** parameter (same as filter slots).
fn scope_param_slot(
    f: &InputFieldSchema,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> String {
    query_param_slot_example(f, cap, cgs, map, catalog_entry_id)
}

/// `Entity(k=v,…)` for multi-`key_vars` GET examples (validated like other teaching lines).
fn compound_get_expr_line(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    if ent.key_vars.len() <= 1 {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    let p = TEACHING_PARAM_VALUE_PLACEHOLDER;
    for kv in &ent.key_vars {
        let f = ent.fields.get(kv)?;
        let sym = id_sym_entity(map, catalog_entry_id, ent.name.as_str(), kv.as_str());
        let nv = f.named_value(cgs).ok()?;
        match &nv.field_type {
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
                parts.push(format!(
                    "{sym}={}",
                    entity_ref_id_example(cgs, catalog_entry_id, target, map)
                ));
            }
        }
    }
    Some(format!("{es}({})", parts.join(", ")))
}

/// Unary identity GET teaching: positional literal for simple string ids (e.g. `e#(pikachu)` on
/// Pokemon), otherwise opaque **`p#`** (`e#(p…)`) when the field has an allocated teaching ident
/// symbol; otherwise **`e#($)`** (canonical / unresolved gloss).
fn unary_entity_id_teaching_expr_line(
    es: &str,
    ent: &EntityDef,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> String {
    if let Some(literal) = positional_identity_teaching_literal(ent) {
        return format!("{es}({literal})");
    }
    let sym = id_sym_entity(
        map,
        catalog_entry_id,
        ent.name.as_str(),
        ent.id_field.as_str(),
    );
    if map.is_some_and(|m| m.resolve_session_slot(sym.as_str()).is_ok()) {
        format!("{es}({sym})")
    } else {
        format!("{es}({})", TEACHING_PARAM_VALUE_PLACEHOLDER)
    }
}

/// Literal positional identity for teaching rows (B2): simple string `id_field`, no compound keys.
fn positional_identity_teaching_literal(ent: &EntityDef) -> Option<&'static str> {
    if !ent.key_vars.is_empty() {
        return None;
    }
    match ent.id_format {
        Some(crate::schema::IdFormat::Uuid) | Some(crate::schema::IdFormat::Integer) => None,
        Some(crate::schema::IdFormat::Email) => Some("user@example.com"),
        Some(crate::schema::IdFormat::Other) => None,
        Some(crate::schema::IdFormat::Slug) | None => match ent.name.as_str() {
            "Pokemon" => Some("pikachu"),
            _ if ent.id_field.as_str() == "name" => Some("example-name"),
            _ => None,
        },
    }
}

/// Scope predicates + all filter-like parameters (required + optional) with CGS-derived placeholders.
fn query_expr_maximal(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
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
        inner.push(scope_param_slot(sf, cap, cgs, map, catalog_entry_id));
    }

    for f in fields {
        if matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        inner.push(query_param_slot_example(f, cap, cgs, map, catalog_entry_id));
    }

    if inner.is_empty() {
        return Some(es.to_string());
    }
    Some(format!("{es}{{{}}}", inner.join(", ")))
}

/// Filter predicates only (no scope) — one `Entity{p#=…}` line per query cap so teaching table shows **filter**
/// field symbols even when scope+filters are merged on the maximal line.
fn query_expr_filters_only(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
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
        inner.push(query_param_slot_example(f, cap, cgs, map, catalog_entry_id));
    }
    if inner.is_empty() {
        return None;
    }
    Some(format!("{es}{{{}}}", inner.join(", ")))
}

/// Search filter slots for `e#~"text"{p#=…}` — same param selection as [`query_expr_filters_only`].
fn search_expr_with_filters(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
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
        if matches!(f.role, Some(ParameterRole::Search)) {
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        inner.push(query_param_slot_example(f, cap, cgs, map, catalog_entry_id));
    }
    if inner.is_empty() {
        return None;
    }
    Some(format!("{es}~\"text\"{{{}}}", inner.join(", ")))
}

/// Only scope predicates (for a distinct structural example when maximal adds filters).
fn query_expr_scope_only(
    cap: &crate::CapabilitySchema,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
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
        inner.push(scope_param_slot(sf, cap, cgs, map, catalog_entry_id));
    }
    Some(format!("{es}{{{}}}", inner.join(", ")))
}

#[inline]
fn path_vars_empty(cap: &crate::CapabilitySchema) -> bool {
    !cap.domain_exemplar_requires_entity_anchor()
}

/// Cardinality-many relation nav `Source(id).rel` parses to [`Expr::Chain`] when `materialize` is set;
/// with [`RelationMaterialization::Unavailable`], parse fails — omit teaching lines that cannot validate.
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

/// teaching table line metadata from an already type-checked [`Expr`] (avoids a second parse in the render hot path).
fn domain_line_execution_meta_from_validated(
    cgs: &CGS,
    work: String,
    relation: Option<&RelationSchema>,
    source_capability: Option<&CapabilityName>,
    expr: &Expr,
) -> TeachingLineMeta {
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
            Expr::Chain(_)
            | Expr::Page(_)
            | Expr::Wait(_)
            | Expr::Cancel(_)
            | Expr::TeachingValue { .. } => DomainLineKind::Other,
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

    TeachingLineMeta {
        expression: work,
        kind,
        source_capability: source_capability.map(|n| n.to_string()),
        cross_entity,
        relation_materialization,
    }
}

#[allow(clippy::too_many_arguments)]
fn try_push_teaching_example(
    gloss_emit: &mut Option<GlossScratch<'_>>,
    teaching_rows: &mut Vec<EntityTeachingExprRow>,
    collect_meta: bool,
    cgs: &CGS,
    expr: &str,
    gloss: Option<String>,
    cap_leg: Option<String>,
    relation: Option<&RelationSchema>,
    source_capability: Option<&CapabilityName>,
    // When true: strip [`TeachingExprLine::description`] from capability legend (Query/Get/Search);
    // scope / optional params / compact args remain.
    omit_capability_prose: bool,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) -> bool {
    if let Some(gs) = gloss_emit.as_mut() {
        let optional_syms: Vec<String> = match (map_arc, source_capability) {
            (Some(map), Some(cap_name)) => {
                cgs.get_capability(cap_name.as_str())
                    .map_or_else(Vec::new, |cap| {
                        crate::symbol_tuning::optional_legend_param_syms(
                            map.as_ref(),
                            cgs.entry_id.as_deref().unwrap_or(""),
                            cap.domain.as_str(),
                            cap,
                        )
                    })
            }
            _ => Vec::new(),
        };
        gs.emit_before_teaching_example(expr, cap_leg.as_deref(), gloss.as_deref(), &optional_syms);
    }
    let mut teaching_line =
        teaching_expr_line_from_layers(expr, gloss.as_deref(), cap_leg.as_deref());
    if omit_capability_prose {
        teaching_line.legend.description.clear();
    }
    let dedupe_key = TeachingRowDedupeKey::new(expr, gloss.as_ref(), cap_leg.as_ref());

    let Some((parsed, work)) =
        domain_line_validate_cached(line_valid_cache, line_valid_cache_seed, cgs, expr, map_arc)
    else {
        return false;
    };

    let meta = if collect_meta {
        domain_line_execution_meta_from_validated(
            cgs,
            work,
            relation,
            source_capability,
            &parsed.expr,
        )
    } else {
        TeachingLineMeta {
            expression: work,
            kind: DomainLineKind::Other,
            source_capability: None,
            cross_entity: None,
            relation_materialization: None,
        }
    };
    teaching_rows.push(EntityTeachingExprRow {
        teaching_expr: teaching_line,
        meta,
        dedupe_key,
    });
    true
}

/// Omit path-bound scope keys from explicit dotted-call `(…)` when they are already supplied by the
/// receiver: unary `Entity($)` / symbolic unary `e#(p#)` identity injects `{entity}_id`, and compound
/// `Entity(k1=$, k2=$)` injects each `key_vars` slot that also appears as a path template variable.
fn field_omitted_from_path_inject(
    ent: &EntityDef,
    cap: &crate::CapabilitySchema,
    field_name: &str,
) -> bool {
    let path_vars = crate::schema::path_var_names_from_mapping_json(&cap.mapping.template.0);
    if !path_vars.iter().any(|pv| pv == field_name) {
        return false;
    }
    let unary_anchor_id = format!("{}_id", ent.name.to_lowercase());
    if field_name == unary_anchor_id {
        return true;
    }
    // Compound receiver `Entity(k1=$,…)` may inject path vars that duplicate explicit scope args,
    // but only when every identity key that appears on this capability's HTTP path is also a
    // declared required scope parameter (some APIs bind extra path segments purely from row keys).
    if ent.key_vars.len() > 1 {
        if let Some(is) = cap.input_schema.as_ref() {
            if let InputType::Object { fields, .. } = &is.input_type {
                let required_scope: HashSet<&str> = fields
                    .iter()
                    .filter(|f| f.required && matches!(f.role, Some(ParameterRole::Scope)))
                    .map(|f| f.name.as_str())
                    .collect();
                let path_set: HashSet<&str> = path_vars.iter().map(|s| s.as_str()).collect();
                let every_path_bound_key_declared = ent.key_vars.iter().all(|kv| {
                    let k = kv.as_str();
                    !path_set.contains(k) || required_scope.contains(k)
                });
                if every_path_bound_key_declared
                    && ent.key_vars.iter().any(|kv| kv.as_str() == field_name)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Capability legend after result gloss in teaching rows: `[scope …]` / `optional params: …` only.
/// Required invoke parameters are implicit from the taught expression; standalone `p#` gloss rows
/// carry wire names and types.
fn format_capability_legend_line(
    map: &SymbolMap,
    cgs: &CGS,
    cap: &crate::CapabilitySchema,
    _anchor_entity: &str,
    _ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    _catalog_entry_id: &str,
) -> String {
    const MAX_DESC: usize = 80;
    let kebab = capability_method_label_kebab(cap);
    let raw = cap.description.as_str().trim();
    let gloss = if raw.is_empty() {
        kebab
    } else {
        truncate_inline_desc(raw, MAX_DESC)
    };
    let sig = map.capability_input_signature_gloss(cgs, cap);
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
    cgs: &CGS,
    cap: &crate::CapabilitySchema,
    anchor_entity: &str,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    catalog_entry_id: &str,
) -> Option<String> {
    map.map(|m| {
        format_capability_legend_line(m, cgs, cap, anchor_entity, ident_meta, catalog_entry_id)
    })
}

#[inline]
fn capability_legend_with_session_gloss(
    map: Option<&SymbolMap>,
    cgs: &CGS,
    cap: &crate::CapabilitySchema,
    anchor_entity: &str,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    catalog_entry_id: &str,
) -> Option<String> {
    let mut leg =
        capability_legend_for_domain(map, cgs, cap, anchor_entity, ident_meta, catalog_entry_id)?;
    if let Some(hint) = unseeded_entity_ref_invocation_gloss(cap, cgs, map, catalog_entry_id) {
        if !leg.is_empty() {
            leg.push(' ');
        }
        leg.push_str(&hint);
    }
    Some(leg)
}

/// Structural invoke RHS inside union constructors (`v101{…}`): keyed by opaque `p#` when a
/// [`SymbolMap`] is present (teaching TSV); canonical [`RenderMode`] uses wire names.
fn format_inline_structural_example_symbolic(
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    domain: &str,
    cap_name: &str,
    path_prefix: &str,
    ty: &crate::InputType,
    _cgs: &CGS,
) -> String {
    match ty {
        crate::InputType::None | crate::InputType::Value { .. } => {
            TEACHING_PARAM_VALUE_PLACEHOLDER.to_string()
        }
        crate::InputType::Object { fields, .. } => {
            let mut parts = Vec::new();
            for sf in fields {
                let seg = if path_prefix.is_empty() {
                    sf.name.clone()
                } else {
                    format!("{path_prefix}.{}", sf.name)
                };
                match &sf.wire {
                    crate::InputFieldWire::Inline(inner) => {
                        let rhs = format_inline_structural_example_symbolic(
                            map,
                            catalog_entry_id,
                            domain,
                            cap_name,
                            &seg,
                            inner.as_ref(),
                            _cgs,
                        );
                        let lhs = map
                            .map(|m| {
                                m.ident_sym_cap_param_for(
                                    catalog_entry_id,
                                    domain,
                                    cap_name,
                                    seg.as_str(),
                                )
                            })
                            .unwrap_or_else(|| sf.name.clone());
                        parts.push(format!("{lhs}={rhs}"));
                    }
                    crate::InputFieldWire::Registry(_) => {
                        let lhs = map
                            .map(|m| {
                                m.ident_sym_cap_param_for(
                                    catalog_entry_id,
                                    domain,
                                    cap_name,
                                    seg.as_str(),
                                )
                            })
                            .unwrap_or_else(|| sf.name.clone());
                        parts.push(format!("{lhs}={}", TEACHING_PARAM_VALUE_PLACEHOLDER));
                    }
                }
            }
            format!("{{{}}}", parts.join(","))
        }
        crate::InputType::Array { element_type, .. } => {
            format!(
                "[{}]",
                format_inline_structural_example_symbolic(
                    map,
                    catalog_entry_id,
                    domain,
                    cap_name,
                    path_prefix,
                    element_type.as_ref(),
                    _cgs,
                )
            )
        }
        crate::InputType::Union { .. } => TEACHING_PARAM_VALUE_PLACEHOLDER.to_string(),
    }
}

/// Like [`format_inline_structural_example_symbolic`] for an object body, but **only required** fields
/// and **no** `,..` optional tail — union constructor payloads inside `{…}` must parse as plain `k=v` pairs.
fn format_inline_structural_example_symbolic_required_only(
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    domain: &str,
    cap_name: &str,
    path_prefix: &str,
    ty: &crate::InputType,
    cgs: &CGS,
) -> String {
    let crate::InputType::Object { fields, .. } = ty else {
        return format_inline_structural_example_symbolic(
            map,
            catalog_entry_id,
            domain,
            cap_name,
            path_prefix,
            ty,
            cgs,
        );
    };
    let mut parts = Vec::new();
    for sf in fields {
        if !sf.required {
            continue;
        }
        let seg = if path_prefix.is_empty() {
            sf.name.clone()
        } else {
            format!("{path_prefix}.{}", sf.name)
        };
        match &sf.wire {
            crate::InputFieldWire::Inline(inner) => {
                let rhs = format_inline_structural_example_symbolic_required_only(
                    map,
                    catalog_entry_id,
                    domain,
                    cap_name,
                    &seg,
                    inner.as_ref(),
                    cgs,
                );
                let lhs = map
                    .map(|m| {
                        m.ident_sym_cap_param_for(catalog_entry_id, domain, cap_name, seg.as_str())
                    })
                    .unwrap_or_else(|| sf.name.clone());
                parts.push(format!("{lhs}={rhs}"));
            }
            crate::InputFieldWire::Registry(_) => {
                let lhs = map
                    .map(|m| {
                        m.ident_sym_cap_param_for(catalog_entry_id, domain, cap_name, seg.as_str())
                    })
                    .unwrap_or_else(|| sf.name.clone());
                parts.push(format!("{lhs}={}", TEACHING_PARAM_VALUE_PLACEHOLDER));
            }
        }
    }
    let inner = parts.join(",");
    format!("{{{inner}}}")
}

fn format_union_constructor_invoke_example(
    variant: &crate::schema::InputVariantSchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    domain: &str,
    cap_name: &str,
    operations_field: &str,
) -> Option<String> {
    let ctor = crate::schema::union_variant_constructor_symbol(variant)?;
    let body_ty = crate::schema::input_variant_body_type(variant);
    let prefix = format!("{}.{}", operations_field, variant.name);
    Some(format!(
        "{}{}",
        ctor,
        format_inline_structural_example_symbolic(
            map,
            catalog_entry_id,
            domain,
            cap_name,
            &prefix,
            &body_ty,
            cgs,
        )
    ))
}

/// Root-level invoke union (`input_schema.type: union`): ctor body uses flat param paths (`p5`, …).
fn format_root_union_constructor_invoke_example(
    variant: &crate::schema::InputVariantSchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    domain: &str,
    cap_name: &str,
) -> Option<String> {
    let ctor = crate::schema::union_variant_constructor_symbol(variant)?;
    let body_ty = crate::schema::input_variant_body_type(variant);
    Some(format!(
        "{}{}",
        ctor,
        format_inline_structural_example_symbolic_required_only(
            map,
            catalog_entry_id,
            domain,
            cap_name,
            "",
            &body_ty,
            cgs
        )
    ))
}

/// `v101`-row **Meaning** column: variant discriminator name + prose (not the symbolic ctor shape).
fn format_union_constructor_gloss_legend(v: &crate::schema::InputVariantSchema) -> String {
    const MAX_DESC: usize = 120;
    let disc = v.name.as_str();
    let raw =
        strip_union_constructor_authoring_noise(v.description.as_deref().unwrap_or("").trim());
    if raw.is_empty() {
        return disc.to_string();
    }
    format!(
        "{disc}{LEGEND_EM_DESC_SEP}{}",
        truncate_inline_desc(&raw, MAX_DESC)
    )
}

fn emit_union_array_constructor_teaching_gloss(
    gs: &mut GlossScratch<'_>,
    union_ty: &crate::InputType,
) {
    let crate::InputType::Union { variants } = union_ty else {
        return;
    };
    if variants.is_empty()
        || variants
            .iter()
            .any(|v| crate::schema::union_variant_constructor_symbol(v).is_none())
    {
        return;
    }
    let mut keys = BTreeSet::new();
    crate::schema::collect_registry_keys_from_input_type(union_ty, &mut keys);
    let cid = gs.catalog_entry_id;
    for key in keys {
        let fp = format!("{}|vr:{}", cid, key.as_str());
        if let Some(vsym) = gs.map.value_domain_fp_to_sym().get(&fp) {
            let vs = vsym.as_wire();
            if let Some(vg) = gs.map.value_domain_gloss_for_v_sym(&vs) {
                let Some(v_canon) = meaning_canonical_sym_for_emit(
                    vg,
                    &vs,
                    &mut gs.state.registry_value_gloss_canonical_v,
                    &mut gs.state.registry_v_sym_alias,
                ) else {
                    continue;
                };
                if gs.state.defined_value_domains.insert(v_canon.clone()) {
                    push_teaching_field_gloss_row(
                        gs.field_gloss,
                        v_canon,
                        vg,
                        gs.entity,
                        cid,
                        Some(gs.map),
                        Some(gs.meta),
                        Some(gs.cgs),
                        false,
                    );
                }
            }
        }
    }
    let alts: Vec<&str> = variants
        .iter()
        .filter_map(crate::schema::union_variant_constructor_symbol)
        .collect();
    let union_summary = format!("union · {}", alts.join(" | "));
    let summary_sym = crate::symbol_tuning::next_opaque_v_symbol_after_map_and_extra_syms(
        gs.map,
        gs.field_gloss.iter().map(|g| g.symbol.as_str()),
    );
    push_teaching_field_gloss_row(
        gs.field_gloss,
        summary_sym,
        &union_summary,
        gs.entity,
        cid,
        Some(gs.map),
        Some(gs.meta),
        Some(gs.cgs),
        true,
    );
}

fn emit_array_of_union_constructor_teaching_gloss(
    gs: &mut GlossScratch<'_>,
    cap: &crate::CapabilitySchema,
) {
    let Some(is) = cap.input_schema.as_ref() else {
        return;
    };
    if let crate::InputType::Union { variants } = &is.input_type {
        if variants.is_empty()
            || variants
                .iter()
                .any(|v| crate::schema::union_variant_constructor_symbol(v).is_none())
        {
            return;
        }
        emit_union_array_constructor_teaching_gloss(gs, &is.input_type);
        return;
    }
    let crate::InputType::Object { fields, .. } = &is.input_type else {
        return;
    };
    for field in fields {
        let crate::InputFieldWire::Inline(ty) = &field.wire else {
            continue;
        };
        let crate::InputType::Array { element_type, .. } = ty.as_ref() else {
            continue;
        };
        let el = element_type.as_ref();
        let crate::InputType::Union { variants } = el else {
            continue;
        };
        if variants.is_empty()
            || variants
                .iter()
                .any(|v| crate::schema::union_variant_constructor_symbol(v).is_none())
        {
            continue;
        }
        emit_union_array_constructor_teaching_gloss(gs, el);
        return;
    }
}

/// One validated teaching row per union variant constructor (`v101{p#=$,…}`) before the dotted-call assembly line.
#[allow(clippy::too_many_arguments)]
fn try_push_union_constructor_teaching_expr_rows(
    gloss_emit: &mut Option<GlossScratch<'_>>,
    teaching_rows: &mut Vec<EntityTeachingExprRow>,
    collect_meta: bool,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    cap: &crate::CapabilitySchema,
    catalog_entry_id: &str,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) {
    let Some(is) = cap.input_schema.as_ref() else {
        return;
    };
    if let crate::InputType::Union { variants } = &is.input_type {
        if variants.is_empty()
            || variants
                .iter()
                .any(|v| crate::schema::union_variant_constructor_symbol(v).is_none())
        {
            return;
        }
        for v in variants {
            let Some(expr_line) = format_root_union_constructor_invoke_example(
                v,
                cgs,
                map,
                catalog_entry_id,
                cap.domain.as_str(),
                cap.name.as_str(),
            ) else {
                continue;
            };
            let legend = format_union_constructor_gloss_legend(v);
            let _ = try_push_teaching_example(
                gloss_emit,
                teaching_rows,
                collect_meta,
                cgs,
                &expr_line,
                Some(legend),
                None,
                None,
                Some(&cap.name),
                false,
                line_valid_cache,
                line_valid_cache_seed,
                map_arc,
            );
        }
        return;
    }
    let crate::InputType::Object { fields, .. } = &is.input_type else {
        return;
    };
    for field in fields {
        let crate::InputFieldWire::Inline(ty) = &field.wire else {
            continue;
        };
        let crate::InputType::Array { element_type, .. } = ty.as_ref() else {
            continue;
        };
        let el = element_type.as_ref();
        let crate::InputType::Union { variants } = el else {
            continue;
        };
        if variants.is_empty()
            || variants
                .iter()
                .any(|v| crate::schema::union_variant_constructor_symbol(v).is_none())
        {
            return;
        }
        for v in variants {
            let Some(expr_line) = format_union_constructor_invoke_example(
                v,
                cgs,
                map,
                catalog_entry_id,
                cap.domain.as_str(),
                cap.name.as_str(),
                field.name.as_str(),
            ) else {
                continue;
            };
            let legend = format_union_constructor_gloss_legend(v);
            let _ = try_push_teaching_example(
                gloss_emit,
                teaching_rows,
                collect_meta,
                cgs,
                &expr_line,
                Some(legend),
                None,
                None,
                Some(&cap.name),
                false,
                line_valid_cache,
                line_valid_cache_seed,
                map_arc,
            );
        }
        return;
    }
}

/// One `key=value` for dotted-call `method(k=v,…)` — equality/entity forms parse as invoke args (not query `>=` predicates).
fn invoke_dotted_call_arg_example(
    f: &crate::InputFieldSchema,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    let n = id_sym_cap(map, catalog_entry_id, cap, f.name.as_str());
    let p = TEACHING_PARAM_VALUE_PLACEHOLDER;
    if let crate::InputFieldWire::Inline(ty) = &f.wire {
        return Some(match ty.as_ref() {
            crate::InputType::Array { element_type, .. } => {
                if let crate::InputType::Union { variants } = element_type.as_ref() {
                    if variants
                        .iter()
                        .all(|v| crate::schema::union_variant_constructor_symbol(v).is_some())
                    {
                        // Edit-v2-style ops use wire discriminator `op`; nested ctor RHS in dotted-call
                        // teaching lines type-check end-to-end. Proof `/ops` comment batches (and similar)
                        // discriminate with `type`; keep invoke exemplars placeholder-heavy — standalone
                        // union ctor rows still teach each `vNNN{…}` branch.
                        if variants.iter().all(|v| v.wire.field == "type") {
                            return Some(format!("{n}=[{p}]"));
                        }
                        let a = format_union_constructor_invoke_example(
                            variants.first()?,
                            cgs,
                            map,
                            catalog_entry_id,
                            cap.domain.as_str(),
                            cap.name.as_str(),
                            f.name.as_str(),
                        )?;
                        // Pair `replace_block` with `insert_after` so the teaching line shows both a
                        // flat `{markdown=$}` body and nested `[{markdown=$}]` blocks arrays.
                        let b = variants
                            .get(2)
                            .and_then(|vx| {
                                format_union_constructor_invoke_example(
                                    vx,
                                    cgs,
                                    map,
                                    catalog_entry_id,
                                    cap.domain.as_str(),
                                    cap.name.as_str(),
                                    f.name.as_str(),
                                )
                            })
                            .unwrap_or_else(|| a.clone());
                        return Some(format!("{n}=[{a},{b}]"));
                    }
                }
                format!("{n}=[{p}]")
            }
            _ => format!("{n}={p}"),
        });
    }
    let nv = match f.named_value(cgs) {
        Ok(nv) => nv,
        Err(_) => return Some(format!("{n}={p}")),
    };
    match &nv.field_type {
        FieldType::Boolean
        | FieldType::String
        | FieldType::Blob
        | FieldType::Json
        | FieldType::Uuid
        | FieldType::Integer
        | FieldType::Number => Some(format!("{n}={p}")),
        FieldType::Select | FieldType::MultiSelect => Some(format!("{n}={p}")),
        FieldType::EntityRef { target } => Some(format!(
            "{n}={}",
            entity_ref_id_example(cgs, catalog_entry_id, target, map)
        )),
        FieldType::Date => match &nv.value_format {
            // Same placeholder as strings — avoid teaching ISO literals in teaching table dotted-call invokes.
            Some(ValueWireFormat::Temporal(_)) => Some(format!(
                "{n}={p}",
                n = n,
                p = TEACHING_PARAM_VALUE_PLACEHOLDER
            )),
            _ => None,
        },
        FieldType::Array => match f.resolved_array_items(cgs) {
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
    catalog_entry_id: &str,
) -> Option<String> {
    let ent = cgs.get_entity(anchor_entity)?;
    let is = cap.input_schema.as_ref()?;
    if let InputType::Union { variants } = &is.input_type {
        if variants.is_empty()
            || variants
                .iter()
                .any(|v| crate::schema::union_variant_constructor_symbol(v).is_none())
        {
            return None;
        }
        let v = variants.first()?;
        return format_root_union_constructor_invoke_example(
            v,
            cgs,
            map,
            catalog_entry_id,
            cap.domain.as_str(),
            cap.name.as_str(),
        );
    }
    let InputType::Object { fields, .. } = &is.input_type else {
        return None;
    };
    let mut parts: Vec<String> = Vec::new();
    let mut required_example_failed = false;
    for f in fields {
        if !f.required || !matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if field_omitted_from_path_inject(ent, cap, f.name.as_str()) {
            continue;
        }
        parts.push(scope_param_slot(f, cap, cgs, map, catalog_entry_id));
    }
    for f in fields {
        if matches!(f.role, Some(ParameterRole::Scope)) {
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        if field_omitted_from_path_inject(ent, cap, f.name.as_str()) {
            continue;
        }
        match invoke_dotted_call_arg_example(f, cap, cgs, map, catalog_entry_id) {
            Some(a) => parts.push(a),
            None if f.required => required_example_failed = true,
            None => {}
        }
    }
    if required_example_failed {
        return None;
    }
    // Path-bound scope slots may be fully injected from a compound receiver (`Entity(k1=$,k2=$)`),
    // leaving only `method()` for zero-body deletes / similar invokes.
    if parts.is_empty() {
        return Some(String::new());
    }
    Some(parts.join(", "))
}

/// Parentheses for **standalone** `Entity.create(…)` when the capability has required `role: scope`
/// parameters (no anchor to inject them). [`build_dotted_call_paren_args`] skips scope fields;
/// without scope slots, lines like `Comment.create(text=…)` fail validation for nested REST creates.
fn build_standalone_create_paren_args(
    ename: &str,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
) -> Option<String> {
    if cap.kind != CapabilityKind::Create {
        return build_dotted_call_paren_args(ename, cap, cgs, map, catalog_entry_id);
    }
    let is = cap.input_schema.as_ref()?;
    let InputType::Object { fields, .. } = &is.input_type else {
        return None;
    };
    let has_required_scope = fields
        .iter()
        .any(|f| f.required && matches!(f.role, Some(ParameterRole::Scope)));
    if !has_required_scope {
        return build_dotted_call_paren_args(ename, cap, cgs, map, catalog_entry_id);
    }

    let ent = cgs.get_entity(ename)?;
    let mut parts: Vec<String> = Vec::new();
    let mut required_failed = false;
    for f in fields {
        if matches!(f.role, Some(ParameterRole::Scope)) {
            if f.required {
                parts.push(scope_param_slot(f, cap, cgs, map, catalog_entry_id));
            }
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        if field_omitted_from_path_inject(ent, cap, f.name.as_str()) {
            continue;
        }
        match invoke_dotted_call_arg_example(f, cap, cgs, map, catalog_entry_id) {
            Some(a) => parts.push(a),
            None if f.required => required_failed = true,
            None => {}
        }
    }
    if required_failed {
        return None;
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(", "))
}

#[allow(clippy::too_many_arguments)]
fn format_dotted_call_line(
    anchor_entity: &str,
    cap: &crate::CapabilitySchema,
    ent: &EntityDef,
    es: &str,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
) -> Option<String> {
    let args = build_dotted_call_paren_args(anchor_entity, cap, cgs, map, catalog_entry_id)?;
    let ms = met_sym(map, catalog_entry_id, cap.domain.as_str(), cap);
    let suffix = format!(".{ms}({args})");
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

const MAX_MULTI_ARITY_METHOD_LINES: usize = 16;

#[inline]
fn surface_allows_capability(
    surface: Option<&ExposureSurface>,
    catalog_entry_id: &str,
    cap: &crate::schema::CapabilitySchema,
) -> bool {
    let Some(s) = surface else {
        return true;
    };
    s.capabilities.contains(&ExposureCapabilityKey {
        entry_id: catalog_entry_id.to_string(),
        domain: cap.domain.clone(),
        capability: cap.name.clone(),
    })
}

#[inline]
fn surface_allows_entity_field(
    surface: Option<&ExposureSurface>,
    catalog_entry_id: &str,
    entity: &str,
    field: &str,
) -> bool {
    let Some(s) = surface else {
        return true;
    };
    let ekey = ExposureEntityKey {
        entry_id: catalog_entry_id.to_string(),
        entity: EntityName::from(entity),
    };
    s.slots.contains(&ExposureSlotKey::EntityField {
        entity: ekey,
        field: EntityFieldName::new(field.to_string()),
    })
}

#[inline]
fn surface_allows_relation_nav(
    surface: Option<&ExposureSurface>,
    catalog_entry_id: &str,
    entity: &str,
    relation: &str,
    is_declared_relation: bool,
) -> bool {
    let Some(s) = surface else {
        return true;
    };
    if is_declared_relation {
        let source = ExposureEntityKey {
            entry_id: catalog_entry_id.to_string(),
            entity: EntityName::from(entity),
        };
        return s.slots.contains(&ExposureSlotKey::Relation {
            source,
            relation: RelationName::new(relation.to_string()),
        });
    }
    surface_allows_entity_field(surface, catalog_entry_id, entity, relation)
}

/// Canonical catalog-qualified entity key for [`ExposureSurface::entities`] membership checks.
fn exposure_entity_key_for_surface(
    cgs: &CGS,
    catalog_entry_id: &str,
    raw_entity: &str,
) -> Option<ExposureEntityKey> {
    let raw = raw_entity.trim();
    if raw.is_empty() {
        return None;
    }
    for k in cgs.entities.keys() {
        if k.eq_ignore_ascii_case(raw) {
            return Some(ExposureEntityKey {
                entry_id: catalog_entry_id.to_string(),
                entity: EntityName::from(k.as_str()),
            });
        }
    }
    None
}

/// Catalog-qualified entity appears in [`ExposureSurface::entities`] (canonical name via CGS keys).
/// Without a surface (`None`), treated as included (legacy full teaching table).
#[inline]
fn surface_includes_exposed_entity(
    surface: Option<&ExposureSurface>,
    cgs: &CGS,
    catalog_entry_id: &str,
    raw_entity: &str,
) -> bool {
    let Some(s) = surface else {
        return true;
    };
    let Some(ekey) = exposure_entity_key_for_surface(cgs, catalog_entry_id, raw_entity) else {
        return false;
    };
    s.entities.contains(&ekey)
}

/// Relation-navigation rows (`… .r#` or wire toward another CGS entity, or declared relation chains) are only
/// taught when the **target** entity name appears in [`ExposureSurface::entities`] for the same
/// `catalog_entry_id`. Without a surface (`None`), navigation is unrestricted (legacy full teaching table).
#[inline]
fn surface_exposes_relation_nav_target(
    surface: Option<&ExposureSurface>,
    cgs: &CGS,
    catalog_entry_id: &str,
    target_entity: &str,
) -> bool {
    surface_includes_exposed_entity(surface, cgs, catalog_entry_id, target_entity)
}

/// Non–zero-arity invoke/create/update: `e#($).m#(p#=…)` (same rules as parser dotted-call capability resolution).
#[allow(clippy::too_many_arguments)]
fn collect_multi_arity_method_lines(
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
fn collect_entity_teaching_block(
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
        let (search_line, search_gloss) = scap.map_or_else(
            || (line.clone(), sg.clone()),
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
        );
    }

    let mut field_gloss_rows = gloss_emit
        .as_mut()
        .map(|gs| std::mem::take(gs.field_gloss))
        .unwrap_or_default();
    field_gloss_rows =
        filter_field_gloss_to_referenced_symbols(&field_gloss_rows, &teaching_rows, &es);

    EntityTeachingBlock {
        heading,
        field_gloss_rows,
        teaching_rows,
    }
}

/// Keep `p#`/`v#` gloss rows referenced by teaching exemplars (and linked value domains).
fn filter_field_gloss_to_referenced_symbols(
    rows: &[TeachingFieldGloss],
    teaching_rows: &[EntityTeachingExprRow],
    entity_surface: &str,
) -> Vec<TeachingFieldGloss> {
    let mut referenced = collect_opaque_domain_symbols(entity_surface);
    for row in teaching_rows {
        referenced.extend(collect_opaque_domain_symbols(&row.teaching_expr.expression));
    }
    loop {
        let mut expanded = false;
        for g in rows {
            if !referenced.contains(g.symbol.as_str()) {
                continue;
            }
            for sym in collect_opaque_domain_symbols(&format!(
                "{} {} {}",
                g.field_type, g.allowed_values, g.description
            )) {
                if referenced.insert(sym) {
                    expanded = true;
                }
            }
        }
        if !expanded {
            break;
        }
    }
    rows.iter()
        .filter(|g| g.is_inline_union_summary || referenced.contains(g.symbol.as_str()))
        .cloned()
        .collect()
}

fn collect_opaque_domain_symbols(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if matches!(b, b'e' | b'm' | b'p' | b'r' | b'v') {
            let start = i;
            i += 1;
            if i < bytes.len() && bytes[i].is_ascii_digit() {
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if let Ok(s) = std::str::from_utf8(&bytes[start..i]) {
                    out.insert(s.to_string());
                }
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Count of synthesized teaching example lines for an entity (same pipeline as emission).
#[cfg(test)]
pub(crate) fn domain_example_line_count(cgs: &CGS, ename: &str, map: Option<&SymbolMap>) -> usize {
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let seed = prompt_line_valid_cache_seed_cgs(cgs);
    let map_arc: Option<std::sync::Arc<SymbolMap>> = map.map(|m| std::sync::Arc::new(m.clone()));
    collect_entity_teaching_block(
        cgs,
        ename,
        map_arc.as_ref(),
        None,
        false,
        &mut line_valid_cache,
        seed,
        &mut gloss_emit_none,
        None,
        None,
    )
    .teaching_rows
    .len()
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
