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
//! teaching expressions (projection witnesses last). Value domain once per `v#`, then each distinct
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
use std::collections::hash_map::Entry;
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

mod contract;
mod line_validate;
mod mcp_frontmatter;
mod mcp_prompt_fragments;
mod mcp_tool_descriptions;
mod row_producer;
mod stats;

#[cfg(test)]
mod query_teaching_tests;

use line_validate::{
    domain_line_validate_cached, domain_line_work_valid_cached, prompt_line_valid_cache_seed_cgs,
    prompt_line_valid_cache_seed_exposure, DomainLineValidCacheKey, DomainLineValidEntry,
};
use row_producer::RowProducerProjection;

pub use contract::{
    markdown_fence_body_inner, split_tsv_teaching_contract_and_table,
    teaching_tsv_agent_body_from_wrapped_prompt, teaching_tsv_from_wrapped_prompt,
    teaching_tsv_table_from_wrapped_prompt, TeachingFenceSlice, ROW_COMPUTE_EXEMPLAR_THRESHOLD,
};
pub use mcp_prompt_fragments::{
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

pub(crate) use contract::enforce_teaching_tsv_teaching_invariant;
#[cfg(test)]
pub(crate) use contract::validate_teaching_tsv_teaching_table;
pub(crate) use mcp_frontmatter::*;

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
pub struct TeachingExprLine {
    pub expression: String,
    pub result_type: String,
    /// `[scope …]` fragment when present (teaching table / capability-input legend).
    pub scope: String,
    pub optional_params: String,
    /// `args: p# wire type req; …` when the compact teaching legend includes it.
    pub compact_args: String,
    pub description: String,
    /// Projection witness row: `e#…[p#,…]` whose result gloss includes `· projection`.
    pub is_projection_teaching: bool,
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
        return TeachingExprLine {
            expression: expr,
            result_type: String::new(),
            scope: String::new(),
            optional_params: String::new(),
            compact_args: String::new(),
            description: String::new(),
            is_projection_teaching: false,
        };
    }
    let is_projection_teaching = gloss.is_some_and(|g| g.contains(PROJECTION_WITNESS_LEGEND_MARK))
        && parse_trailing_projection_bracket(expr.trim()).is_some();
    let mut row = TeachingExprLine {
        expression: expr,
        result_type: gloss.map(|s| s.to_string()).unwrap_or_default(),
        scope: String::new(),
        optional_params: String::new(),
        compact_args: String::new(),
        description: String::new(),
        is_projection_teaching,
    };
    apply_compact_legend_remainder(&mut row, cap.unwrap_or(""));
    row
}

fn compute_tsv_identity_row_index(teaching_expr_rows: &[&TeachingExprLine]) -> Option<usize> {
    teaching_expr_rows
        .iter()
        .position(|row| {
            !row.is_projection_teaching
                && tsv_identity_expr_is_entity_get(&row.expression)
                && !row.expression.contains('{')
                && !row.expression.contains('~')
                && !row.result_type.starts_with('[')
        })
        .or_else(|| {
            teaching_expr_rows.iter().position(|row| {
                !row.is_projection_teaching
                    && row.expression.contains('(')
                    && !row.expression.contains('{')
                    && !row.expression.contains('~')
                    && !row.result_type.starts_with('[')
            })
        })
        .or_else(|| {
            (teaching_expr_rows.len() == 1 && !teaching_expr_rows[0].is_projection_teaching)
                .then_some(0)
        })
}

/// Scalar projection bracket `[p#,…]` from a synthesized projection-teaching row (`TeachingExprLine`).
fn projection_bracket_from_teaching_rows(rows: &[&TeachingExprLine]) -> Option<String> {
    for row in rows {
        if !row.is_projection_teaching {
            continue;
        }
        if let Some(b) = parse_trailing_projection_bracket(row.expression.trim()) {
            return Some(b);
        }
    }
    None
}

/// Top-level union constructor teaching exemplars (`v101{p#=…}`), distinct from bare value-domain `v#` gloss symbols.
fn is_union_ctor_teaching_surface_line(expr: &str) -> bool {
    let e = expr.trim_start();
    let b = e.as_bytes();
    if b.first() != Some(&b'v') {
        return false;
    }
    let mut i = 1usize;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    i > 1 && i < b.len() && b[i] == b'{'
}

/// Numeric ordering for opaque `pN` / `rN` / `vN` tokens (`p12` before `p101`, not lexicographic).
fn opaque_pv_symbol_sort_key(sym: &str) -> Option<(u32, u32)> {
    let mut it = sym.chars();
    let prefix = it.next()?;
    if prefix != 'p' && prefix != 'v' && prefix != 'r' {
        return None;
    }
    let rest: String = it.collect();
    let n = rest.parse::<u32>().ok()?;
    Some((prefix as u32, n))
}

fn teaching_relation_field_gloss(
    map: &SymbolMap,
    r_sym: &str,
    description: &str,
) -> Option<TeachingFieldGloss> {
    let wire = map.resolve_relation_ident(r_sym)?;
    Some(TeachingFieldGloss {
        symbol: r_sym.to_string(),
        field_type: wire.to_string(),
        allowed_values: String::new(),
        description: description.to_string(),
        is_inline_union_summary: false,
    })
}

fn write_sorted_symbol_prefix_gloss_rows(
    out: &mut String,
    field_gloss_rows: &[TeachingFieldGloss],
    prefix: char,
) {
    let mut gloss: Vec<&TeachingFieldGloss> = field_gloss_rows
        .iter()
        .filter(|g| g.symbol.starts_with(prefix))
        .collect();
    gloss.sort_by(|a, b| {
        let ka = opaque_pv_symbol_sort_key(&a.symbol);
        let kb = opaque_pv_symbol_sort_key(&b.symbol);
        ka.cmp(&kb).then_with(|| a.symbol.cmp(&b.symbol))
    });
    let mut emitted = HashSet::new();
    for g in gloss {
        if emitted.insert(g.symbol.clone()) {
            write_teaching_tsv_row(out, DomainTsvRow::FieldGloss(g));
        }
    }
}

pub(crate) fn render_prompt_tsv_from_bundle(bundle: &TeachingPromptBundle) -> String {
    let mut out = String::new();
    out.push_str(TSV_TEACHING_TABLE_HEADER);
    let mut global_p_gloss_emitted: HashMap<String, String> = HashMap::new();
    let gloss_emit_fingerprint =
        |g: &TeachingFieldGloss| format!("{}|{}|{}", g.field_type, g.allowed_values, g.description);
    for block in &bundle.teaching_blocks {
        let heading = &block.heading;
        let field_gloss_rows = &block.field_gloss_rows;
        let teaching_expr_rows: Vec<&TeachingExprLine> = block
            .teaching_rows
            .iter()
            .map(|r| &r.teaching_expr)
            .collect();
        let union_ctor_row_idxs: Vec<usize> = teaching_expr_rows
            .iter()
            .enumerate()
            .filter(|(_, r)| is_union_ctor_teaching_surface_line(&r.expression))
            .map(|(i, _)| i)
            .collect();
        let union_ctor_row_set: HashSet<usize> = union_ctor_row_idxs.iter().copied().collect();
        let identity_idx = compute_tsv_identity_row_index(&teaching_expr_rows);
        let projection_first_idx = teaching_expr_rows
            .iter()
            .position(|r| r.is_projection_teaching);
        let entity_desc_attach_idx = projection_first_idx.or(identity_idx);
        // Do not read projection from the entity heading: legends may contain unrelated `[…]`
        // fragments (e.g. `[e1]` in result gloss). Teach projection only via a validated witness row
        // and/or
        // a trailing `[p#,…]` on the identity get line.
        let mut proj =
            projection_bracket_from_teaching_rows(&teaching_expr_rows).unwrap_or_default();
        if proj.is_empty() {
            if let Some(i) = identity_idx {
                if let Some(s) =
                    parse_trailing_projection_bracket(teaching_expr_rows[i].expression.trim())
                {
                    proj = s;
                }
            }
        }
        let projection_symbols = parse_projection_symbols(&proj);
        let projection_set: HashSet<&str> = projection_symbols.iter().map(|s| s.as_str()).collect();
        let mut field_gloss_by_symbol: HashMap<String, TeachingFieldGloss> = HashMap::new();
        for g in field_gloss_rows {
            field_gloss_by_symbol.insert(g.symbol.clone(), g.clone());
        }
        // Phase A: `v#` field gloss except deferred synthetic union summaries (`is_inline_union_summary`).
        let mut seen_v_phase_a: HashSet<String> = HashSet::new();
        for g in field_gloss_rows {
            if !g.symbol.starts_with('v') || g.is_inline_union_summary {
                continue;
            }
            if seen_v_phase_a.insert(g.symbol.clone()) {
                write_teaching_tsv_row(&mut out, DomainTsvRow::FieldGloss(g));
            }
        }
        // Phase B: `p#` gloss — non-projection numeric order, then projection bracket tail.
        // `p#` slots for optional query params / invokes appear in teaching gloss lines but are not part
        // of the scalar projection bracket — emit them in stable numeric `p#` order before projection fields.
        let mut p_non_projection: Vec<&TeachingFieldGloss> = field_gloss_rows
            .iter()
            .filter(|g| g.symbol.starts_with('p') && !projection_set.contains(g.symbol.as_str()))
            .collect();
        p_non_projection.sort_by(|a, b| {
            let ka = opaque_pv_symbol_sort_key(&a.symbol);
            let kb = opaque_pv_symbol_sort_key(&b.symbol);
            ka.cmp(&kb).then_with(|| a.symbol.cmp(&b.symbol))
        });
        let mut emitted_p_slot: HashSet<String> = HashSet::new();
        for g in p_non_projection {
            if !emitted_p_slot.insert(g.symbol.clone()) {
                continue;
            }
            if g.symbol.starts_with('p') {
                let fp = gloss_emit_fingerprint(g);
                if global_p_gloss_emitted
                    .get(&g.symbol)
                    .is_some_and(|prev| prev == &fp)
                {
                    continue;
                }
                global_p_gloss_emitted.insert(g.symbol.clone(), fp);
            }
            write_teaching_tsv_row(&mut out, DomainTsvRow::FieldGloss(g));
        }
        for sym in &projection_symbols {
            if emitted_p_slot.contains(sym) {
                continue;
            }
            if let Some(gloss) = field_gloss_by_symbol.get(sym.as_str()) {
                if sym.starts_with('p') {
                    let fp = gloss_emit_fingerprint(gloss);
                    if global_p_gloss_emitted
                        .get(sym.as_str())
                        .is_some_and(|prev| prev == &fp)
                    {
                        continue;
                    }
                    global_p_gloss_emitted.insert(sym.clone(), fp);
                }
                write_teaching_tsv_row(&mut out, DomainTsvRow::FieldGloss(gloss));
                emitted_p_slot.insert(sym.clone());
            }
        }

        // Phase B.5: `r#` gloss — stable numeric order (alias → wire name on the Meaning cell).
        write_sorted_symbol_prefix_gloss_rows(&mut out, field_gloss_rows, 'r');

        // Phase C: union constructor exemplars (`v101{p#=…}`) — before deferred union summary gloss.
        for &row_idx in &union_ctor_row_idxs {
            let row = teaching_expr_rows[row_idx];
            let identity_returns_row = Some(row_idx) == identity_idx;
            let attach_entity_heading = Some(row_idx) == entity_desc_attach_idx;
            write_teaching_tsv_row(
                &mut out,
                DomainTsvRow::TeachingExpr {
                    line: row,
                    identity_returns_row,
                    attach_entity_heading,
                    heading,
                },
            );
        }
        // Phase D: deferred inline union summary (`union · v101 | …`).
        for g in field_gloss_rows {
            if g.is_inline_union_summary {
                write_teaching_tsv_row(&mut out, DomainTsvRow::FieldGloss(g));
            }
        }

        // Phase E: remaining teaching expr rows (projection witnesses last).
        let mut emit_order: Vec<usize> = (0..teaching_expr_rows.len()).collect();
        emit_order.sort_by_key(|&i| {
            let is_proj = teaching_expr_rows[i].is_projection_teaching;
            (!is_proj, i)
        });
        for row_idx in emit_order {
            if union_ctor_row_set.contains(&row_idx) {
                continue;
            }
            let row = teaching_expr_rows[row_idx];
            let identity_returns_row = Some(row_idx) == identity_idx;
            let attach_entity_heading = Some(row_idx) == entity_desc_attach_idx;
            write_teaching_tsv_row(
                &mut out,
                DomainTsvRow::TeachingExpr {
                    line: row,
                    identity_returns_row,
                    attach_entity_heading,
                    heading,
                },
            );
        }
    }
    enforce_teaching_tsv_teaching_invariant(&out);
    out
}

const TSV_MEANING_JOIN: &str = " · ";

/// One logical TSV row before wire encoding ([`write_teaching_tsv_row`]).
enum DomainTsvRow<'a> {
    TeachingExpr {
        line: &'a TeachingExprLine,
        /// [`compute_tsv_identity_row_index`] — affects relation vs `returns …` gloss shaping.
        identity_returns_row: bool,
        /// Entity banner description at most once: first projection witness, else identity fallback.
        attach_entity_heading: bool,
        heading: &'a TeachingHeading,
    },
    FieldGloss(&'a TeachingFieldGloss),
}

/// Replace raw tabs inside a cell and trim edges (never used as column delimiter).
fn sanitize_tsv_cell(s: &str) -> String {
    if !s.contains('\t') {
        return s.trim().to_string();
    }
    s.replace('\t', " ").trim().to_string()
}

/// Typed fragment of a teaching-row `Meaning` cell (joined with [`TSV_MEANING_JOIN`], then sanitized as a whole).
#[derive(Clone, Debug)]
enum TeachingMeaningAtom {
    Returns { gloss: String },
    RelationNav { line: String },
    EntityHeadingDescription(String),
    LegendScope(String),
    LegendOptionalParams(String),
    LegendCompactArgs(String),
    LegendDescription(String),
}

/// True when the teaching bundle includes at least one **relation navigation** exemplar row.
#[allow(dead_code)]
pub(crate) fn teaching_bundle_has_relation_nav_exemplar(bundle: &TeachingPromptBundle) -> bool {
    bundle.teaching_blocks.iter().any(|block| {
        block
            .teaching_rows
            .iter()
            .any(|row| row.meta.kind == DomainLineKind::RelationNav)
    })
}

/// True when an emitted teaching row already demonstrates **relation navigation** on `rel_sym`
/// (receiver`.rel_sym`), not merely a scoped query filter `e#{rel_sym=…}`.
fn relation_sym_shown_in_query_teaching_rows(
    teaching_rows: &[EntityTeachingExprRow],
    rel_sym: &str,
) -> bool {
    let dotted = format!(".{rel_sym}");
    teaching_rows.iter().any(|row| {
        row.teaching_expr
            .expression
            .as_str()
            .contains(dotted.as_str())
    })
}

impl TeachingMeaningAtom {
    fn encoded_fragment(&self) -> String {
        let raw = match self {
            TeachingMeaningAtom::Returns { gloss } => format!("→ {gloss}"),
            TeachingMeaningAtom::RelationNav { line } => line.clone(),
            TeachingMeaningAtom::EntityHeadingDescription(s) => s.clone(),
            TeachingMeaningAtom::LegendScope(s) => s.clone(),
            TeachingMeaningAtom::LegendOptionalParams(s) => format!("opt: {s}"),
            TeachingMeaningAtom::LegendCompactArgs(s) => format!("args: {s}"),
            TeachingMeaningAtom::LegendDescription(s) => s.clone(),
        };
        sanitize_tsv_cell(&raw)
    }
}

/// Typed fragment of a field-gloss `Meaning` cell.
#[derive(Clone, Debug)]
enum FieldGlossMeaningAtom {
    FieldType(String),
    AllowedValues(String),
    Description(String),
}

impl FieldGlossMeaningAtom {
    fn encoded_fragment(&self) -> String {
        let raw = match self {
            FieldGlossMeaningAtom::FieldType(s) => s.clone(),
            FieldGlossMeaningAtom::AllowedValues(s) => format!("allowed: {s}"),
            FieldGlossMeaningAtom::Description(s) => s.clone(),
        };
        sanitize_tsv_cell(&raw)
    }
}

/// Sanitized `plasm_expr` column for teaching table teaching TSV (no literal tabs; trimmed).
#[derive(Clone, Debug)]
struct DomainTsvExprCell(String);

impl DomainTsvExprCell {
    fn from_plasm_expr(expr: &str) -> Self {
        Self(sanitize_tsv_cell(expr))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Sanitized `Meaning` column for teaching table teaching TSV (no literal tabs; trimmed).
#[derive(Clone, Debug)]
struct DomainTsvMeaningCell(String);

impl DomainTsvMeaningCell {
    fn from_teaching_atoms(atoms: Vec<TeachingMeaningAtom>) -> Self {
        Self(Self::join_encoded_fragments(
            atoms.into_iter().map(|a| a.encoded_fragment()),
        ))
    }

    fn from_field_gloss_atoms(atoms: Vec<FieldGlossMeaningAtom>) -> Self {
        Self(Self::join_encoded_fragments(
            atoms.into_iter().map(|a| a.encoded_fragment()),
        ))
    }

    fn join_encoded_fragments(fragments: impl Iterator<Item = String>) -> String {
        let joined = fragments
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(TSV_MEANING_JOIN);
        sanitize_tsv_cell(&joined)
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// One encoded teaching table teaching row: sanitized expr, **exactly one** U+0009, sanitized meaning, newline.
struct DomainTsvEncodedLine {
    expr: DomainTsvExprCell,
    meaning: DomainTsvMeaningCell,
}

impl DomainTsvEncodedLine {
    fn write_line(self, out: &mut String) {
        let expr_s = self.expr.as_str();
        let meaning_s = self.meaning.as_str();
        debug_assert!(
            !expr_s.contains('\t'),
            "expr cell must be tab-free before wire emit"
        );
        debug_assert!(
            !meaning_s.contains('\t'),
            "meaning cell must be tab-free before wire emit"
        );
        out.push_str(expr_s);
        out.push('\t');
        out.push_str(meaning_s);
        out.push('\n');
    }
}

fn teaching_expr_meaning_atoms(
    row: &TeachingExprLine,
    identity_returns_row: bool,
    attach_entity_heading: bool,
    heading: &TeachingHeading,
) -> Vec<TeachingMeaningAtom> {
    let mut atoms = Vec::new();
    push_teaching_meaning_result_atom(&mut atoms, row, identity_returns_row);
    if attach_entity_heading && !heading.description.is_empty() {
        atoms.push(TeachingMeaningAtom::EntityHeadingDescription(
            heading.description.clone(),
        ));
    }
    append_teaching_meaning_legend_tail_atoms(&mut atoms, row);
    atoms
}

fn field_gloss_meaning_atoms(g: &TeachingFieldGloss) -> Vec<FieldGlossMeaningAtom> {
    let mut atoms = vec![FieldGlossMeaningAtom::FieldType(g.field_type.clone())];
    if !g.allowed_values.is_empty() {
        atoms.push(FieldGlossMeaningAtom::AllowedValues(
            g.allowed_values.clone(),
        ));
    }
    if !g.description.is_empty() {
        atoms.push(FieldGlossMeaningAtom::Description(g.description.clone()));
    }
    atoms
}

fn append_teaching_meaning_legend_tail_atoms(
    atoms: &mut Vec<TeachingMeaningAtom>,
    row: &TeachingExprLine,
) {
    if !row.scope.is_empty() {
        atoms.push(TeachingMeaningAtom::LegendScope(row.scope.clone()));
    }
    if !row.optional_params.is_empty() {
        atoms.push(TeachingMeaningAtom::LegendOptionalParams(
            row.optional_params.clone(),
        ));
    }
    if !row.compact_args.is_empty() {
        atoms.push(TeachingMeaningAtom::LegendCompactArgs(
            row.compact_args.clone(),
        ));
    }
    if !row.description.is_empty() {
        atoms.push(TeachingMeaningAtom::LegendDescription(
            row.description.clone(),
        ));
    }
}

/// When `identity_row`, always prefix with `returns …` (including relation-nav identity picks).
fn push_teaching_meaning_result_atom(
    atoms: &mut Vec<TeachingMeaningAtom>,
    row: &TeachingExprLine,
    identity_row: bool,
) {
    if row.result_type.is_empty() {
        return;
    }
    if identity_row {
        atoms.push(TeachingMeaningAtom::Returns {
            gloss: row.result_type.clone(),
        });
    } else if row.result_type.starts_with("relation ") {
        atoms.push(TeachingMeaningAtom::RelationNav {
            line: row.result_type.clone(),
        });
    } else {
        atoms.push(TeachingMeaningAtom::Returns {
            gloss: row.result_type.clone(),
        });
    }
}

fn write_teaching_tsv_row(out: &mut String, row: DomainTsvRow<'_>) {
    match row {
        DomainTsvRow::TeachingExpr {
            line,
            identity_returns_row,
            attach_entity_heading,
            heading,
        } => {
            DomainTsvEncodedLine {
                expr: DomainTsvExprCell::from_plasm_expr(&line.expression),
                meaning: DomainTsvMeaningCell::from_teaching_atoms(teaching_expr_meaning_atoms(
                    line,
                    identity_returns_row,
                    attach_entity_heading,
                    heading,
                )),
            }
            .write_line(out);
        }
        DomainTsvRow::FieldGloss(g) => {
            DomainTsvEncodedLine {
                expr: DomainTsvExprCell::from_plasm_expr(&g.symbol),
                meaning: DomainTsvMeaningCell::from_field_gloss_atoms(field_gloss_meaning_atoms(g)),
            }
            .write_line(out);
        }
    }
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
            .unwrap_or(symbol.as_str())
            .to_string()
    } else {
        symbol_map
            .and_then(|m| m.resolve_ident(symbol.as_str()))
            .unwrap_or(symbol.as_str())
            .to_string()
    };
    // Leaf expand keys (e.g. `blocks`) collide with relation wire names — prefer full capability path.
    // `IdentMetaKey` is only `(catalog, entity, path)`; distinct capabilities can share the same path
    // (e.g. two different `operations` arrays). When CGS is present, resolve via the full `(cap, path)` quad.
    let meta = match (symbol_map, cgs) {
        (Some(sym_m), Some(cgs_ref)) => sym_m
            .capability_param_quad_for_p_sym(symbol.as_str())
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
            .capability_param_quad_for_p_sym(symbol.as_str())
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
                sym.value_domain_gloss_for_v_sym(vs)
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

/// True when the expression is a symbolic **entity get** `e#(…)` (unary `e#($)` / `e#(p#)`, compound, …) —
/// not `e#.m#(…)` invoke and not an anchored chain like `e#(…).m#(…)`.
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
    if let Some(p) = tail
        .strip_prefix("optional params:")
        .or_else(|| tail.strip_prefix("opt:"))
    {
        *optional_params = p.trim().to_string();
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
fn met_sym(m: Option<&SymbolMap>, catalog_entry_id: &str, entity: &str, kebab: &str) -> String {
    m.map(|x| x.method_sym_for(catalog_entry_id, entity, kebab))
        .unwrap_or_else(|| kebab.to_string())
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
        let line = TeachingExprLine {
            expression: plasm_expr,
            result_type,
            scope: String::new(),
            optional_params: String::new(),
            compact_args: String::new(),
            description: String::new(),
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
) -> (String, Option<String>) {
    let bracket = match projection {
        RowProducerProjection::BareQueryListAll => None,
        RowProducerProjection::CapabilityProvides => capability_row_projection_bracket(
            cgs,
            cap,
            map,
            catalog_entry_id,
            ename,
            surface_filter,
        ),
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
    if map.is_some_and(|m| m.resolve_ident(sym.as_str()).is_some()) {
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
        gs.emit_before_teaching_example(expr, cap_leg.as_deref(), gloss.as_deref());
    }
    let mut teaching_line =
        teaching_expr_line_from_layers(expr, gloss.as_deref(), cap_leg.as_deref());
    if omit_capability_prose {
        teaching_line.description.clear();
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

/// Structural invoke RHS inside union constructors (`v101{…}`): keyed by opaque `p#` when a
/// [`SymbolMap`] is present (teaching TSV); canonical [`RenderMode`] uses wire names.
fn format_inline_structural_example_symbolic(
    map: Option<&SymbolMap>,
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
            let mut has_optional = false;
            for sf in fields {
                if !sf.required {
                    has_optional = true;
                    break;
                }
            }
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
                        let rhs = format_inline_structural_example_symbolic(
                            map,
                            domain,
                            cap_name,
                            &seg,
                            inner.as_ref(),
                            _cgs,
                        );
                        let lhs = map
                            .map(|m| m.ident_sym_cap_param(domain, cap_name, &seg))
                            .unwrap_or_else(|| sf.name.clone());
                        parts.push(format!("{lhs}={rhs}"));
                    }
                    crate::InputFieldWire::Registry(_) => {
                        let lhs = map
                            .map(|m| m.ident_sym_cap_param(domain, cap_name, &seg))
                            .unwrap_or_else(|| sf.name.clone());
                        parts.push(format!("{lhs}={}", TEACHING_PARAM_VALUE_PLACEHOLDER));
                    }
                }
            }
            let inner = parts.join(",");
            let body = if has_optional {
                if inner.is_empty() {
                    "..".to_string()
                } else {
                    format!("{inner},..")
                }
            } else {
                inner
            };
            format!("{{{body}}}")
        }
        crate::InputType::Array { element_type, .. } => {
            format!(
                "[{}]",
                format_inline_structural_example_symbolic(
                    map,
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
    domain: &str,
    cap_name: &str,
    path_prefix: &str,
    ty: &crate::InputType,
    cgs: &CGS,
) -> String {
    let crate::InputType::Object { fields, .. } = ty else {
        return format_inline_structural_example_symbolic(
            map,
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
                    domain,
                    cap_name,
                    &seg,
                    inner.as_ref(),
                    cgs,
                );
                let lhs = map
                    .map(|m| m.ident_sym_cap_param(domain, cap_name, &seg))
                    .unwrap_or_else(|| sf.name.clone());
                parts.push(format!("{lhs}={rhs}"));
            }
            crate::InputFieldWire::Registry(_) => {
                let lhs = map
                    .map(|m| m.ident_sym_cap_param(domain, cap_name, &seg))
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
        format_inline_structural_example_symbolic(map, domain, cap_name, &prefix, &body_ty, cgs)
    ))
}

/// Root-level invoke union (`input_schema.type: union`): ctor body uses flat param paths (`p5`, …).
fn format_root_union_constructor_invoke_example(
    variant: &crate::schema::InputVariantSchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    domain: &str,
    cap_name: &str,
) -> Option<String> {
    let ctor = crate::schema::union_variant_constructor_symbol(variant)?;
    let body_ty = crate::schema::input_variant_body_type(variant);
    Some(format!(
        "{}{}",
        ctor,
        format_inline_structural_example_symbolic_required_only(
            map, domain, cap_name, "", &body_ty, cgs
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
        if let Some(vsym) = gs.map.value_domain_fp_to_sym.get(&fp) {
            if let Some(vg) = gs.map.value_domain_gloss_for_v_sym(vsym) {
                let Some(v_canon) = meaning_canonical_sym_for_emit(
                    vg,
                    vsym.as_str(),
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
            anchor_entity,
            cap.name.as_str(),
        );
    }
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
        if field_omitted_from_path_inject(ent, cap, f.name.as_str()) {
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
        if !f.required {
            continue;
        }
        match invoke_dotted_call_arg_example(f, cap, cgs, map, catalog_entry_id) {
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
    // Path-bound scope slots may be fully injected from a compound receiver (`Entity(k1=$,k2=$)`),
    // leaving only `method()` for zero-body deletes / similar invokes.
    if parts.is_empty() {
        return Some(String::new());
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
    let mut has_optional = false;
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
            parts.push(scope_param_slot(f, cap, cgs, map, catalog_entry_id));
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
    let label = capability_method_label_kebab(cap);
    let ms = met_sym(map, catalog_entry_id, cap.domain.as_str(), &label);
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
        let label = capability_method_label_kebab(cap);
        let ms = met_sym(map, catalog_entry_id, ename, &label);
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
        gs.emit_before_teaching_example(&es, ent_desc_short.as_deref(), None);
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
    // narrow `[p#,…]` appears first in teaching table.
    if let Some(bracket) = primary_get_projection_bracket
        .as_deref()
        .filter(|b| !b.trim().is_empty())
    {
        let _ = try_push_projection_witness_row(
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
        );
    }

    let mut seen_singleton_cap: HashSet<String> = HashSet::new();
    for cap in &singleton_get_caps {
        if !seen_singleton_cap.insert(cap.name.to_string()) {
            continue;
        }
        let label = capability_method_label_kebab(cap);
        let ms = met_sym(map, catalog_entry_id, ename, &label);
        let expr = format!("{es}.{ms}()");
        let result_gloss = crate::result_gloss::result_gloss_for_capability(cap, cgs, map);
        let cap_leg =
            capability_legend_for_domain(map, cgs, cap, ename, ident_meta, catalog_entry_id);
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
            let label = capability_method_label_kebab(cap);
            let ms = met_sym(map, catalog_entry_id, ename, &label);
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
            let cap_leg =
                capability_legend_for_domain(map, cgs, cap, ename, ident_meta, catalog_entry_id);
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
                line_valid_cache,
                line_valid_cache_seed,
                map_arc,
            );
        }
        let cap_leg = cap_ref.and_then(|c| {
            capability_legend_for_domain(map, cgs, c, ename, ident_meta, catalog_entry_id)
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
            let cap_leg =
                capability_legend_for_domain(map, cgs, cap, ename, ident_meta, catalog_entry_id);
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
            capability_legend_for_domain(map, cgs, cap, ename, ident_meta, catalog_entry_id)
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

#[cfg(test)]
mod lazy_field_gloss_tests {
    use super::*;

    #[test]
    fn collect_opaque_domain_symbols_finds_em_pv_tokens() {
        let syms = collect_opaque_domain_symbols("e1{p14=e3(p5=$), p71=open}[p1,p2]");
        assert!(syms.contains("e1"));
        assert!(syms.contains("e3"));
        assert!(syms.contains("p14"));
        assert!(syms.contains("p5"));
        assert!(syms.contains("p71"));
        assert!(syms.contains("p1"));
        assert!(syms.contains("p2"));
        let rel = collect_opaque_domain_symbols("e5(p7=$).r8");
        assert!(rel.contains("r8"));
    }
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

/// Raw teaching lines for an entity (for per-capability witness checks).
#[cfg(test)]
pub(crate) fn domain_example_lines(
    cgs: &CGS,
    ename: &str,
    map: Option<&SymbolMap>,
    surface_filter: Option<&ExposureSurface>,
) -> Vec<String> {
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
        surface_filter,
        None,
    )
    .teaching_rows
    .into_iter()
    .map(|r| r.teaching_expr.expression.clone())
    .collect()
}

/// Count canonical `· projection ·` witness rows (one per entity; query/search may reuse the bracket suffix).
#[cfg(test)]
fn count_projection_teaching_witness_rows(
    cgs: &CGS,
    ename: &str,
    map: Option<&SymbolMap>,
    surface_filter: Option<&ExposureSurface>,
) -> usize {
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
        surface_filter,
        None,
    )
    .teaching_rows
    .iter()
    .filter(|r| r.teaching_expr.is_projection_teaching)
    .count()
}

/// Primary-get projection bracket for the teaching table entity heading (when enabled); test-only helper.
#[cfg(test)]
#[allow(dead_code)] // Retained for debugging / synthesis parity checks; tests prefer [`domain_projection_bracket_from_final_bundle`].
fn domain_heading_projection_bracket(
    cgs: &CGS,
    ename: &str,
    map: Option<&SymbolMap>,
    surface_filter: Option<&ExposureSurface>,
) -> Option<String> {
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let seed = prompt_line_valid_cache_seed_cgs(cgs);
    let map_arc: Option<std::sync::Arc<SymbolMap>> = map.map(|m| std::sync::Arc::new(m.clone()));
    let block = collect_entity_teaching_block(
        cgs,
        ename,
        map_arc.as_ref(),
        None,
        false,
        &mut line_valid_cache,
        seed,
        &mut gloss_emit_none,
        surface_filter,
        None,
    );
    let refs: Vec<&TeachingExprLine> = block
        .teaching_rows
        .iter()
        .map(|r| &r.teaching_expr)
        .collect();
    projection_bracket_from_teaching_rows(&refs)
}

/// Full scalar projection list `[p#,…]` from the projection teaching row or a legacy get suffix.
#[cfg(test)]
#[allow(dead_code)] // Superseded by [`domain_projection_bracket_from_final_bundle`] for prompt-aligned assertions.
fn domain_projection_bracket_exemplar(
    cgs: &CGS,
    ename: &str,
    map: Option<&SymbolMap>,
    surface_filter: Option<&ExposureSurface>,
) -> Option<String> {
    if let Some(b) = domain_heading_projection_bracket(cgs, ename, map, surface_filter) {
        return Some(b);
    }
    for line in domain_example_lines(cgs, ename, map, surface_filter) {
        if let Some(b) = parse_trailing_projection_bracket(line.trim()) {
            return Some(b);
        }
    }
    None
}

/// [`domain_projection_bracket_exemplar`] reads pre–post-pass teaching synthesis; this uses the same
/// [`render_teaching_prompt_bundle_for_exposure`] path as production prompts (opaque alias rewrite applied).
#[cfg(test)]
fn domain_projection_bracket_from_final_bundle(
    cgs: &CGS,
    exposure: &crate::symbol_tuning::TeachingExposureSession,
    config: RenderConfig<'_>,
    ename: &str,
) -> Option<String> {
    let bundle = render_teaching_prompt_bundle_for_exposure(cgs, config, exposure, None);
    let refs: Vec<&str> = exposure.entities.iter().map(|s| s.as_str()).collect();
    let focus = crate::symbol_tuning::FocusSpec::SeedsExact(&refs);
    let (full_entities, _) = crate::symbol_tuning::entity_slices_for_render(cgs, focus);
    let idx = full_entities.iter().position(|e| *e == ename)?;
    let block = bundle.teaching_blocks.get(idx)?;
    let lines: Vec<&TeachingExprLine> = block
        .teaching_rows
        .iter()
        .map(|r| &r.teaching_expr)
        .collect();
    projection_bracket_from_teaching_rows(&lines)
}

/// Turn a teaching scope variant into the **same shape as a path expression**: bare `e#` when unscoped,
/// else `e#{p#=e#(id),…}` with `*` stripped from scope hints (teaching-table-only marker).
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

#[cfg(test)]
fn is_field_gloss_line(trimmed: &str) -> bool {
    let t = trimmed.trim_start();
    let rest = if let Some(r) = t.strip_prefix('p') {
        r
    } else if let Some(r) = t.strip_prefix('v') {
        r
    } else if let Some(r) = t.strip_prefix('r') {
        r
    } else {
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

/// Extract expression strings from the rendered teaching section: **tsv** uses the `plasm_expr` column
/// after the `plasm_expr\tMeaning` header.
#[cfg(test)]
fn example_expressions_from_prompt(prompt: &str) -> Vec<String> {
    if prompt.contains(TSV_TEACHING_TABLE_HEADER) {
        return example_expressions_from_prompt_tsv(prompt);
    }
    let mut out = Vec::new();
    let mut in_domain = false;
    for line in prompt.lines() {
        if line.contains(TEACHING_VALID_EXPR_MARKER) {
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
    let rest = if let Some(r) = s.strip_prefix('p') {
        r
    } else if let Some(r) = s.strip_prefix('v') {
        r
    } else if let Some(r) = s.strip_prefix('r') {
        r
    } else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
fn example_expressions_from_prompt_tsv(prompt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_table = false;
    for line in prompt.lines() {
        if line == TSV_TEACHING_TABLE_HEADER.trim_end() {
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
        CapabilityMapping, CapabilitySchema, FieldSchema, FieldValueKind, NamedValueSchema,
        RelationSchema, ResourceSchema, ValueDomainKey,
    };
    use crate::symbol_tuning::{
        entity_slices_for_render, resolve_prompt_surface_entities, symbol_map_for_prompt,
        ExposureEntityKey, FocusSpec, TeachingExposureSession,
    };
    use crate::CapabilityKind;
    use crate::Cardinality;
    use crate::EntityName;
    use crate::FieldType;
    use crate::CGS;
    use line_validate::validate_teaching_line_wire;

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

    /// Locks Proof `Document`-focused symbolic teaching TSV (`apis/proof`): union ctor teaching rows,
    /// value-domain gloss, and `document_edit_v2` witness line. Update with
    /// `INSTA_UPDATE=1 cargo test -p plasm-core proof_document_teaching_tsv_snapshot`.
    #[test]
    fn proof_document_teaching_tsv_snapshot() {
        let dir = apis_dir("proof");
        if !dir.is_dir() {
            eprintln!(
                "skip: apis/proof not at {} (incomplete plasm-oss tree?)",
                dir.display()
            );
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(Some("Document")));
        with_insta_snapshots(|| {
            insta::assert_snapshot!("proof_document_teaching_tsv", tsv);
        });
    }

    #[test]
    fn proof_document_blocks_operation_params_are_not_relation_nav_gloss() {
        let dir = apis_dir("proof");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(Some("Document")));
        for sym in ["p1", "p2", "p4"] {
            let needle = format!("{sym}\t=> Block ·");
            assert!(
                !tsv.contains(&needle),
                "capability `blocks` ctor params must not reuse relation-style `→ Block` gloss (symbol {sym}); relation nav stays on `e1($).r6`-style rows.\n{tsv}"
            );
        }
    }

    #[test]
    fn proof_bug_report_capabilities_require_report_parameter() {
        let dir = apis_dir("proof");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        for cap_name in ["bug_report_submit", "document_bug_report_submit"] {
            let cap = cgs
                .get_capability(cap_name)
                .unwrap_or_else(|| panic!("missing capability {cap_name}"));
            assert!(
                cap.has_any_required_param(),
                "{cap_name}: expected at least one required parameter so teaching table cannot teach a no-arg bug report"
            );
            let fields = cap.object_params().unwrap_or_else(|| {
                panic!("{cap_name}: expected merged object input schema from parameters:")
            });
            let report = fields
                .iter()
                .find(|f| f.name == "report")
                .unwrap_or_else(|| panic!("{cap_name}: missing `report` parameter"));
            assert!(report.required, "{cap_name}: `report` must be required");
        }
    }

    #[test]
    fn proof_document_tsv_topo_p_gloss_before_union_ctor_and_summary_after() {
        let dir = apis_dir("proof");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(Some("Document")));
        fn expr_cell(line: &str) -> Option<&str> {
            line.split_once('\t').map(|(e, _)| e)
        }
        fn meaning_cell(line: &str) -> Option<&str> {
            line.split_once('\t').map(|(_, m)| m)
        }
        let mut ctor_idxs = Vec::new();
        let mut union_summary_idx = None;
        let mut p7_idx = None;
        for (i, line) in tsv.lines().enumerate() {
            if line == "plasm_expr\tMeaning" {
                continue;
            }
            let Some(expr) = expr_cell(line) else {
                continue;
            };
            if expr == "p7" {
                p7_idx = Some(i);
            }
            if is_union_ctor_teaching_surface_line(expr) {
                ctor_idxs.push(i);
            }
            if meaning_cell(line).is_some_and(|m| m.trim_start().starts_with("union ·")) {
                union_summary_idx = Some(i);
            }
        }
        let Some(u) = union_summary_idx else {
            panic!("expected union summary row with Meaning starting `union ·`");
        };
        let Some(p7) = p7_idx else {
            panic!("expected p7 gloss row");
        };
        assert!(!ctor_idxs.is_empty(), "expected union ctor exemplar rows");
        let first_ctor = *ctor_idxs.iter().min().expect("ctor rows");
        assert!(
            p7 < first_ctor,
            "p7 gloss must precede union ctor exemplars; p7={p7} first_ctor={first_ctor}"
        );
        for &c in &ctor_idxs {
            assert!(c < u, "union ctor at {c} must precede union summary at {u}");
        }
    }

    fn fixtures_schemas_dir(name: &str) -> std::path::PathBuf {
        repo_path(&["..", "..", "fixtures", "schemas", name])
    }

    /// Upper bound for [`prompt_matrix_full_tsv_synthesis_benchmark`] (best-of-three wall time after warmup).
    ///
    /// Override for slow CI hosts or local profiling: `PLASM_PROMPT_MATRIX_SYNTH_MAX_MS` (milliseconds).
    fn prompt_matrix_synthesis_time_limit() -> std::time::Duration {
        const DEFAULT_MS: u64 = 3000;
        match std::env::var("PLASM_PROMPT_MATRIX_SYNTH_MAX_MS") {
            Ok(s) => s
                .parse::<u64>()
                .map(std::time::Duration::from_millis)
                .unwrap_or_else(|_| std::time::Duration::from_millis(DEFAULT_MS)),
            Err(_) => std::time::Duration::from_millis(DEFAULT_MS),
        }
    }

    /// Insta resolves the default `snapshots/` path from `file!()`. In the parent
    /// `plasm/` virtual workspace, path remaps can make that resolve under a spurious
    /// `plasm-oss/plasm-oss/...` tree, so the committed `.snap` is not found. Anchor to
    /// [`CARGO_MANIFEST_DIR`], which is always the `plasm-core` crate root.
    ///
    /// Serialize snapshot reads/writes: parallel `cargo test` threads share Insta's global settings and
    /// can otherwise flake snapshot comparisons.
    fn with_insta_snapshots<R>(f: impl FnOnce() -> R) -> R {
        static INSTA_SNAPSHOT_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = INSTA_SNAPSHOT_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_path(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/snapshots"),
        );
        settings.bind(f)
    }

    #[test]
    fn plasm_language_contract_is_tsv_first_and_avoids_legacy_terms() {
        let contract = super::PLASM_TOOL_DESCRIPTION;
        assert!(
            contract.contains("TSV table semantics:"),
            "contract should teach TSV interpretation before catalog rows"
        );
        assert!(
            contract.contains("Replace teaching placeholders")
                || contract.contains("substitute placeholders"),
            "symbolic contract must teach placeholder substitution"
        );
        assert!(
            contract.contains("run_ref"),
            "MCP contract must teach paging via run_ref on plasm_run"
        );
        assert!(
            !contract.contains("page_handle"),
            "contract must not advertise removed page_handle param"
        );
        assert!(
            !contract.contains("plan_commit_ref"),
            "plasm tool contract must not advertise plan_commit_ref as plasm_run param"
        );
        assert!(
            !contract.contains("program continuations"),
            "MCP contract must not advertise legacy program continuations on plasm_run"
        );
        assert!(
            !contract.contains("teaching table") && !contract.contains(";;") && !contract.contains("p#=v"),
            "contract must not reintroduce legacy teaching table/compact separators or bare-v placeholders:\n{contract}"
        );
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
                let n = domain_example_line_count(&cgs, ename, map.as_deref());
                assert!(
                    n > 0,
                    "{}: entity `{ename}` is in full_entities but collect_entity_teaching_block emitted no teaching rows",
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
        let lines = domain_example_lines(&cgs, "ValueRange", None, None);
        let expected = "ValueRange(spreadsheetId=Spreadsheet($), range=$)";
        assert!(
            lines.iter().any(|l| l.starts_with(expected)),
            "missing compound dotted-call-safe get witness for entity_ref key var: expected prefix `{expected}` in {:?}",
            lines
        );
        assert!(
            validate_teaching_line_wire(&cgs, expected).is_some(),
            "expected synthesized compound get witness to parse+typecheck: `{expected}`"
        );
    }

    /// Regression: Issue teaching table teaches **one** canonical `· projection ·` witness row.
    /// Scoped query/search exemplars may **reuse** the same trailing `[p#,…]` suffix.
    #[test]
    fn github_issue_domain_emits_single_full_projection_exemplar() {
        let dir = apis_dir("github");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let exposure =
            crate::symbol_tuning::teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
        let surface = Some(&exposure.surface);
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
        let cfg = RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact);
        let br = domain_projection_bracket_from_final_bundle(&cgs, &exposure, cfg, "Issue").expect(
            "Issue should carry a full projection bracket (heading or primary get) after alias pass",
        );
        assert!(
            br.starts_with('[') && br.contains('p'),
            "unexpected projection bracket: {br}"
        );
        let lines = domain_example_lines(&cgs, "Issue", map.as_deref(), surface);
        assert_eq!(
            count_projection_teaching_witness_rows(&cgs, "Issue", map.as_deref(), surface),
            1,
            "expect exactly one `· projection ·` witness row per entity"
        );
        let trailing_projection_rows = lines
            .iter()
            .filter(|l| parse_trailing_projection_bracket(l.trim()).is_some())
            .count();
        assert!(
            trailing_projection_rows > 1,
            "scoped query/search exemplars reuse the canonical projection suffix (got {trailing_projection_rows} trailing bracket rows)"
        );
        let out = render_prompt_with_config(&cgs, cfg);
        assert!(
            !out.contains("Federated sessions"),
            "single-catalog github slice teaching TSV should not embed grammar pitfalls"
        );
        assert!(
            out.contains(br.as_str()),
            "full prompt should include the full projection list `{br}` (heading or primary get)"
        );
        assert!(
            out.len() > 8_000,
            "full apis/github teaching table+legend should be substantial (got {}); compare `github_api_full_prompt_symbolic` snapshot",
            out.len()
        );
        // Baseline bumped after wait/cancel continuation contract + projection suffix on scoped query/search exemplars.
        const GITHUB_FULL_PROMPT_BASELINE_V0173: usize = 32_000;
        const GITHUB_FULL_PROMPT_BASELINE_V0179: usize = 32_000;
        assert!(
            out.len() <= GITHUB_FULL_PROMPT_BASELINE_V0179,
            "github full prompt regressed above v0.1.79 baseline (got {} bytes, baseline {})",
            out.len(),
            GITHUB_FULL_PROMPT_BASELINE_V0179
        );
        assert!(
            out.len() < GITHUB_FULL_PROMPT_BASELINE_V0173,
            "github full prompt should remain smaller than v0.1.73 baseline (got {} bytes, baseline {})",
            out.len(),
            GITHUB_FULL_PROMPT_BASELINE_V0173
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
        let exposure =
            crate::symbol_tuning::teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
        let surface = Some(&exposure.surface);
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true);
        let cfg = RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact);
        let br = domain_projection_bracket_from_final_bundle(&cgs, &exposure, cfg, "Issue")
            .expect("Linear Issue should carry a full projection bracket (heading or primary get)");
        assert!(
            br.starts_with('[') && br.contains('p'),
            "unexpected projection bracket: {br}"
        );
        let lines = domain_example_lines(&cgs, "Issue", map.as_deref(), surface);
        assert_eq!(
            count_projection_teaching_witness_rows(&cgs, "Issue", map.as_deref(), surface),
            1,
            "expect exactly one `· projection ·` witness row per entity"
        );
        let trailing_projection_rows = lines
            .iter()
            .filter(|l| parse_trailing_projection_bracket(l.trim()).is_some())
            .count();
        assert!(
            trailing_projection_rows > 1,
            "scoped exemplars reuse projection suffix on Linear Issue (got {trailing_projection_rows})"
        );
        let out = render_prompt_with_config(&cgs, cfg);
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
        let exposure =
            crate::symbol_tuning::teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
        let cfg = RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact);
        let br = domain_projection_bracket_from_final_bundle(&cgs, &exposure, cfg, "Issue")
            .expect("Issue should carry a projection list");
        let out = render_prompt_with_config(&cgs, cfg);
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
        let mut exp = TeachingExposureSession::new(&cgs, "", &["Pet"]);
        let first = pipeline.render_teaching_first_wave_for_session(&cgs, &exp, None);
        assert!(
            !first.contains(TEACHING_VALID_EXPR_MARKER),
            "execute first wave must not repeat global grammar contract"
        );
        assert!(
            first.starts_with(TSV_TEACHING_TABLE_HEADER),
            "first wave should start with teaching table header"
        );
        let (c, table) = split_tsv_teaching_contract_and_table(&first);
        assert!(c.is_none(), "execute first wave has no contract prefix");
        assert!(
            table.starts_with(TSV_TEACHING_TABLE_HEADER),
            "table body should start with plasm_expr/Meaning"
        );
        let eval_prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        assert!(
            !eval_prompt.contains(TEACHING_VALID_EXPR_MARKER),
            "teaching TSV is table-only; grammar lives in PLASM_TOOL_DESCRIPTION"
        );
        assert!(
            super::PLASM_TOOL_DESCRIPTION.contains(TEACHING_VALID_EXPR_MARKER),
            "canonical grammar const must include contract marker"
        );
        exp.expose_entities(&[&cgs], std::sync::Arc::new(cgs.clone()), "", &["Order"]);
        let delta = pipeline.render_teaching_exposure_delta(&cgs, &exp, &["Order"], None);
        assert!(
            !delta.contains(TEACHING_VALID_EXPR_MARKER),
            "additive TSV must not repeat global contract comments"
        );
        assert!(
            delta.contains(TSV_TEACHING_TABLE_HEADER.trim_end()),
            "additive TSV should keep column header"
        );
    }

    #[test]
    fn expand_wave_emits_parent_relation_edge_for_pokeapi_berry_firmness() {
        use crate::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};
        use crate::symbol_tuning::ExposureEntityKey;

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let pipeline = PromptPipelineConfig::default();
        let intent = "cheri berry firmness";
        let relation_keys = vec![ExposureEntityKey {
            entry_id: "pokeapi".to_string(),
            entity: crate::EntityName::from("Berry"),
        }];
        let delta1 = derive_intent_exposure_surface_batch(
            &cgs,
            "pokeapi",
            intent,
            &relation_keys,
            &["Berry".to_string()],
            None,
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        let mut exp =
            TeachingExposureSession::new_with_intent_delta(&cgs, "pokeapi", &["Berry"], delta1);
        let slots_before = exp.surface.slots.clone();
        let cgs_arc = std::sync::Arc::new(cgs.clone());
        let relation_keys_wave2 =
            exp.relation_endpoint_keys_for_wave("pokeapi", &["BerryFirmness".to_string()]);
        let delta2 = derive_intent_exposure_surface_batch(
            &cgs,
            "pokeapi",
            intent,
            &relation_keys_wave2,
            &["BerryFirmness".to_string()],
            None,
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        exp.expose_surface(&[&cgs], cgs_arc, "pokeapi", &["BerryFirmness"], delta2);
        let added = exp.qualified_entities_since(1);
        let new_relation_slots = exp.relation_edge_delta_slots(&slots_before, &added);
        exp.admit_relation_edge_slots_for_render(&[&cgs], &new_relation_slots);
        assert!(
            new_relation_slots.iter().any(|slot| {
                matches!(
                    slot,
                    ExposureSlotKey::Relation {
                        source,
                        relation,
                    } if source.entity.as_str() == "Berry" && relation.as_str() == "firmness"
                )
            }),
            "expand should add Berry.firmness relation slot"
        );
        let delta = pipeline.render_teaching_exposure_delta_with_edges(
            &cgs,
            &exp,
            &["BerryFirmness"],
            &new_relation_slots,
            None,
        );
        assert!(
            delta.contains("relation e1 → e2"),
            "delta should teach parent hop: {delta}"
        );
        assert!(
            delta.contains(".r"),
            "delta should include opaque relation symbol: {delta}"
        );
    }

    #[test]
    fn split_tsv_teaching_contract_and_table_table_only() {
        let t = "plasm_expr\tMeaning\na\tb\n";
        let (c, b) = split_tsv_teaching_contract_and_table(t);
        assert_eq!(c, None);
        assert_eq!(b, t);
    }

    #[test]
    fn split_tsv_teaching_contract_and_table_with_comment_prefix() {
        let t = "# Plasm contract line\n# second\n\nplasm_expr\tMeaning\na\tb\n";
        let (c, b) = split_tsv_teaching_contract_and_table(t);
        assert_eq!(c.as_deref(), Some("# Plasm contract line\n# second"));
        assert_eq!(b, "plasm_expr\tMeaning\na\tb\n");
    }

    #[test]
    fn rendered_teaching_tsv_teaching_rows_single_tab_separator() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let (_, body) = split_tsv_teaching_contract_and_table(&tsv);
        validate_teaching_tsv_teaching_table(&body)
            .expect("every teaching row must be expr\\tMeaning");
    }

    /// Regression guard: full symbolic TSV prompt synthesis for [`fixtures/schemas/plasm_prompt_matrix`]
    /// must stay within a fixed wall-time budget (best of three timed runs after warmup).
    ///
    /// Calibrated for **small** matrix fixtures (~tens of ms on a laptop); failures usually mean
    /// accidental quadratic work or extra clones on the prompt path. Relax only with cause:
    /// `PLASM_PROMPT_MATRIX_SYNTH_MAX_MS`.
    #[test]
    fn prompt_matrix_full_tsv_synthesis_benchmark() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let config = RenderConfig::for_eval(None);

        let warmup = render_prompt_tsv_with_config(&cgs, config);
        assert!(
            warmup.contains(TSV_TEACHING_TABLE_HEADER.trim_end()),
            "warmup must emit teaching TSV header"
        );

        let mut best = std::time::Duration::MAX;
        for _ in 0..3 {
            let t0 = Instant::now();
            let tsv = render_prompt_tsv_with_config(&cgs, config);
            best = best.min(t0.elapsed());
            assert!(
                tsv.len() > 2000,
                "sanity: symbolic prompt should be substantial (got {} chars)",
                tsv.len()
            );
        }

        let limit = prompt_matrix_synthesis_time_limit();
        assert!(
            best <= limit,
            "plasm_prompt_matrix TSV synthesis too slow: best-of-3 {:?} > limit {:?}. \
             Set PLASM_PROMPT_MATRIX_SYNTH_MAX_MS to raise the cap (milliseconds).",
            best,
            limit
        );
    }

    /// Regression: TSV `p#` gloss rows must use [`IdentMetadata`] for the entity owning the teaching table
    /// block, not `full_entities[idx]` by YAML insertion order (symbolic bundle uses sorted
    /// [`TeachingExposureSession::entities`]). Overshow has `RecordedContent.id` (string) and
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
            .split(TSV_TEACHING_TABLE_HEADER)
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
        let id_typing_on_v = first_block.lines().any(|l| {
            let mut cols = l.split('\t');
            let Some(sym) = cols.next() else {
                return false;
            };
            let Some(meaning) = cols.next() else {
                return false;
            };
            sym.starts_with('v') && meaning.contains("int")
        });
        let id_slot_teaches_v = first_block.lines().any(|l| {
            let mut cols = l.split('\t');
            let Some(sym) = cols.next() else {
                return false;
            };
            let Some(meaning) = cols.next() else {
                return false;
            };
            sym.starts_with('p')
                && meaning.starts_with('v')
                && meaning.contains("id")
                && meaning.contains(" · id")
        });
        assert!(
            id_typing_on_v && id_slot_teaches_v,
            "CaptureItem `id` should type on a v# row (`int`) and the p# row should teach `v# · id`; first block:\n{first_block}"
        );
    }

    /// `Profile.recorded_matches` targets `RecordedContent`, which has Search/Query but no Get — teaching table
    /// must still teach chain nav for `query_scoped` many relations using a **validated** receiver
    /// (query-scoped `e7{…}` preferred over bare `e7($)` when that is the anchor that type-checks).
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
                .any(|l| { l.contains("Content scoped to this profile") && l.starts_with('r') })
                && tsv.lines().any(|l| {
                    l.contains("e7")
                        && (l.contains(".r") || l.contains(".recorded_matches"))
                        && l.contains("relation e7")
                }),
            "expected Profile → RecordedContent r# gloss and relation nav; e7 lines:\n{}",
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
                    && line.contains("→ e1")
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
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let mut lines = tsv.lines();
        let first = lines.next().expect("tsv header");
        assert_eq!(
            first,
            TSV_TEACHING_TABLE_HEADER.trim_end(),
            "TSV output should begin with plasm_expr/Meaning header (grammar is static in PLASM_TOOL_DESCRIPTION)"
        );
        assert!(
            !tsv.contains(TEACHING_VALID_EXPR_MARKER),
            "teaching TSV must not embed grammar contract"
        );
        assert!(
            super::PLASM_TOOL_DESCRIPTION.contains(TEACHING_VALID_EXPR_MARKER),
            "canonical grammar const must include contract marker"
        );
        let issue_identity = tsv
            .lines()
            .find(|l| {
                let cols: Vec<&str> = l.split('\t').collect();
                cols.len() == 2
                    && cols[0].starts_with("e5(")
                    && !cols[0].contains('[')
                    && cols[1].starts_with("→ e5")
            })
            .expect("Issue compound identity get row");
        let cols: Vec<&str> = issue_identity.split('\t').collect();
        assert_eq!(cols.len(), 2, "identity row should have 2 columns");
        assert!(cols[0].starts_with("e5("));
        assert!(
            !cols[0].contains('['),
            "Issue identity get should not fuse a projection bracket; row={issue_identity:?}"
        );
        let issue_projection_row = tsv.lines().find(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            if c.len() != 2 {
                return false;
            }
            let expr = c[0].trim();
            expr.starts_with("e5")
                && c[1].contains("· projection")
                && parse_trailing_projection_bracket(expr).is_some()
        });
        let issue_projection_row =
            issue_projection_row.expect("expected Issue projection witness TSV row");
        assert!(
            issue_projection_row.contains("GitHub issue"),
            "projection witness Meaning should carry Issue entity prose once: {issue_projection_row:?}"
        );
        assert!(
            !cols[1].contains("GitHub issue"),
            "identity get Meaning should not repeat entity banner prose; row={issue_identity:?}"
        );
        let state_slot = tsv
            .lines()
            .find(|l| {
                let cols: Vec<&str> = l.split('\t').collect();
                cols.len() == 2
                    && cols[0].starts_with('p')
                    && cols[1].contains("state")
                    && cols[1].contains('v')
            })
            .expect("Issue state field TSV row (compact `v# · state` when select shares values:)");
        let state_cols: Vec<&str> = state_slot.split('\t').collect();
        assert_eq!(state_cols.len(), 2);
        assert!(
            state_cols[1].starts_with('v') && state_cols[1].contains(" · state"),
            "expected `v# · wire` Meaning for enum-backed state slot; got {:?}",
            state_cols[1]
        );
        assert!(
            tsv.lines().any(|l| {
                let c: Vec<&str> = l.split('\t').collect();
                c.len() == 2
                    && c[0].starts_with('v')
                    && c[1].contains("open")
                    && c[1].contains("closed")
            }),
            "expected a v# row carrying Issue state allowed values; excerpt missing open/closed"
        );
        let body = tsv
            .lines()
            .skip_while(|line| *line != TSV_TEACHING_TABLE_HEADER.trim_end())
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !body.contains(";;"),
            "2-column TSV surface should remove compact `;;` gloss separators"
        );
        let p_owner = map.ident_sym_cap_param("Issue", "issue_sub_issue_query", "owner");
        let owner_row = tsv
            .lines()
            .find(|l| l.starts_with(&format!("{p_owner}\t")))
            .unwrap_or_else(|| {
                panic!("expected TSV gloss row for {p_owner} (Issue.issue_sub_issue_query.owner)")
            });
        let oc: Vec<&str> = owner_row.split('\t').collect();
        assert_eq!(oc.len(), 2, "owner slot row should be 2-column TSV");
        assert!(
            oc[1].starts_with('v') && oc[1].contains(" · "),
            "capability-param registry gloss should use `v# · wire` (and optional prose); got {:?}",
            oc[1]
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
            .expect("IssueComment invoke teaching table row");
        assert!(
            issue_comment_create_row.contains("[scope")
                || issue_comment_create_row.contains("scope"),
            "invoke row should reference scoping, got {issue_comment_create_row:?}"
        );
        let contrib_ent = map.entity_sym("Contributor");
        let p_repo = map.ident_sym_cap_param("Contributor", "contributor_query", "repository");
        let p_anon = map.ident_sym_cap_param("Contributor", "contributor_query", "anon");
        let contrib = tsv
            .lines()
            .find(|l| {
                let cols: Vec<&str> = l.split('\t').collect();
                cols.len() == 2
                    && cols[0].starts_with(&format!("{contrib_ent}{{"))
                    && cols[0].contains(&format!("{p_repo}="))
                    && cols[0].contains(&format!("{p_anon}="))
                    && cols[1].contains("inputs:")
            })
            .or_else(|| {
                tsv.lines().find(|l| {
                    let cols: Vec<&str> = l.split('\t').collect();
                    cols.len() == 2
                        && cols[0].starts_with(&format!("{contrib_ent}{{"))
                        && cols[0].contains(&format!("{p_repo}="))
                        && cols[0].contains(&format!("{p_anon}="))
                        && (cols[1].contains("opt:") || cols[1].contains("[scope"))
                })
            })
            .expect("Contributor scoped query teaching table row");
        assert!(
            parse_trailing_projection_bracket(contrib.split('\t').next().unwrap_or("").trim())
                .is_some(),
            "Contributor scoped query should carry the canonical projection suffix: {contrib:?}"
        );
        assert!(
            contrib.starts_with('e') && contrib.contains("{p"),
            "contributor query row should be a brace-query exemplar: {contrib:?}"
        );
        assert!(
            !contrib.contains("args:"),
            "capability legends omit inline `args:`; contributor row was: {contrib:?}"
        );
        assert!(
            contrib.contains("opt:") || contrib.contains("[scope") || contrib.contains("inputs:"),
            "contributor query Meaning should carry optionality, scope, or inputs legend: {contrib:?}"
        );
    }

    #[test]
    fn tsv_teaching_emitted_directly_has_no_compact_domain_separator_in_table() {
        let dir = apis_dir("dnd5e");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let Some(idx) = prompt.find(TSV_TEACHING_TABLE_HEADER) else {
            panic!(
                "expected {} in rendered prompt",
                TSV_TEACHING_TABLE_HEADER.trim_end()
            );
        };
        let table = &prompt[idx..];
        validate_teaching_tsv_teaching_table(table).expect("TSV teaching invariant");
        for line in table.lines().skip(1) {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            assert!(
                !line.contains(";;"),
                "direct TSV emission must not leak compact teaching table transcript tokens: {line:?}"
            );
        }
    }

    #[test]
    fn teaching_expr_line_from_layers_splits_result_and_capability_legend() {
        let row = teaching_expr_line_from_layers(
            "e2(p20=$, p11=$)",
            Some("e2 · gloss with no delimiter issue"),
            Some("[scope p20→e4] — cap desc"),
        );
        assert_eq!(row.expression, "e2(p20=$, p11=$)");
        assert_eq!(row.result_type, "e2 · gloss with no delimiter issue");
        assert!(
            row.scope.contains("scope") || row.description.contains("cap"),
            "expected capability legend in scope/description: scope={:?} desc={:?}",
            row.scope,
            row.description
        );
    }

    #[test]
    fn teaching_expr_line_from_layers_preserves_double_spaces_in_result_gloss() {
        let row = teaching_expr_line_from_layers("e1()", Some("part1  part2"), Some("[scope x]"));
        assert_eq!(row.result_type, "part1  part2");
    }

    #[test]
    fn teaching_expr_line_from_layers_double_space_in_result_before_scope() {
        let row = teaching_expr_line_from_layers("e1()", Some("e2 · tail  "), Some("[scope x]"));
        assert_eq!(row.result_type, "e2 · tail");
        assert!(row.scope.contains("scope") || row.description.contains('['));
    }

    #[test]
    fn prompt_matrix_zone_domain_no_unary_placeholder_relation_or_fake_projection_meaning() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let lines = domain_example_lines(&cgs, "Zone", Some(map.as_ref()), None);
        for line in &lines {
            let head = line.trim();
            assert!(
                !(head.contains("($)") && head.contains('.')),
                "relation/method recv must not use invalid unary identity get `e#($).…`: {head}"
            );
        }
        let mut line_valid_cache = HashMap::new();
        let mut gloss_emit_none = None;
        let block = collect_entity_teaching_block(
            &cgs,
            "Zone",
            Some(&map),
            None,
            false,
            &mut line_valid_cache,
            prompt_line_valid_cache_seed_cgs(&cgs),
            &mut gloss_emit_none,
            None,
            None,
        );
        let witness_row = block.teaching_rows.iter().find(|r| {
            r.teaching_expr.is_projection_teaching
                && parse_trailing_projection_bracket(r.teaching_expr.expression.trim()).is_some()
        });
        let Some(row) = witness_row else {
            panic!("expected a projection witness row for Zone teaching table; lines={lines:?}");
        };
        let expr = row.teaching_expr.expression.as_str();
        let legend = DomainTsvMeaningCell::from_teaching_atoms(teaching_expr_meaning_atoms(
            &row.teaching_expr,
            false,
            false,
            &TeachingHeading::default(),
        ))
        .as_str()
        .to_owned();
        assert!(
            domain_line_validate_cached(
                &mut HashMap::new(),
                prompt_line_valid_cache_seed_cgs(&cgs),
                &cgs,
                expr,
                Some(&map),
            )
            .is_some(),
            "projection witness must parse+typecheck: {expr}"
        );
        assert!(
            !legend.contains("projection [") && !legend.contains("· projection ["),
            "projection Meaning must not use legacy `projection […]` gloss prefix: {legend:?}"
        );
        assert!(
            !legend.contains("$)["),
            "projection Meaning must not embed a fake `…($)[…]` exemplar: {legend:?}"
        );
    }

    #[test]
    fn plasm_language_contract_defines_ref_meaning_prefix() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        assert!(
            prompt.contains("ref:Zone") && prompt.contains("str · Zone identifier"),
            "teaching TSV must include entity-ref value-domain gloss with canonical entity (not e#):\n{prompt}"
        );
    }

    #[test]
    fn prompt_matrix_symbolic_prompt_avoids_raw_zone_id_navigation_suffix() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        for line in prompt.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((expr, _)) = line.split_once('\t') else {
                continue;
            };
            if expr == "plasm_expr" {
                continue;
            }
            if expr.starts_with('e') && expr.contains('.') {
                assert!(
                    !expr.contains(".zone_id"),
                    "entity_ref fields must teach symbolic p# navigation, not raw `.zone_id`: {expr}"
                );
            }
        }
    }

    #[test]
    fn prompt_matrix_zone_entity_ref_value_domain_gloss_includes_id_primitive() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let p = map.ident_sym_entity_field("Ruleset", "zone_id");
        let v = map
            .value_sym_for_p_sym(&p)
            .expect("Ruleset.zone_id should map to a value-domain symbol");
        let g = map
            .value_domain_gloss_for_v_sym(v)
            .expect("value-domain gloss");
        assert!(
            g.starts_with("ref:Zone · str ·"),
            "expected ref:Zone · str · … value-domain gloss, got {g:?}"
        );
    }

    #[test]
    fn exposure_surface_omits_entity_ref_nav_when_target_entity_not_exposed() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let entry = cgs.entry_id.clone().unwrap_or_default();
        let delta = crate::discovery::derive_intent_exposure_surface_batch(
            &cgs,
            entry.as_str(),
            "rules traffic handling Cloudflare zone firewall WAF",
            &crate::relation_endpoint_keys(entry.as_str(), &["Ruleset".to_string()]),
            &["Ruleset".to_string()],
            None,
            crate::discovery::ExposureSurfaceOptions::default(),
        );
        assert!(
            delta
                .required
                .entities
                .iter()
                .any(|e| e.entity.as_str() == "Ruleset"),
            "expected Ruleset in exposure entities"
        );
        assert!(
            !delta
                .required
                .entities
                .iter()
                .any(|e| e.entity.as_str() == "Zone"),
            "narrow wave should not list Zone as an exposed entity"
        );
        let map = symbol_map_for_prompt(&cgs, FocusSpec::SeedsExact(&["Ruleset"]), true)
            .expect("symbol map");
        let zone_nav_sym = map.ident_sym_entity_field("Ruleset", "zone_id");
        let mut line_valid_cache = HashMap::new();
        let mut gloss_emit_none = None;
        let block = collect_entity_teaching_block(
            &cgs,
            "Ruleset",
            Some(&map),
            None,
            false,
            &mut line_valid_cache,
            prompt_line_valid_cache_seed_cgs(&cgs),
            &mut gloss_emit_none,
            Some(&delta.required),
            Some(entry.as_str()),
        );
        let has_zone_nav = block.teaching_rows.iter().any(|r| {
            let ex = r.teaching_expr.expression.as_str();
            ex.contains('.') && ex.contains(zone_nav_sym.as_str())
        });
        assert!(
            !has_zone_nav,
            "zone_id navigation should be omitted when Zone is not on the exposure entity set; exprs={:?}",
            block
                .teaching_rows
                .iter()
                .map(|r| r.teaching_expr.expression.as_str())
                .collect::<Vec<_>>()
        );

        let mut surface_with_zone = delta.required.clone();
        surface_with_zone.entities.insert(ExposureEntityKey {
            entry_id: entry.clone(),
            entity: EntityName::from("Zone"),
        });
        let mut line_valid_cache2 = HashMap::new();
        let mut gloss_emit_none2 = None;
        let block2 = collect_entity_teaching_block(
            &cgs,
            "Ruleset",
            Some(&map),
            None,
            false,
            &mut line_valid_cache2,
            prompt_line_valid_cache_seed_cgs(&cgs),
            &mut gloss_emit_none2,
            Some(&surface_with_zone),
            Some(entry.as_str()),
        );
        assert!(
            block2.teaching_rows.iter().any(|r| {
                let ex = r.teaching_expr.expression.as_str();
                (ex.contains('.') && ex.contains(zone_nav_sym.as_str()))
                    || ex.contains("Zone(")
                    || ex.contains("Zone($)")
            }),
            "adding Zone to exposure entities should admit zone_id navigation again; exprs={:?}",
            block2
                .teaching_rows
                .iter()
                .map(|r| r.teaching_expr.expression.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn incoming_relation_nav_bases_respect_exposure_surface_parent_and_slots() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let entry = cgs.entry_id.clone().unwrap_or_default();
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let zone_es = map.entity_sym("Zone");
        let mut nav_cache = HashMap::new();
        let nav_seed = prompt_line_valid_cache_seed_cgs(&cgs);
        let unfiltered = super::incoming_relation_nav_bases_to_entity(
            &cgs,
            "Ruleset",
            Some(map.as_ref()),
            None,
            entry.as_str(),
            &mut nav_cache,
            nav_seed,
            Some(&map),
        );
        assert!(
            unfiltered.iter().any(|line| line.contains(zone_es.as_str())),
            "without surface filter expect Zone-anchored incoming bases toward Ruleset; got {unfiltered:?}"
        );

        let delta = crate::discovery::derive_intent_exposure_surface_batch(
            &cgs,
            entry.as_str(),
            "rules traffic handling Cloudflare zone firewall WAF",
            &crate::relation_endpoint_keys(entry.as_str(), &["Ruleset".to_string()]),
            &["Ruleset".to_string()],
            None,
            crate::discovery::ExposureSurfaceOptions::default(),
        );
        let filtered = super::incoming_relation_nav_bases_to_entity(
            &cgs,
            "Ruleset",
            Some(map.as_ref()),
            Some(&delta.required),
            entry.as_str(),
            &mut nav_cache,
            nav_seed,
            Some(&map),
        );
        assert!(
            !filtered
                .iter()
                .any(|line| line.contains(zone_es.as_str())),
            "Zone must not anchor incoming projection bases when Zone is absent from exposure.entities; got {filtered:?}"
        );
    }

    #[test]
    fn prompt_matrix_zone_id_p_slot_gloss_omits_duplicate_values_row_prose() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let p = map.ident_sym_entity_field("Ruleset", "zone_id");
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        for line in prompt.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((expr, meaning)) = line.split_once('\t') else {
                continue;
            };
            if expr == p {
                assert!(
                    !meaning.contains("Zone identifier"),
                    "compact p# gloss must not repeat values: row description; got {meaning:?}"
                );
            }
        }
    }

    #[test]
    fn prompt_matrix_zone_projection_tsv_row_has_exactly_one_machine_tab() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let mut line_valid_cache = HashMap::new();
        let mut gloss_emit_none = None;
        let block = collect_entity_teaching_block(
            &cgs,
            "Zone",
            Some(&map),
            None,
            false,
            &mut line_valid_cache,
            prompt_line_valid_cache_seed_cgs(&cgs),
            &mut gloss_emit_none,
            None,
            None,
        );
        let witness_row = block.teaching_rows.iter().find(|r| {
            r.teaching_expr.is_projection_teaching
                && parse_trailing_projection_bracket(r.teaching_expr.expression.trim()).is_some()
        });
        let Some(row) = witness_row else {
            panic!("expected a projection witness row for Zone teaching table");
        };
        let expr = row.teaching_expr.expression.as_str();
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let line = prompt.lines().find(|l| {
            if l.is_empty() || l.starts_with('#') {
                return false;
            }
            l.split_once('\t').is_some_and(|(e, _)| e == expr)
        });
        let Some(line) = line else {
            panic!("TSV row for witness expr not found: {expr:?}");
        };
        assert_eq!(
            line.bytes().filter(|b| *b == b'\t').count(),
            1,
            "teaching table row must use exactly one U+0009 column delimiter; line={line:?}"
        );
    }

    #[test]
    fn prompt_matrix_ruleset_tsv_teaching_semantics() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        assert!(
            !prompt.contains("List rulesets on a zone"),
            "ruleset_query capability prose must not leak into TSV Meaning"
        );
        let desc = "Rules configuration held here";
        assert_eq!(
            prompt.matches(desc).count(),
            1,
            "Ruleset entity description should appear exactly once (terminal `.` stripped for agent gloss); excerpt around Ruleset teaching rows should be inspected"
        );
        let bundle = render_teaching_prompt_bundle(&cgs, RenderConfig::for_eval(None));
        let (names, _) = resolve_prompt_surface_entities(&cgs, FocusSpec::All, true);
        let idx = names
            .iter()
            .position(|n| n == "Ruleset")
            .expect("Ruleset in surface");
        let block = &bundle.teaching_blocks[idx];
        let rows: Vec<_> = block
            .teaching_rows
            .iter()
            .map(|r| &r.teaching_expr)
            .collect();
        let proj_i = rows
            .iter()
            .position(|r| r.is_projection_teaching)
            .expect("Ruleset projection witness");
        let mut order: Vec<usize> = (0..rows.len()).collect();
        order.sort_by_key(|&i| (!rows[i].is_projection_teaching, i));
        assert_eq!(
            order[0], proj_i,
            "TSV encoder emits projection witness rows before other teaching rows"
        );
        let compound_i = rows.iter().position(|r| {
            r.expression.contains('(')
                && r.expression.contains(',')
                && !r.expression.contains('{')
                && !r.is_projection_teaching
        });
        let query_i = rows
            .iter()
            .position(|r| r.expression.contains('{') && !r.is_projection_teaching);
        if let Some(ci) = compound_i {
            assert!(
                proj_i < ci,
                "projection witness should precede compound get in synthesis order"
            );
        }
        if let Some(qi) = query_i {
            assert!(
                proj_i < qi,
                "projection witness should precede query brace line in synthesis order"
            );
        }
    }

    #[test]
    fn prompt_matrix_waf_package_query_projection_witness_row() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let mut line_valid_cache = HashMap::new();
        let mut gloss_emit_none = None;
        let block = collect_entity_teaching_block(
            &cgs,
            "WafPackage",
            Some(&map),
            None,
            false,
            &mut line_valid_cache,
            prompt_line_valid_cache_seed_cgs(&cgs),
            &mut gloss_emit_none,
            None,
            None,
        );
        let witness = block.teaching_rows.iter().find(|r| {
            r.teaching_expr.is_projection_teaching
                && parse_trailing_projection_bracket(r.teaching_expr.expression.trim()).is_some()
        });
        let Some(row) = witness else {
            panic!(
                "expected query-backed projection witness for WafPackage; rows={:?}",
                block
                    .teaching_rows
                    .iter()
                    .map(|r| r.teaching_expr.expression.as_str())
                    .collect::<Vec<_>>()
            );
        };
        assert!(
            row.teaching_expr.expression.contains('{'),
            "witness base should be query-shaped brace form: {}",
            row.teaching_expr.expression
        );
        let expr = row.teaching_expr.expression.as_str();
        let Some(wp_ent) = cgs.get_entity("WafPackage") else {
            panic!("missing WafPackage entity");
        };
        if wp_ent.abstract_entity {
            // Abstract entities are omitted from default teaching slices — explicit teaching
            // collection still synthesizes witness rows for tooling/tests.
            let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
            assert!(
                !prompt.lines().any(|l| {
                    !l.starts_with('#')
                        && !l.is_empty()
                        && l.split_once('\t').is_some_and(|(e, _)| e == expr)
                }),
                "abstract WafPackage lines must not appear in default teaching TSV: {expr:?}"
            );
            return;
        }
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let line = prompt.lines().find(|l| {
            !l.starts_with('#')
                && !l.is_empty()
                && l.split_once('\t').is_some_and(|(e, _)| e == expr)
        });
        let Some(line) = line else {
            panic!("TSV row for WafPackage projection witness not found: {expr:?}");
        };
        assert_eq!(
            line.bytes().filter(|b| *b == b'\t').count(),
            1,
            "single tab delimiter; line={line:?}"
        );
        assert!(
            line.split_once('\t')
                .is_some_and(|(_, m)| m.contains("· projection")),
            "Meaning should include projection gloss: {line:?}"
        );
    }

    #[test]
    fn seeded_abstract_entity_assigns_symbol_and_teaching_row() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let pipeline = PromptPipelineConfig::default();
        let exp = TeachingExposureSession::new(&cgs, "", &["WafPackage"]);
        assert!(exp.contains_qualified_entity("", "WafPackage"));
        assert_eq!(exp.entities, vec!["WafPackage".to_string()]);
        let first = pipeline.render_teaching_first_wave_for_session(&cgs, &exp, None);
        let (_, body) = split_tsv_teaching_contract_and_table(&first);
        validate_teaching_tsv_teaching_table(&body).expect("valid teaching rows for abstract seed");
        assert!(
            body.lines().any(|l| l.starts_with("e1")),
            "abstract WafPackage seed must produce executable e1 row: {body}"
        );
    }

    #[test]
    fn prompt_matrix_duplicate_registry_p_slot_gloss_suppressed() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let Some(idx) = prompt.find(TSV_TEACHING_TABLE_HEADER) else {
            panic!("expected teaching TSV header");
        };
        fn count_slot_rows(body: &str, prefix: &str) -> usize {
            body.lines()
                .filter(|l| {
                    let l = l.strip_suffix('\r').unwrap_or(l);
                    !l.is_empty()
                        && !l.starts_with('#')
                        && l.split_once('\t').is_some_and(|(cell, _)| cell == prefix)
                })
                .count()
        }
        let table = &prompt[idx..];
        assert_eq!(
            count_slot_rows(table, "p14"),
            1,
            "shared p14 id slot must not repeat an identical registry-backed gloss row"
        );
        assert_eq!(
            count_slot_rows(table, "p15"),
            1,
            "shared p15 name slot must not repeat an identical registry-backed gloss row"
        );
    }

    /// Full `apis/github` TSV teaching prompt (symbolic). Output is **deterministic** for the
    /// tree’s `apis/github` catalog. When the catalog or renderer changes, run
    /// `just update-insta-snapshots` (repo root) or
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

    /// Canonical static `plasm` tool description; update with `INSTA_UPDATE=1 cargo test -p plasm-core plasm_tool_description_snapshot`.
    #[test]
    fn plasm_tool_description_snapshot() {
        with_insta_snapshots(|| {
            insta::assert_snapshot!("plasm_tool_description", super::PLASM_TOOL_DESCRIPTION);
        });
    }

    #[test]
    fn plasm_tool_description_includes_row_compute_worked_example() {
        let frontmatter = super::PLASM_TOOL_DESCRIPTION;
        assert!(frontmatter.contains(".filter{"));
        assert!(frontmatter.contains(".limit(10)"));
        assert!(frontmatter.contains("Core surface:"));
        assert!(frontmatter.contains("Worked shape"));
        assert!(
            frontmatter.contains("Replace teaching placeholders")
                || frontmatter.contains("substitute")
        );
        assert!(frontmatter.contains("e#~$"));
        assert!(
            !frontmatter.contains("Session and symbol discipline"),
            "session discipline belongs in tool workflow descriptions, not duplicated in grammar"
        );
        assert!(
            !frontmatter.contains(" ::="),
            "full pseudo-EBNF block retired; canonical syntax is Core surface + worked examples"
        );
        assert!(
            frontmatter.contains("label = e#"),
            "pitfalls must teach bind-before-filter preference"
        );
        assert!(
            !frontmatter.contains("e2(p10="),
            "canonical frontmatter must not hardcode catalog-specific symbol indices"
        );
    }

    #[test]
    fn mcp_static_tool_descriptions_byte_budget() {
        const MAX_WORKFLOW_BYTES: usize = 2500;
        const MAX_PLASM_TOOL_DESCRIPTION_BYTES: usize = 8000;

        let workflow = super::MCP_INITIALIZE_WORKFLOW;
        let plasm_tool = super::PLASM_TOOL_DESCRIPTION;
        let discover = super::DISCOVER_TOOL_DESCRIPTION;
        let context = super::PLASM_CONTEXT_TOOL_DESCRIPTION;
        let param = super::PLASM_PROGRAM_PARAM_DESCRIPTION;

        assert!(
            workflow.len() <= MAX_WORKFLOW_BYTES,
            "initialize workflow too long: {} bytes",
            workflow.len()
        );
        assert!(
            plasm_tool.len() <= MAX_PLASM_TOOL_DESCRIPTION_BYTES,
            "plasm tool description too long: {} bytes",
            plasm_tool.len()
        );
        assert!(
            discover.len() <= 550,
            "discover tool description too long: {} bytes",
            discover.len()
        );
        assert!(
            context.len() <= 1400,
            "plasm_context tool description too long: {} bytes",
            context.len()
        );
        assert!(plasm_tool.contains(super::MCP_TOOL_SYNTAX_CONTRACT_MARKER));
        assert!(plasm_tool.contains("literal no-op"));
        assert!(param.contains("not JSON data"));
        assert!(param.contains("e3(p15=\"value\").r2[p4]"));
        assert!(param.len() < 200);
    }

    #[test]
    fn plasm_tool_description_stats() {
        const MAX_PLASM_TOOL_DESCRIPTION_BYTES: usize = 8000;

        let full = super::PLASM_TOOL_DESCRIPTION;
        assert!(
            full.len() <= MAX_PLASM_TOOL_DESCRIPTION_BYTES,
            "grammar contract grew past max budget: {} bytes",
            full.len()
        );

        let full_stats = super::grammar_frontmatter_stats_from_contract(full);
        assert!(
            full_stats
                .section_bytes
                .get("core_surface")
                .copied()
                .unwrap_or(0)
                > 400
        );
        assert!(
            full_stats
                .section_bytes
                .get("symbol_rules")
                .copied()
                .unwrap_or(0)
                > 500
        );

        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let full_prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let single_prompt = render_prompt_tsv_with_config(
            &cgs,
            RenderConfig {
                focus: FocusSpec::Single("Ruleset"),
                ..RenderConfig::for_eval(None)
            },
        );
        let full_prompt_stats = super::grammar_frontmatter_stats_from_prompt(&full_prompt);
        let single_stats = super::grammar_frontmatter_stats_from_prompt(&single_prompt);
        assert!(
            single_stats.contract_comment_bytes <= full_prompt_stats.contract_comment_bytes,
            "single-entity slice should not add contract comments to teaching TSV"
        );
    }

    /// Reports contract/table ratio for matrix fixture and one real catalog (stderr only on failure).
    #[test]
    fn grammar_frontmatter_stats_matrix_and_catalog() {
        let matrix_dir = fixtures_schemas_dir("plasm_prompt_matrix");
        if !matrix_dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&matrix_dir).unwrap();
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let st = super::grammar_frontmatter_stats_from_prompt(&prompt);
        assert_eq!(st.contract_comment_bytes, 0);
        assert!(st.table_bytes > 0);
        assert!(
            st.contract_comment_bytes + st.table_bytes <= st.total_prompt_bytes,
            "contract + table should not exceed prompt size"
        );
        eprintln!(
            "grammar_frontmatter_stats plasm_prompt_matrix: {}",
            st.summary_line_body()
        );

        for catalog in ["linear", "github"] {
            let dir = apis_dir(catalog);
            if !dir.is_dir() {
                continue;
            }
            let cgs = load_schema_dir(&dir).unwrap();
            let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
            let st = super::grammar_frontmatter_stats_from_prompt(&prompt);
            assert_eq!(st.contract_comment_bytes, 0);
            assert!(st.table_bytes > 0);
            eprintln!(
                "grammar_frontmatter_stats apis/{catalog}: {}",
                st.summary_line_body()
            );
            return;
        }
    }

    /// Search teaching rows must not invite copy-paste of `e#~$` (grammar teaches `e#~"text"`).
    #[test]
    fn domain_search_teaching_rows_use_quoted_text_not_dollar() {
        let dir = apis_dir("linear");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        for line in prompt.lines() {
            if line.contains('~') && line.starts_with('e') {
                assert!(
                    !line.contains("~$"),
                    "search teaching row must not contain ~$: {line}"
                );
            }
        }
    }

    /// Row producers teach `inputs:` / `rows:` contract on search and query exemplars.
    #[test]
    fn row_producer_teaching_includes_inputs_and_rows_contract() {
        let dir = fixtures_schemas_dir("plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        assert!(
            prompt.lines().any(|l| l.contains("rows:")),
            "teaching rows should carry row contract via rows: gloss:\n{prompt}"
        );
        assert!(
            prompt
                .lines()
                .any(|l| { l.contains("~\"text\"") && l.contains('[') && l.contains("rows:") }),
            "search exemplar should append provides bracket and rows contract:\n{prompt}"
        );
        assert!(
            prompt.lines().any(|l| {
                l.contains("~\"text\"{")
                    && l.contains('[')
                    && l.contains("inputs:")
                    && l.contains("rows:")
            }),
            "filtered search should show brace inputs, row bracket, and contract:\n{prompt}"
        );
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
        assert!(
            !out.contains("Search-only entities"),
            "Type-only slice must omit search-only entity pitfall"
        );
        assert!(
            !out.contains("Search: copy"),
            "Type-only slice must omit search pitfall when slice has no Search capability"
        );
        with_insta_snapshots(|| {
            insta::assert_snapshot!("pokeapi_type_only_slice_prompt", out);
        });
    }

    #[test]
    fn static_grammar_includes_symbols_only_rule() {
        assert!(
            super::PLASM_TOOL_DESCRIPTION.contains("Symbols only"),
            "canonical static grammar must teach TSV-only program tokens"
        );
    }

    #[test]
    fn teaching_prompt_bundle_tags_relation_nav_materialization() {
        let dir = apis_dir("pokeapi");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let bundle = render_teaching_prompt_bundle(&cgs, RenderConfig::for_eval_seeds(&["Type"]));
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
            "expected a relation teaching line with FromParentGet metadata"
        );
        let mut cfg = RenderConfig::for_eval_canonical(None);
        cfg.include_domain_execution_model = false;
        let bundle2 = render_teaching_prompt_bundle(&cgs, cfg);
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
        let bundle = render_teaching_prompt_bundle(
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
            .expect("Pet teaching block");
        let bound = pet
            .lines
            .iter()
            .filter(|l| l.source_capability.is_some())
            .count();
        assert!(
            bound > 0,
            "expected at least one teaching table line bound to a CGS capability id"
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

    /// `Team(id).spaces` uses `query_scoped` materialization — it parses as [`Expr::Chain`]; teaching table shows
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
        let spaces_rel = map.ident_sym_relation("Team", "spaces");
        assert!(
            raw.contains(".spaces")
                && (raw.contains("Team($)") || raw.contains("Team{"))
                && raw.contains("Team"),
            "expected Team→spaces relation line (chain materialization; receiver may be `Team($)` or query-scoped `Team{{…}}`)"
        );
        let team_ent = cgs.get_entity("Team").expect("Team");
        let p_team_identity = map.ident_sym_entity_field("Team", team_ent.id_field.as_str());
        assert!(
            sym.contains(&format!(".{spaces_rel}"))
                || sym.contains(&format!("{team_sym}($).{spaces_rel}"))
                || sym.contains(&format!(
                    "{team_sym}({p_team_identity}).{spaces_rel}"
                ))
                || sym.contains(&format!("{team_sym}{{")),
            "expected symbol-tuned Team→spaces relation (`.{spaces_rel}` on a `{team_sym}` receiver)"
        );
        assert!(
            raw.contains("Space{") && raw.contains("team_id"),
            "Space scoped query with team_id should remain in teaching table (canonical)"
        );
        assert!(
            sym.contains("Space{")
                || (sym.contains("{p") && sym.contains(&format!("={}(", team_sym)))
                || raw.contains("Space{"),
            "Space scoped query should remain in teaching table"
        );
    }

    /// `team_query` is query-shaped (`e1` in teaching table); capability prose is intentionally omitted from
    /// `Meaning` (types teach shape); see `omit_capability_prose` in teaching synthesis.
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
            "QUERIES table removed — capability text lives on teaching lines"
        );
        assert!(
            !sym.contains("METHODS\n"),
            "METHODS table removed — invoke glosses live on teaching lines"
        );
        let domain_start = sym
            .find(TSV_TEACHING_TABLE_HEADER.trim_end())
            .expect("teaching table header");
        let domain_block = &sym[domain_start..];
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let team_sym = map.entity_sym("Team");
        assert!(
            super::PLASM_TOOL_DESCRIPTION.contains(super::TEACHING_VALID_EXPR_MARKER),
            "canonical grammar const should include valid-expression rules"
        );
        assert!(
            domain_block.lines().any(|line| {
                line.split_once('\t').is_some_and(|(expr, meaning)| {
                    expr.starts_with(team_sym.as_str())
                        && (meaning.contains('→') || meaning.contains("returns"))
                        && meaning.contains(&format!("[{team_sym}]"))
                })
            }),
            "TSV team_query should teach collection result gloss for Team (`[{team_sym}]`) without capability prose"
        );
        assert!(
            !domain_block.contains(" -> "),
            "relation / field nav lines must use `;;  => e#` (or `[e#]`), not `expr -> e#` before ;;"
        );
        let task_sym = map.entity_sym("Task");
        let p_team_id = map.ident_sym_cap_param("Task", "task_query", "team_id");
        let team_ent = cgs.get_entity("Team").expect("Team");
        let p_team_identity = map.ident_sym_entity_field("Team", team_ent.id_field.as_str());
        assert!(
            domain_block.contains(&format!(
                "{}{{{}={}({})",
                task_sym, p_team_id, team_sym, p_team_identity
            )) || domain_block.contains(&format!(
                "{}{{{}={}($)",
                task_sym, p_team_id, team_sym
            )),
            "workspace-scoped task query should teach scope with unary entity-ref fill-in (p#=e#(id_slot) or e#($)), not bare team id literals"
        );
        assert!(
            !domain_block.contains("2000-01-01") && !domain_block.contains("p10>=\""),
            "query teaching table brace form must not teach concrete ISO datetimes or `>=` date literals"
        );
        assert!(
            !domain_block.contains("List all accessible workspaces"),
            "query capability long-form description must not surface in TSV Meaning"
        );
    }

    /// User has only pathless singleton `user_get_me` — teaching table must show `e#.m#()` (get-me) and not mislead with `e#(42)`.
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
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let user_sym = map.entity_sym("User");
        assert!(
            sym.lines().any(|l| {
                l.split_once('\t').is_some_and(|(expr, _)| {
                    expr.contains(&format!("{user_sym}.m")) && expr.contains("()")
                })
            }),
            "User TSV should teach singleton get-me as e#.m#(), not id-based e#(42)"
        );
    }

    /// Book —(shelf)—> Shelf; two query caps; one navigation edge from Book.
    fn prompt_stats_fixture_cgs() -> CGS {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "fixture_str".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
            },
        );
        let id_field = FieldSchema {
            name: "id".into(),
            kind: FieldValueKind::Registry(ValueDomainKey::new("fixture_str").expect("key")),
            description: String::new(),
            required: true,
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
                discovery: None,
            }],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
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
            discovery: None,
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
                preflight: None,
                discovery: None,
            })
            .unwrap();
        }
        cgs.validate().unwrap();
        cgs
    }

    #[test]
    fn prompt_surface_stats_counts_caps_nav_and_domain_tools() {
        let cgs = prompt_stats_fixture_cgs();
        // Symbolic render modes — same entity slice as execute / [`teaching_exposure_session_from_focus`]
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
        // Book: one query line; Shelf: one. Many `shelf` relation is Unmaterialized → no nav line in teaching table.
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
            kind: FieldValueKind::Registry(ValueDomainKey::new("fixture_str").expect("key")),
            description: description.to_string(),
            required: true,
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
        cgs.values.insert(
            "fixture_str".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
            },
        );
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
                discovery: None,
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
                preflight: None,
                discovery: None,
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
            .find(TEACHING_VALID_EXPR_MARKER)
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

    /// Same-shaped `id` slots on different entities share one opaque `p#`; identical compact gloss is taught once.
    #[test]
    fn compact_domain_dedupes_identical_p_slot_gloss_across_entities() {
        let same = "P_SLOT_REIDENT_SAME";
        let cgs = p_slot_redefinition_fixture_cgs(same, same);
        let prompt = render_prompt_with_config(
            &cgs,
            RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
        );
        let domain = prompt
            .find(TEACHING_VALID_EXPR_MARKER)
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
            count, 1,
            "expected one p# gloss row when teaching strings match across entities; domain excerpt:\n{domain}"
        );
    }

    fn assert_prompt_examples_parse(dir: &std::path::Path) {
        assert_prompt_examples_valid(dir, RenderConfig::for_eval(None));
    }

    /// teaching lines must **parse**, **resolve** query capabilities where applicable, and **type-check**
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
            "expected teaching section with expressions for {}",
            dir.display()
        );
        for expr in &exprs {
            if let Some(arc) = map.as_ref() {
                let layers = [&cgs];
                let mut r = crate::expr_parser::parse_with_cgs_layers(expr, &layers, arc.clone())
                    .unwrap_or_else(|e| {
                        panic!(
                            "teaching table expr should parse (opaque) for {}: {expr:?}\n{e}",
                            dir.display()
                        );
                    });
                if let Err(e) = crate::normalize_expr_query_capabilities(&mut r.expr, &cgs) {
                    panic!(
                        "teaching table expr should resolve query capability for {}: {expr:?}\n{e}",
                        dir.display()
                    );
                }
                if let Err(e) = crate::type_check_expr(&r.expr, &cgs) {
                    panic!(
                        "teaching table expr should type-check for {}: {expr:?}\n{e}",
                        dir.display()
                    );
                }
            } else {
                let work = expr.clone();
                let mut r = crate::expr_parser::parse(&work, &cgs).unwrap_or_else(|e| {
                    panic!(
                        "teaching table expr should parse for {}: {expr:?}\n{e}",
                        dir.display()
                    );
                });
                if let Err(e) = crate::normalize_expr_query_capabilities(&mut r.expr, &cgs) {
                    panic!(
                        "teaching table expr should resolve query capability for {}: {expr:?}\n{e}",
                        dir.display()
                    );
                }
                if let Err(e) = crate::type_check_expr(&r.expr, &cgs) {
                    panic!(
                        "teaching table expr should type-check for {}: {expr:?}\n{e}",
                        dir.display()
                    );
                }
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

    /// Locks compact teaching table + symbol preamble for `fixtures/schemas/overshow_tools`.
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

    /// Locks teaching TSV render for the same fixture (review diffs with compact snapshot above).
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

    /// Federated open: colliding wire entity names get distinct `e#` in teaching TSV rows (B1).
    #[test]
    fn federated_duplicate_entity_wire_names_use_distinct_e_in_teaching_tsv() {
        use std::sync::Arc;

        let root = fixtures_schemas_dir("plasm_language_matrix");
        let cgs = load_schema_dir(&root).expect("plasm_language_matrix");
        let layers = [&cgs, &cgs];
        let mut exp = TeachingExposureSession::new(&cgs, "github", &["LangItem"]);
        exp.expose_entities(&layers, Arc::new(cgs.clone()), "linear", &["LangItem"]);
        let mut by_entry: IndexMap<String, &CGS> = IndexMap::new();
        by_entry.insert("github".into(), &cgs);
        by_entry.insert("linear".into(), &cgs);
        let bundle = render_teaching_prompt_bundle_for_exposure_federated(
            &by_entry,
            RenderConfig::for_eval(None),
            &exp,
            None,
        );
        assert!(
            bundle.teaching_blocks.len() >= 2,
            "expected github + linear LangItem blocks"
        );
        let row_text = |block: &EntityTeachingBlock| {
            block
                .teaching_rows
                .iter()
                .map(|r| r.teaching_expr.expression.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let github_text = row_text(&bundle.teaching_blocks[0]);
        let linear_text = row_text(&bundle.teaching_blocks[1]);
        assert!(
            github_text.contains("e1"),
            "github LangItem teaching rows should use e1: {github_text}"
        );
        assert!(
            !github_text.contains("e2"),
            "github block must not bleed linear e2: {github_text}"
        );
        assert!(
            linear_text.contains("e2"),
            "linear LangItem teaching rows should use e2: {linear_text}"
        );
    }

    /// Production catalogs: `github/Issue` + `linear/Issue` federated TSV uses e1 vs e2.
    #[test]
    fn federated_github_linear_issue_distinct_e_symbols_when_apis_present() {
        use std::sync::Arc;

        let github_dir = apis_dir("github");
        let linear_dir = apis_dir("linear");
        if !github_dir.is_dir() || !linear_dir.is_dir() {
            return;
        }
        let mut cgs_github = load_schema_dir(&github_dir).expect("github");
        cgs_github.entry_id = Some("github".into());
        let mut cgs_linear = load_schema_dir(&linear_dir).expect("linear");
        cgs_linear.entry_id = Some("linear".into());
        let layers = [&cgs_github, &cgs_linear];
        let mut exp = TeachingExposureSession::new(&cgs_github, "github", &["Issue"]);
        exp.expose_entities(&layers, Arc::new(cgs_linear.clone()), "linear", &["Issue"]);
        let mut by_entry: IndexMap<String, &CGS> = IndexMap::new();
        by_entry.insert("github".into(), &cgs_github);
        by_entry.insert("linear".into(), &cgs_linear);
        let bundle = render_teaching_prompt_bundle_for_exposure_federated(
            &by_entry,
            RenderConfig::for_eval(None),
            &exp,
            None,
        );
        assert!(bundle.teaching_blocks.len() >= 2);
        let github_has_e1 = bundle.teaching_blocks[0]
            .teaching_rows
            .iter()
            .any(|r| r.teaching_expr.expression.contains("e1"));
        let linear_has_e2 = bundle.teaching_blocks[1]
            .teaching_rows
            .iter()
            .any(|r| r.teaching_expr.expression.contains("e2"));
        let linear_not_only_e1 = !bundle.teaching_blocks[1].teaching_rows.iter().all(|r| {
            r.teaching_expr.expression.contains("e1") && !r.teaching_expr.expression.contains("e2")
        });
        assert!(github_has_e1, "github Issue block should teach e1");
        assert!(linear_has_e2, "linear Issue block should teach e2");
        assert!(linear_not_only_e1, "linear block must not reuse github e1");
    }
}
