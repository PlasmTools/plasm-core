//! CGS prompt renderer — TSV **Plasm** many-shot examples: each teaching row is `plasm_expr`, **one tab (U+0009)**,
//! then `Meaning` (middle-dot ` · ` joins gloss **inside** Meaning only). Synthesis builds structured
//! [`EntityTeachingBlock`] rows and emits TSV directly ([`render_prompt_tsv_from_bundle`]); synthesis stays structured
//! (model → [`TeachingExprLine`] / [`TeachingFieldGloss`]) without re-parsing a compact teaching transcript in production.
//! Symbolic prompts use `p#` / `v#` glosses emitted before first use (`v#` = shared `values:` domain;
//! each distinct taught `p#` meaning teaches **`v# · wire`** (and optional point-of-use prose) when the slot uses a `value_ref`, with typing on the `v#` row only).
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
//! In the teaching TSV, the entity `description` is attached to the **first projection witness** for that
//! entity when one exists, otherwise to the **identity** get row. Rows are phased per block: **`v#` gloss**
//! (except the deferred synthetic union summary), **`p#` gloss**, **union constructor exemplars**
//! (`vN{p#=…}`), **union summary** (`union · v101 | …` on an allocator-chosen `v#`), then remaining
//! teaching expressions (projection witnesses last). Value domain once per `v#`, then each distinct
//! `v# · wire` teaching once per shared `p#`; point-of-use prose is omitted when it duplicates the shared
//! `values:` row description.
//! Model output must be those expression shapes—not prose.
//! Use [`RenderConfig::focus`] to subset entities.
//!
//! **Relations** lines teach `Get(id).relation` when that path **parses and type-checks**. With an
//! [`ExposureSurface`](crate::symbol_tuning::ExposureSurface) filter (incremental DOMAIN waves), **outgoing**
//! navigation teaches only targets in the surface entity set, and **incoming** projection-witness bases
//! (`ParentRecv…[p#,…]`) require the parent entity on the surface plus the same slot checks as outgoing nav;
//! field gloss rows and `ref:*` typing are unchanged.
//! Meaning uses
//! `relation e#_src => [e#_tgt]` (many) or `relation e#_src => e#_tgt` (one); the full receiver stays in `plasm_expr`.
//! For terminal relation chains, the example line already carries a **result gloss** (`relation …`), so the redundant standalone `p#` gloss row
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
//! [`collect_entity_teaching_block`] (opaque symbol map in **compact**/**tsv** modes, matching eval / REPL).

use crate::{
    cross_entity::{choose_strategy, extract_cross_entity_predicates},
    schema::{
        capability_is_zero_arity_invoke, capability_method_label_kebab, Cardinality, EntityDef,
        InputFieldSchema, RelationMaterialization, RelationSchema,
    },
    symbol_tuning::{
        symbol_map_cache_key_federated, symbol_map_cache_key_single_catalog, DomainExposureSession,
        ExposureCapabilityKey, ExposureEntityKey, ExposureSlotKey, ExposureSurface, FocusSpec,
        IdentMetaKey, IdentMetadata, SymbolMap, SymbolMapCrossRequestCache,
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

/// Strip a leading markdown fenced block ` ```{fence_info}\\n … \\n``` ` and return inner body.
pub fn markdown_fence_body_inner<'a>(markdown: &'a str, fence_info: &str) -> Option<&'a str> {
    let open = format!("```{fence_info}\n");
    let rest = markdown.strip_prefix(&open)?;
    let end = rest.find("\n```")?;
    Some(&rest[..end])
}

/// DOMAIN TSV table fragment (from [`TSV_DOMAIN_TABLE_HEADER`] onward), dropping optional `#` contract lines inside the fence body.
pub fn domain_tsv_table_from_wrapped_prompt(prompt: &str, fence_info: &str) -> Option<String> {
    let inner = markdown_fence_body_inner(prompt, fence_info)?;
    Some(split_tsv_domain_contract_and_table(inner).1)
}

/// Invariant for prompts emitted by [`render_prompt_tsv_from_bundle`]: from the `plasm_expr\tMeaning`
/// header through the end of the table, every non-empty body line that is not a `#` comment uses
/// **exactly one** tab between the expression column and Meaning ([`DomainTsvEncodedLine::write_line`] only;
/// middle-dot ` · ` joins gloss fragments **inside** Meaning). Tab U+0009 is emitted solely at that boundary.
pub(crate) fn validate_domain_tsv_teaching_table(body_from_header: &str) -> Result<(), String> {
    let mut lines = body_from_header.lines();
    let header = lines
        .next()
        .ok_or_else(|| "empty DOMAIN TSV table".to_string())?;
    let header = header.strip_suffix('\r').unwrap_or(header);
    if header != "plasm_expr\tMeaning" {
        return Err(format!(
            "expected header `plasm_expr\\tMeaning`, got {:?}",
            header.chars().take(80).collect::<String>()
        ));
    }
    for (i, raw_line) in lines.enumerate() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tabs = line.bytes().filter(|b| *b == b'\t').count();
        if tabs != 1 {
            return Err(format!(
                "line {}: expected exactly one `\\t` between `plasm_expr` and `Meaning`, got {} tab(s): {:?}",
                i + 2,
                tabs,
                line.chars().take(160).collect::<String>()
            ));
        }
        let (expr, meaning) = line.split_once('\t').expect("one tab implies split_once");
        if expr.contains('\t') || meaning.contains('\t') {
            return Err(format!(
                "line {}: stray tab inside a cell after split",
                i + 2
            ));
        }
        let expr_trim = expr.trim();
        let meaning_trim = meaning.trim();
        if expr != expr_trim {
            return Err(format!(
                "line {}: `plasm_expr` cell must not have leading/trailing whitespace (got {:?})",
                i + 2,
                expr.chars().take(120).collect::<String>()
            ));
        }
        if meaning != meaning_trim {
            return Err(format!(
                "line {}: `Meaning` cell must not have leading/trailing whitespace",
                i + 2
            ));
        }
    }
    Ok(())
}

#[inline]
fn enforce_domain_tsv_teaching_invariant(prompt: &str) {
    let Some(idx) = prompt.find(TSV_DOMAIN_TABLE_HEADER) else {
        return;
    };
    let body = &prompt[idx..];
    if let Err(msg) = validate_domain_tsv_teaching_table(body) {
        tracing::error!(
            target: "plasm_core::prompt_render",
            error = %msg,
            "DOMAIN TSV teaching table invariant violated"
        );
        debug_assert!(false, "DOMAIN TSV: {msg}");
    }
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

/// Subset + render mode for DOMAIN / symbol expansion.
///
/// Prefer [`DomainPromptSource`] + [`DomainPromptSettings`] with [`render_domain_bundle`] /
/// [`render_domain_tsv`] for new product integrations; this struct remains the internal carrier
/// used throughout `plasm-core` and for snapshot tests.
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

    /// Full-schema DOMAIN synthesis for [`crate::cgs_expression_validate::validate_cgs_expression_surface`].
    ///
    /// Uses [`FocusSpec::All`], [`PromptRenderMode::Tsv`], and [`Self::include_domain_execution_model`] `true`
    /// so [`DomainPromptModel`] lines carry [`DomainLineMeta::source_capability`] metadata the validator
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

/// Product-facing **where** DOMAIN symbols are seeded from: catalog [`FocusSpec`] vs execute [`DomainExposureSession`].
#[derive(Clone, Copy, Debug)]
pub enum DomainPromptSource<'a> {
    Catalog { focus: FocusSpec<'a> },
    ExecuteWave { exposure: &'a DomainExposureSession },
}

/// Product-facing knobs for the teaching bundle / TSV (prefer over assembling [`RenderConfig`] at new call sites).
#[derive(Clone, Copy, Debug)]
pub struct DomainPromptSettings<'a> {
    pub include_domain_execution_model: bool,
    /// When false, teaching rows use canonical names (tool explorer / narrow tests); when true, `e#`/`p#`/`m#` symbolic TSV.
    pub symbolic: bool,
    pub symbol_map_cross_cache: Option<&'a SymbolMapCrossRequestCache>,
}

impl<'a> Default for DomainPromptSettings<'a> {
    fn default() -> Self {
        Self {
            include_domain_execution_model: true,
            symbolic: true,
            symbol_map_cross_cache: None,
        }
    }
}

/// Render DOMAIN [`DomainPromptBundle`] (structured teaching blocks + execution metadata).
pub fn render_domain_bundle(
    cgs: &CGS,
    source: DomainPromptSource<'_>,
    settings: DomainPromptSettings<'_>,
) -> DomainPromptBundle {
    let render_mode = if settings.symbolic {
        PromptRenderMode::Tsv
    } else {
        PromptRenderMode::Canonical
    };
    let include = settings.include_domain_execution_model;
    let cache = settings.symbol_map_cross_cache;
    match source {
        DomainPromptSource::Catalog { focus } => render_domain_prompt_bundle(
            cgs,
            RenderConfig {
                focus,
                render_mode,
                include_domain_execution_model: include,
                symbol_map_cross_cache: cache,
            },
        ),
        DomainPromptSource::ExecuteWave { exposure } => render_domain_prompt_bundle_for_exposure(
            cgs,
            RenderConfig {
                focus: FocusSpec::All,
                render_mode,
                include_domain_execution_model: include,
                symbol_map_cross_cache: cache,
            },
            exposure,
            None,
        ),
    }
}

/// Render DOMAIN as the teaching TSV (`plasm_expr` + `Meaning`), including contract header on first wave.
pub fn render_domain_tsv(
    cgs: &CGS,
    source: DomainPromptSource<'_>,
    settings: DomainPromptSettings<'_>,
) -> String {
    match source {
        DomainPromptSource::Catalog { focus } => {
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
        DomainPromptSource::ExecuteWave { exposure } => {
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

/// Per-entity DOMAIN lines with execution hints parallel to the rendered prompt strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DomainPromptModel {
    pub entities: Vec<EntityDomainPrompt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityDomainPrompt {
    /// Canonical CGS entity name (`Issue`, `Zone`, …) — not the session-local `e#` alias.
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
    QueryScoped,
    QueryScopedBindings,
    GetScopedBindings,
}

impl From<&RelationMaterialization> for RelationMaterializationSummary {
    fn from(m: &RelationMaterialization) -> Self {
        match m {
            RelationMaterialization::Unavailable => Self::Unavailable,
            RelationMaterialization::FromParentGet { .. } => Self::FromParentGet,
            RelationMaterialization::QueryScoped { .. } => Self::QueryScoped,
            RelationMaterialization::QueryScopedBindings { .. } => Self::QueryScopedBindings,
            RelationMaterialization::GetScopedBindings { .. } => Self::GetScopedBindings,
        }
    }
}

/// [`render_domain_prompt_bundle`] with [`RenderConfig::for_expression_surface_validation`].
///
/// Centralizes the config [`crate::cgs_expression_validate::validate_cgs_expression_surface`] must stay aligned with.
pub(crate) fn render_domain_prompt_bundle_for_validation(cgs: &CGS) -> DomainPromptBundle {
    render_domain_prompt_bundle(cgs, RenderConfig::for_expression_surface_validation())
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

    if let Some(ref map) = map_opt {
        let t_leg = Instant::now();
        let legend = map.format_legend(cgs);
        tracing::debug!(
            elapsed_ms = t_leg.elapsed().as_millis() as u64,
            legend_chars = legend.len(),
            "render_domain_prompt_bundle phase=format_legend"
        );
    }

    let t2 = Instant::now();
    tracing::debug!("prompt: render_domain_table");
    let mut teaching_blocks = Vec::new();
    let mut entities_buf = Vec::new();
    let fill_model = config.include_domain_execution_model;
    render_domain_table(
        cgs,
        &full_entities,
        map_opt.as_ref(),
        &mut teaching_blocks,
        &mut entities_buf,
        fill_model,
        false,
        None,
    );
    tracing::debug!(
        elapsed_ms = t2.elapsed().as_millis() as u64,
        teaching_entities = teaching_blocks.len(),
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
        teaching_entities = teaching_blocks.len(),
        total_elapsed_ms = wall.elapsed().as_millis() as u64,
        "render_domain_prompt_bundle done"
    );
    DomainPromptBundle {
        teaching_blocks,
        model,
    }
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

    let mut entities_buf = Vec::new();
    let mut teaching_blocks = Vec::new();
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
        &mut teaching_blocks,
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

    DomainPromptBundle {
        teaching_blocks,
        model,
    }
}

/// Teaching bundle using [`crate::symbol_tuning::DomainExposureSession`] (monotonic `e#`/`m#`/`p#`).
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

    let mut teaching_blocks = Vec::new();
    let mut entities_buf = Vec::new();
    let fill_model = config.include_domain_execution_model;
    render_domain_table_resolved(
        |_| cgs,
        &full_entities,
        map_opt.as_deref(),
        Some(exposure),
        &mut teaching_blocks,
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

    DomainPromptBundle {
        teaching_blocks,
        model,
    }
}

/// Render the Plasm teaching surface for the given CGS and [`RenderConfig`].
///
/// The only prompt-facing teaching form is TSV; this wrapper is retained for older callers that
/// historically asked for the markdown DOMAIN surface.
pub fn render_prompt_with_config(cgs: &CGS, config: RenderConfig<'_>) -> String {
    render_prompt_tsv_with_config(cgs, config)
}

/// TSV for a **single-catalog** [`DomainExposureSession`]: one [`render_domain_prompt_bundle_for_exposure`]
/// plus the session’s memoized [`SymbolMap`] / [`DomainExposureSession::ident_metadata_for_exposure_entities`]
/// so bundle rows and TSV metadata cannot drift.
pub(crate) fn render_prompt_tsv_for_single_catalog_exposure(
    cgs: &CGS,
    config: RenderConfig<'_>,
    exposure: &DomainExposureSession,
) -> String {
    let full_entities: Vec<&str> = exposure.entities.iter().map(|s| s.as_str()).collect();
    let bundle = render_domain_prompt_bundle_for_exposure(cgs, config, exposure, None);
    if config.uses_symbols() {
        let key = config
            .symbol_map_cross_cache
            .filter(|c| c.is_enabled())
            .map(|_| symbol_map_cache_key_single_catalog(cgs, exposure));
        let (symbol_map_arc, _) = exposure.symbol_map_arc_cross(config.symbol_map_cross_cache, key);
        let ident_meta = exposure.ident_metadata_for_exposure_entities(&full_entities);
        render_prompt_tsv_from_bundle(
            &bundle,
            &full_entities,
            Some(symbol_map_arc.as_ref()),
            Some(&ident_meta),
            DomainWaveSurface::InitialTeaching,
            true,
            |_| cgs,
        )
    } else {
        render_prompt_tsv_from_bundle(
            &bundle,
            &full_entities,
            None,
            None,
            DomainWaveSurface::InitialTeaching,
            false,
            |_| cgs,
        )
    }
}

/// Render the DOMAIN teaching surface as TSV with stable, Plasm-expression-first rows.
///
/// Columns:
/// `plasm_expr`, `Meaning`
pub fn render_prompt_tsv_with_config(cgs: &CGS, config: RenderConfig<'_>) -> String {
    if config.uses_symbols() {
        let exposure = crate::symbol_tuning::domain_exposure_session_from_focus(cgs, config.focus);
        return render_prompt_tsv_for_single_catalog_exposure(cgs, config, &exposure);
    }
    // Canonical names: 2-hop neighbourhood slice (not execute-parity [`DomainExposureSession`]).
    let (full_entity_names, _) =
        crate::symbol_tuning::resolve_prompt_surface_entities(cgs, config.focus, false);
    let full_entities: Vec<&str> = full_entity_names.iter().map(|s| s.as_str()).collect();
    let bundle = render_domain_prompt_bundle(cgs, config);
    render_prompt_tsv_from_bundle(
        &bundle,
        &full_entities,
        None,
        None,
        DomainWaveSurface::InitialTeaching,
        false,
        |_| cgs,
    )
}

pub(crate) fn render_prompt_surface_from_bundle<'b, F>(
    bundle: &DomainPromptBundle,
    symbolic: bool,
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
        symbolic,
        resolve,
    )
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
    /// `[scope …]` fragment when present (DOMAIN / capability-input legend).
    pub scope: String,
    pub optional_params: String,
    /// `args: p# wire type req; …` when the compact DOMAIN legend includes it.
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

/// DOMAIN teaching slices plus structured execution metadata for tooling / HTTP/MCP TSV emission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainPromptBundle {
    pub teaching_blocks: Vec<EntityTeachingBlock>,
    pub model: DomainPromptModel,
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
    pub meta: DomainLineMeta,
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

/// Numeric ordering for opaque `pN` / `vN` tokens (`p12` before `p101`, not lexicographic).
fn opaque_pv_symbol_sort_key(sym: &str) -> Option<(u32, u32)> {
    let mut it = sym.chars();
    let prefix = it.next()?;
    if prefix != 'p' && prefix != 'v' {
        return None;
    }
    let rest: String = it.collect();
    let n = rest.parse::<u32>().ok()?;
    Some((prefix as u32, n))
}

fn render_prompt_tsv_from_bundle<'b, F>(
    bundle: &DomainPromptBundle,
    full_entities: &[&str],
    _symbol_map: Option<&SymbolMap>,
    _ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
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
                write_domain_tsv_row(&mut out, DomainTsvRow::FieldGloss(g));
            }
        }
        // Phase B: `p#` gloss — non-projection numeric order, then projection bracket tail.
        // `p#` slots for optional query params / invokes appear in DOMAIN gloss lines but are not part
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
            write_domain_tsv_row(&mut out, DomainTsvRow::FieldGloss(g));
        }
        for sym in &projection_symbols {
            if emitted_p_slot.contains(sym) {
                continue;
            }
            if let Some(gloss) = field_gloss_by_symbol.get(sym.as_str()) {
                write_domain_tsv_row(&mut out, DomainTsvRow::FieldGloss(gloss));
                emitted_p_slot.insert(sym.clone());
            }
        }

        // Phase C: union constructor exemplars (`v101{p#=…}`) — before deferred union summary gloss.
        for &row_idx in &union_ctor_row_idxs {
            let row = teaching_expr_rows[row_idx];
            let identity_returns_row = Some(row_idx) == identity_idx;
            let attach_entity_heading = Some(row_idx) == entity_desc_attach_idx;
            write_domain_tsv_row(
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
                write_domain_tsv_row(&mut out, DomainTsvRow::FieldGloss(g));
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
            write_domain_tsv_row(
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
    enforce_domain_tsv_teaching_invariant(&out);
    out
}

const TSV_MEANING_JOIN: &str = " · ";

/// One logical TSV row before wire encoding ([`write_domain_tsv_row`]).
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

impl TeachingMeaningAtom {
    fn encoded_fragment(&self) -> String {
        let raw = match self {
            TeachingMeaningAtom::Returns { gloss } => format!("returns {gloss}"),
            TeachingMeaningAtom::RelationNav { line } => line.clone(),
            TeachingMeaningAtom::EntityHeadingDescription(s) => s.clone(),
            TeachingMeaningAtom::LegendScope(s) => s.clone(),
            TeachingMeaningAtom::LegendOptionalParams(s) => format!("optional params: {s}"),
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

/// Sanitized `plasm_expr` column for DOMAIN teaching TSV (no literal tabs; trimmed).
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

/// Sanitized `Meaning` column for DOMAIN teaching TSV (no literal tabs; trimmed).
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

/// One encoded DOMAIN teaching row: sanitized expr, **exactly one** U+0009, sanitized meaning, newline.
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

fn write_domain_tsv_row(out: &mut String, row: DomainTsvRow<'_>) {
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
        Some(c @ ('p' | 'v')) => c,
        _ => return,
    };
    let rest: String = cs.collect();
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return;
    }
    let field_name = symbol_map
        .and_then(|m| m.resolve_ident(symbol.as_str()))
        .unwrap_or(symbol.as_str())
        .to_string();
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
    if let Some(p) = tail.strip_prefix("optional params:") {
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
            navigation_tools += navigation_edge_count(cgs, ent);
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
    let surface_filter = exposure_opt.map(|e| &e.surface);
    for &ename in &full_entities {
        let mut seen_expr: HashSet<TeachingRowDedupeKey> = HashSet::new();
        let mut gloss_emit_none = None;
        let block = collect_entity_teaching_block(
            cgs,
            ename,
            map.as_deref(),
            None,
            false,
            &mut line_valid_cache,
            &mut gloss_emit_none,
            surface_filter,
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

/// Human capability / list gloss after `[scope …]` / `optional params:` (emit parity with
/// [`format_capability_legend_line`]): Unicode em dash U+2014, spaces around it.
const LEGEND_EM_DESC_SEP: &str = " — ";

const PROJECTION_WITNESS_LEGEND_MARK: &str = "· projection";

/// Ordered receiver bases for DOMAIN dotted calls / relation nav on `ent` (`es` = entity symbol).
fn nav_receiver_candidates(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    if let Some(cmp) = compound_get_expr_line(es, ent, cgs, map) {
        if seen.insert(cmp.clone()) {
            out.push(cmp);
        }
    }
    let mut query_caps: Vec<_> = cgs.find_capabilities(ent.name.as_str(), CapabilityKind::Query);
    query_caps.sort_by(|a, b| a.name.cmp(&b.name));
    for cap in &query_caps {
        for qline in [
            query_expr_maximal(cap, es, cgs, map),
            query_expr_scope_only(cap, es, cgs, map),
            query_expr_filters_only(cap, es, cgs, map),
        ]
        .into_iter()
        .flatten()
        {
            if seen.insert(qline.clone()) {
                out.push(qline);
            }
        }
    }
    let unary = unary_entity_id_teaching_expr_line(es, ent, map);
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
fn relation_nav_anchor_expr(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
) -> Option<String> {
    for recv in nav_receiver_candidates(es, ent, cgs, map) {
        let work = domain_line_work_string(&recv, map);
        if domain_line_valid_work(cgs, &work) {
            return Some(recv);
        }
    }
    None
}

/// First receiver such that `recv + suffix` is a valid full DOMAIN expression (e.g. `.m#(…)`).
fn receiver_for_dotted_suffix(
    es: &str,
    ent: &EntityDef,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    suffix: &str,
) -> Option<String> {
    for recv in nav_receiver_candidates(es, ent, cgs, map) {
        let full = format!("{recv}{suffix}");
        let work = domain_line_work_string(&full, map);
        if domain_line_valid_work(cgs, &work) {
            return Some(recv);
        }
    }
    None
}

const MAX_INCOMING_REL_NAV_PROJECTION_BASES: usize = 16;

/// `ParentRecv.rel` expressions that type-check and return `target_ename` (incoming edges).
///
/// With `surface_filter: Some`, only edges whose **parent** (`src_name`) is in
/// [`ExposureSurface::entities`] and passes [`surface_allows_relation_nav`] for that slot are kept —
/// symmetric with outgoing relation-nav rows on the parent entity block.
fn incoming_relation_nav_bases_to_entity(
    cgs: &CGS,
    target_ename: &str,
    map: Option<&SymbolMap>,
    surface_filter: Option<&ExposureSurface>,
    catalog_entry_id: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (src_key, src_ent) in cgs.entities.iter() {
        let src_name = src_key.as_str();
        if src_name == target_ename {
            continue;
        }
        if !surface_includes_exposed_entity(surface_filter, cgs, catalog_entry_id, src_name) {
            continue;
        }
        let parent_es = ent_sym(map, src_name);
        let rel_keys: HashSet<&str> = src_ent.relations.keys().map(|k| k.as_str()).collect();
        for (rel_k, rel_s) in src_ent.relations.iter() {
            if rel_s.target_resource.as_str() != target_ename {
                continue;
            }
            if rel_s.cardinality == Cardinality::Many && !many_relation_nav_emittable(rel_s) {
                continue;
            }
            if !surface_allows_relation_nav(
                surface_filter,
                catalog_entry_id,
                src_name,
                rel_k.as_str(),
                true,
            ) {
                continue;
            }
            let Some(recv) = relation_nav_anchor_expr(&parent_es, src_ent, cgs, map) else {
                continue;
            };
            let expr = format!("{}.{}", recv, id_sym_rel(map, src_name, rel_k.as_str()));
            let work = domain_line_work_string(&expr, map);
            if domain_line_valid_work(cgs, &work) && seen.insert(expr.clone()) {
                out.push(expr);
                if out.len() >= MAX_INCOMING_REL_NAV_PROJECTION_BASES {
                    return out;
                }
            }
        }
        for (fname, f) in src_ent.fields.iter() {
            if rel_keys.contains(fname.as_str()) {
                continue;
            }
            let Ok(nv) = f.named_value(cgs) else {
                continue;
            };
            let FieldType::EntityRef { target } = &nv.field_type else {
                continue;
            };
            if target.as_str() != target_ename {
                continue;
            }
            if !surface_allows_relation_nav(
                surface_filter,
                catalog_entry_id,
                src_name,
                fname.as_str(),
                false,
            ) {
                continue;
            }
            let Some(recv) = relation_nav_anchor_expr(&parent_es, src_ent, cgs, map) else {
                continue;
            };
            let expr = format!("{}.{}", recv, id_sym_entity(map, src_name, fname.as_str()));
            let work = domain_line_work_string(&expr, map);
            if domain_line_valid_work(cgs, &work) && seen.insert(expr.clone()) {
                out.push(expr);
                if out.len() >= MAX_INCOMING_REL_NAV_PROJECTION_BASES {
                    return out;
                }
            }
        }
    }
    out
}

/// Maps parsed projection witness to a capability id for DOMAIN coverage (see [`covered_capabilities`]).
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
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, bool>,
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
            query_expr_maximal(cap, es, cgs, map),
            query_expr_scope_only(cap, es, cgs, map),
            query_expr_filters_only(cap, es, cgs, map),
        ]
        .into_iter()
        .flatten()
        {
            if seen_bases.insert(qline.clone()) {
                attempts.push((qline, Some(cap)));
            }
        }
    }
    if let Some(cmp) = compound_get_expr_line(es, ent, cgs, map) {
        if seen_bases.insert(cmp.clone()) {
            attempts.push((cmp, primary_get_cap));
        }
    }
    for rel_base in
        incoming_relation_nav_bases_to_entity(cgs, ename, map, surface_filter, catalog_entry_id)
    {
        if seen_bases.insert(rel_base.clone()) {
            attempts.push((rel_base, None));
        }
    }
    // Unary identity get is omitted from projection attempts when list/query exists — teach
    // `e#{{…}}[p#,…]` instead of unary `e#(p#)[p#,…]` / `e#($)[p#,…]` (same policy as primary-get emission).
    if query_caps.is_empty() {
        let unary = unary_entity_id_teaching_expr_line(es, ent, map);
        if seen_bases.insert(unary.clone()) {
            attempts.push((unary, primary_get_cap));
        }
    }

    for (base, witness_cap) in attempts {
        let full = format!("{base}{bracket}");
        let work = domain_line_work_string(&full, map);
        let Some(parsed) = domain_line_validate_full(cgs, &work) else {
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
            map,
            &full,
            Some(gloss),
            None,
            None,
            source_cap,
            false,
            line_valid_cache,
        );
    }
    false
}

/// In DOMAIN synthetic lines, bare `$` (and search `~$`) marks a **placeholder** for the real
/// parameter value — use the corresponding `p#` gloss line; it is not a literal value to send to the API.
const DOMAIN_PARAM_VALUE_PLACEHOLDER: &str = "$";

fn truncate_inline_desc(s: &str, max: usize) -> String {
    let t = crate::symbol_tuning::trim_description_for_agent_gloss(s).replace('\t', " ");
    crate::utf8_trunc::truncate_utf8_bytes_with_ellipsis(&t, max)
}

/// Strip authoring noise like ``(constructor `v101`)`` from variant descriptions before DOMAIN Meaning.
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
        Some(h) => format!("relation {h} => {target_gloss}"),
        None => target_gloss,
    }
}

/// Compound `Entity(p#=$,…)` when the target has multiple `key_vars` (per-key placeholders are still the string `$`).
///
/// Unary entity refs use [`unary_entity_id_teaching_expr_line`] / `$` fallback like scalar identity GET teaching.
fn entity_ref_id_example(cgs: &CGS, target: &str, map: Option<&SymbolMap>) -> String {
    let target_sym = ent_sym(map, target);
    let p = DOMAIN_PARAM_VALUE_PLACEHOLDER;
    let Some(ent) = cgs.get_entity(target) else {
        return format!("{target_sym}({})", DOMAIN_PARAM_VALUE_PLACEHOLDER);
    };
    if ent.key_vars.len() > 1 {
        let parts: Vec<String> = ent
            .key_vars
            .iter()
            .map(|kv| format!("{}={}", id_sym_entity(map, target, kv.as_str()), p))
            .collect();
        format!("{}({})", target_sym, parts.join(", "))
    } else {
        unary_entity_id_teaching_expr_line(&target_sym, ent, map)
    }
}

/// One `p#=value` in `Entity{p#=,…}` — same placeholder discipline as [`invoke_dotted_call_arg_example`].
fn query_param_slot_example(
    f: &crate::InputFieldSchema,
    cap: &crate::CapabilitySchema,
    cgs: &CGS,
    map: Option<&SymbolMap>,
) -> String {
    let Ok(nv) = f.named_value(cgs) else {
        let n = id_sym_cap(map, cap, f.name.as_str());
        return format!("{n}={}", DOMAIN_PARAM_VALUE_PLACEHOLDER);
    };
    if matches!(nv.field_type, FieldType::Array) {
        // Array predicates in DOMAIN teaching use bare `$` so query type-check can apply
        // capability-param placeholder relaxation (`field=$`) for list-like filters.
        let n = id_sym_cap(map, cap, f.name.as_str());
        return format!("{n}={}", DOMAIN_PARAM_VALUE_PLACEHOLDER);
    }
    invoke_dotted_call_arg_example(f, cap, cgs, map).unwrap_or_else(|| {
        let n = id_sym_cap(map, cap, f.name.as_str());
        let p = DOMAIN_PARAM_VALUE_PLACEHOLDER;
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
                parts.push(format!("{sym}={}", entity_ref_id_example(cgs, target, map)));
            }
        }
    }
    Some(format!("{es}({})", parts.join(", ")))
}

/// Unary identity GET teaching: [`EntityDef::id_field`] as opaque **`p#`** (`e#(p…)`) when the field has an
/// allocated DOMAIN ident symbol; otherwise **`e#($)`** (canonical / unresolved gloss).
fn unary_entity_id_teaching_expr_line(
    es: &str,
    ent: &EntityDef,
    map: Option<&SymbolMap>,
) -> String {
    let sym = id_sym_entity(map, ent.name.as_str(), ent.id_field.as_str());
    if map.is_some_and(|m| m.resolve_ident(sym.as_str()).is_some()) {
        format!("{es}({sym})")
    } else {
        format!("{es}({})", DOMAIN_PARAM_VALUE_PLACEHOLDER)
    }
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
            Expr::Chain(_) | Expr::Page(_) | Expr::TeachingValue { .. } => DomainLineKind::Other,
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
fn try_push_teaching_example(
    gloss_emit: &mut Option<GlossScratch<'_>>,
    teaching_rows: &mut Vec<EntityTeachingExprRow>,
    collect_meta: bool,
    cgs: &CGS,
    map: Option<&SymbolMap>,
    expr: &str,
    gloss: Option<String>,
    cap_leg: Option<String>,
    relation: Option<&RelationSchema>,
    source_capability: Option<&CapabilityName>,
    // When true: strip [`TeachingExprLine::description`] from capability legend (Query/Get/Search);
    // scope / optional params / compact args remain.
    omit_capability_prose: bool,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, bool>,
) -> bool {
    if let Some(gs) = gloss_emit.as_mut() {
        gs.emit_before_teaching_example(expr, cap_leg.as_deref(), gloss.as_deref());
    }
    let work = domain_line_work_string(expr, map);
    let mut teaching_line =
        teaching_expr_line_from_layers(expr, gloss.as_deref(), cap_leg.as_deref());
    if omit_capability_prose {
        teaching_line.description.clear();
    }
    let dedupe_key = TeachingRowDedupeKey::new(expr, gloss.as_ref(), cap_leg.as_ref());

    if collect_meta {
        let Some(parsed_expr) = domain_line_validate_full(cgs, &work) else {
            return false;
        };
        teaching_rows.push(EntityTeachingExprRow {
            teaching_expr: teaching_line,
            meta: domain_line_execution_meta_from_validated(
                cgs,
                work,
                relation,
                source_capability,
                &parsed_expr.expr,
            ),
            dedupe_key,
        });
        return true;
    }

    let cache_key = domain_line_cache_key(cgs, &work);
    let ok = *line_valid_cache
        .entry(cache_key)
        .or_insert_with(|| domain_line_valid_work(cgs, &work));
    if !ok {
        return false;
    }
    teaching_rows.push(EntityTeachingExprRow {
        teaching_expr: teaching_line,
        meta: DomainLineMeta {
            expression: work,
            kind: DomainLineKind::Other,
            source_capability: None,
            cross_entity: None,
            relation_materialization: None,
        },
        dedupe_key,
    });
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
    const MAX_DESC: usize = 100;
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
/// [`SymbolMap`] is present (TSV DOMAIN); canonical [`RenderMode`] uses wire names.
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
            DOMAIN_PARAM_VALUE_PLACEHOLDER.to_string()
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
                        parts.push(format!("{lhs}={}", DOMAIN_PARAM_VALUE_PLACEHOLDER));
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
        crate::InputType::Union { .. } => DOMAIN_PARAM_VALUE_PLACEHOLDER.to_string(),
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
                parts.push(format!("{lhs}={}", DOMAIN_PARAM_VALUE_PLACEHOLDER));
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
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, bool>,
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
                map,
                &expr_line,
                Some(legend),
                None,
                None,
                Some(&cap.name),
                false,
                line_valid_cache,
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
                map,
                &expr_line,
                Some(legend),
                None,
                None,
                Some(&cap.name),
                false,
                line_valid_cache,
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
) -> Option<String> {
    let n = id_sym_cap(map, cap, f.name.as_str());
    let p = DOMAIN_PARAM_VALUE_PLACEHOLDER;
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
        FieldType::EntityRef { target } => {
            Some(format!("{n}={}", entity_ref_id_example(cgs, target, map)))
        }
        FieldType::Date => match &nv.value_format {
            // Same placeholder as strings — avoid teaching ISO literals in DOMAIN dotted-call invokes.
            Some(ValueWireFormat::Temporal(_)) => Some(format!(
                "{n}={p}",
                n = n,
                p = DOMAIN_PARAM_VALUE_PLACEHOLDER
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
        parts.push(scope_param_slot(f, cap, cgs, map));
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
            parts.push(scope_param_slot(f, cap, cgs, map));
            continue;
        }
        if !field_is_filter_like(f) {
            continue;
        }
        if field_omitted_from_path_inject(ent, cap, f.name.as_str()) {
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
    let suffix = format!(".{ms}({args})");
    let recv = receiver_for_dotted_suffix(es, ent, cgs, map, &suffix)?;
    Some(format!("{recv}{suffix}"))
}

const MAX_MULTI_ARITY_METHOD_LINES: usize = 48;

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
/// Without a surface (`None`), treated as included (legacy full DOMAIN).
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

/// Relation-navigation rows (`… .p#` toward another CGS entity, or declared relation chains) are only
/// taught when the **target** entity name appears in [`ExposureSurface::entities`] for the same
/// `catalog_entry_id`. Without a surface (`None`), navigation is unrestricted (legacy full DOMAIN).
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
fn collect_multi_arity_method_lines(
    cgs: &CGS,
    ename: &str,
    es: &str,
    map: Option<&SymbolMap>,
    surface_filter: Option<&ExposureSurface>,
    catalog_entry_id: &str,
) -> Vec<(CapabilityName, String)> {
    let mut out: Vec<(CapabilityName, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let Some(ent) = cgs.get_entity(ename) else {
        return out;
    };

    for cap in cgs.find_capabilities(ename, CapabilityKind::Action) {
        if !surface_allows_capability(surface_filter, catalog_entry_id, cap) {
            continue;
        }
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
        if !surface_allows_capability(surface_filter, catalog_entry_id, cap) {
            continue;
        }
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
        if !surface_allows_capability(surface_filter, catalog_entry_id, cap) {
            continue;
        }
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
        if !surface_allows_capability(surface_filter, catalog_entry_id, cap) {
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

#[allow(clippy::too_many_arguments)]
fn collect_entity_teaching_block(
    cgs: &CGS,
    ename: &str,
    map: Option<&SymbolMap>,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    collect_meta: bool,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, bool>,
    gloss_emit: &mut Option<GlossScratch<'_>>,
    surface_filter: Option<&ExposureSurface>,
) -> EntityTeachingBlock {
    let mut teaching_rows: Vec<EntityTeachingExprRow> = Vec::new();

    let Some(ent) = cgs.get_entity(ename) else {
        return EntityTeachingBlock {
            heading: TeachingHeading::default(),
            field_gloss_rows: Vec::new(),
            teaching_rows,
        };
    };
    let es = ent_sym(map, ename);
    let catalog_entry_id = cgs.entry_id.as_deref().unwrap_or("");
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
                .map(|k| id_sym_entity(map, ename, k.as_str()))
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
    query_caps.sort_by(|a, b| a.name.cmp(&b.name));
    let query_cap_refs: Vec<&crate::CapabilitySchema> = query_caps.to_vec();

    // Projection witness before other `e#…` lines for this entity (query/get/relation) so the field
    // narrow `[p#,…]` appears first in DOMAIN.
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
        let ms = met_sym(map, ename, &label);
        let expr = format!("{es}.{ms}()");
        let result_gloss = crate::result_gloss::result_gloss_for_capability(cap, cgs, map);
        let cap_leg =
            capability_legend_for_domain(map, cgs, cap, ename, ident_meta, catalog_entry_id);
        try_push_teaching_example(
            gloss_emit,
            &mut teaching_rows,
            collect_meta,
            cgs,
            map,
            &expr,
            result_gloss,
            cap_leg,
            None,
            Some(&cap.name),
            true,
            line_valid_cache,
        );
    }

    let mut emitted_primary_get = false;
    if primary_get_cap.is_some() && !only_singleton_gets {
        let primary_name = primary_get_cap.map(|c| &c.name);
        if let Some(cmp) = compound_get_expr_line(&es, ent, cgs, map) {
            if try_push_teaching_example(
                gloss_emit,
                &mut teaching_rows,
                collect_meta,
                cgs,
                map,
                &cmp,
                get_gloss.clone(),
                None,
                None,
                primary_name,
                true,
                line_valid_cache,
            ) {
                emitted_primary_get = true;
            }
        }
        // Unary identity get only when there is no query surface (compound already attempted above).
        if !emitted_primary_get && query_caps.is_empty() {
            let line_base = unary_entity_id_teaching_expr_line(&es, ent, map);
            if try_push_teaching_example(
                gloss_emit,
                &mut teaching_rows,
                collect_meta,
                cgs,
                map,
                &line_base,
                get_gloss.clone(),
                None,
                None,
                primary_name,
                true,
                line_valid_cache,
            ) {
                emitted_primary_get = true;
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
            if !surface_allows_capability(surface_filter, catalog_entry_id, cap) {
                continue;
            }
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
            let expr = if path_vars_empty(cap) {
                format!("{es}.{ms}()")
            } else {
                let suffix = format!(".{ms}()");
                let Some(recv) = receiver_for_dotted_suffix(&es, ent, cgs, map, &suffix) else {
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
                map,
                &expr,
                result_gloss,
                cap_leg,
                None,
                Some(&cap.name),
                false,
                line_valid_cache,
            );
        }
    }
    for (cap_name, line) in
        collect_multi_arity_method_lines(cgs, ename, &es, map, surface_filter, catalog_entry_id)
    {
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
            map,
            &line,
            gloss,
            cap_leg,
            None,
            Some(&cap_name),
            false,
            line_valid_cache,
        );
    }

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
                capability_legend_for_domain(map, cgs, cap, ename, ident_meta, catalog_entry_id);
            let mut added = false;
            if let Some(line) = query_expr_maximal(cap, &es, cgs, map) {
                if local_seen.insert(line.clone())
                    && try_push_teaching_example(
                        gloss_emit,
                        &mut teaching_rows,
                        collect_meta,
                        cgs,
                        map,
                        &line,
                        qgloss.clone(),
                        cap_leg.clone(),
                        None,
                        Some(&cap.name),
                        true,
                        line_valid_cache,
                    )
                {
                    added = true;
                    query_line_count += 1;
                }
            }
            if !added {
                if let Some(line) = query_expr_scope_only(cap, &es, cgs, map) {
                    if local_seen.insert(line.clone())
                        && try_push_teaching_example(
                            gloss_emit,
                            &mut teaching_rows,
                            collect_meta,
                            cgs,
                            map,
                            &line,
                            qgloss.clone(),
                            cap_leg.clone(),
                            None,
                            Some(&cap.name),
                            true,
                            line_valid_cache,
                        )
                    {
                        added = true;
                        query_line_count += 1;
                    }
                }
            }
            if !added {
                if let Some(line) = query_expr_filters_only(cap, &es, cgs, map) {
                    if local_seen.insert(line.clone())
                        && try_push_teaching_example(
                            gloss_emit,
                            &mut teaching_rows,
                            collect_meta,
                            cgs,
                            map,
                            &line,
                            qgloss.clone(),
                            cap_leg.clone(),
                            None,
                            Some(&cap.name),
                            true,
                            line_valid_cache,
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
        let keyed = unary_entity_id_teaching_expr_line(&es, ent, map);
        let _ = try_push_teaching_example(
            gloss_emit,
            &mut teaching_rows,
            collect_meta,
            cgs,
            map,
            &keyed,
            get_gloss.clone(),
            None,
            None,
            primary_name,
            true,
            line_valid_cache,
        );
    }

    let mut search_caps: Vec<_> = cgs
        .find_capabilities(ename, CapabilityKind::Search)
        .into_iter()
        .filter(|cap| surface_allows_capability(surface_filter, catalog_entry_id, cap))
        .collect();
    if !search_caps.is_empty() {
        let line = format!("{es}~{}", DOMAIN_PARAM_VALUE_PLACEHOLDER);
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
        try_push_teaching_example(
            gloss_emit,
            &mut teaching_rows,
            collect_meta,
            cgs,
            map,
            &line,
            sg,
            cap_leg,
            None,
            scap.map(|c| &c.name),
            true,
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
    const MAX_REL_NAV_LINES: usize = 16;
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
            id_sym_rel(map, ename, rel.as_str())
        } else {
            id_sym_entity(map, ename, rel.as_str())
        };
        let suffix = format!(".{rel_sym}");
        let Some(recv) = receiver_for_dotted_suffix(&es, ent, cgs, map, &suffix) else {
            continue;
        };
        let rel_expr = format!("{recv}{suffix}");
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
            map,
            &rel_expr,
            Some(result_gloss),
            rel_desc_opt,
            rel_for_meta,
            None,
            false,
            line_valid_cache,
        );
    }

    let field_gloss_rows = gloss_emit
        .as_mut()
        .map(|gs| std::mem::take(gs.field_gloss))
        .unwrap_or_default();

    EntityTeachingBlock {
        heading,
        field_gloss_rows,
        teaching_rows,
    }
}

/// Count of synthesized DOMAIN example lines for an entity (same pipeline as emission).
#[cfg(test)]
pub(crate) fn domain_example_line_count(cgs: &CGS, ename: &str, map: Option<&SymbolMap>) -> usize {
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    collect_entity_teaching_block(
        cgs,
        ename,
        map,
        None,
        false,
        &mut line_valid_cache,
        &mut gloss_emit_none,
        None,
    )
    .teaching_rows
    .len()
}

/// Raw DOMAIN lines for an entity (for per-capability witness checks).
#[cfg(test)]
pub(crate) fn domain_example_lines(
    cgs: &CGS,
    ename: &str,
    map: Option<&SymbolMap>,
    surface_filter: Option<&ExposureSurface>,
) -> Vec<String> {
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    collect_entity_teaching_block(
        cgs,
        ename,
        map,
        None,
        false,
        &mut line_valid_cache,
        &mut gloss_emit_none,
        surface_filter,
    )
    .teaching_rows
    .into_iter()
    .map(|r| r.teaching_expr.expression.clone())
    .collect()
}

/// Primary-get projection bracket for the DOMAIN entity heading (when enabled); test-only helper.
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
    let block = collect_entity_teaching_block(
        cgs,
        ename,
        map,
        None,
        false,
        &mut line_valid_cache,
        &mut gloss_emit_none,
        surface_filter,
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
/// [`render_domain_prompt_bundle_for_exposure`] path as production prompts (opaque alias rewrite applied).
#[cfg(test)]
fn domain_projection_bracket_from_final_bundle(
    cgs: &CGS,
    exposure: &crate::symbol_tuning::DomainExposureSession,
    config: RenderConfig<'_>,
    ename: &str,
) -> Option<String> {
    let bundle = render_domain_prompt_bundle_for_exposure(cgs, config, exposure, None);
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
    "Follow the grammar and the teaching TSV below; reply with one valid plasm_program:";

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
                let Ok(nv) = f.named_value(cgs) else {
                    continue;
                };
                if matches!(nv.field_type, FieldType::Blob) {
                    return true;
                }
                if matches!(nv.field_type, FieldType::String)
                    && f.effective_string_semantics(cgs)
                        .is_structured_or_multiline()
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
            let Ok(nv) = f.named_value(cgs) else {
                continue;
            };
            if matches!(nv.field_type, FieldType::Blob) {
                return true;
            }
            if matches!(nv.field_type, FieldType::String)
                && f.effective_string_semantics(cgs)
                    .is_structured_or_multiline()
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

fn render_prompt_contract(spec: PromptContractSpec) -> String {
    render_prompt_contract_dense(spec)
}

fn render_prompt_contract_dense(spec: PromptContractSpec) -> String {
    let entity = symbolic_entity_form(spec.symbolic);
    let projection = if spec.symbolic {
        "[p#,…]"
    } else {
        "[field,…]"
    };
    let get_form = if spec.symbolic {
        "e#(<id>) or e#(p#=<value>, p#=<value>)"
    } else {
        "Entity(<id>) or Entity(name=<value>, name=<value>)"
    };
    let query_form = if spec.symbolic {
        "e#{p#=<value>, …}"
    } else {
        "Entity{name=<value>, …}"
    };
    let query_all_form = entity;
    let nav_form = if spec.symbolic {
        "<receiver>.p#"
    } else {
        "<receiver>.field"
    };
    let method_form = if spec.symbolic {
        "e#(<id>).m#() or e#(<id>).m#(p#=<value>, …)"
    } else {
        "Entity(<id>).method() or Entity(<id>).method(name=<value>, …)"
    };
    let create_form = if spec.symbolic {
        "e#.m#(p#=<value>, …)"
    } else {
        "Entity.method(name=<value>, …)"
    };
    let projection_form = if spec.symbolic {
        "e#(<id>)[p#,…]"
    } else {
        "Entity(<id>)[field,…]"
    };
    let scoped_form = if spec.symbolic {
        "e#{p#=e#(<id>)}"
    } else {
        "Entity{scope_param=AnchorEntity(<id>)}"
    };
    let array_form = if spec.symbolic {
        "p#=[e#(<id>), …]"
    } else {
        "name=[Entity(<id>), …]"
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
        "- Emit only code: either one `plasm_expr`, or one multi-line `plasm_program` with bindings then final roots.\n\
- Do not emit prose, JSON wrappers, `return`, Markdown fences, or table rows.\n\
- Prefer bind → narrow/project/transform → few final roots when more than one step is needed.\n\n",
    );

    s.push_str("TSV table semantics:\n");
    s.push_str(
        "- Header is exactly `plasm_expr<TAB>Meaning`; every following row has exactly one tab delimiter.\n\
- Left cell is either executable syntax or metadata. Right cell is selection guidance only; never copy `Meaning` into output.\n\
- Executable rows start with an entity surface (`e#`/`Entity`). Copy their left-cell shape, then replace placeholders.\n\
- Metadata rows whose left cell is only `v#`/`value domain` define reusable value types. Metadata rows whose left cell is only `p#`/`field` define keyed slots. Metadata rows are never executable roots.\n\
- `Meaning` fragments joined by ` · ` stay inside `Meaning`; they are not operators.\n\n",
    );

    s.push_str("Symbol and fill rules:\n");
    if spec.symbolic {
        s.push_str(
            "- `e#` = entity surface; `m#` = method/action surface; `p#` = keyed field/parameter/relation slot; `v#` = value-domain metadata only.\n\
- Entity-ref slots in `Meaning` look like `ref:Zone · str · Zone identifier`: canonical entity, id wire type, short note — not `plasm_expr` syntax.\n\
- Never write `v#` inside a `plasm_expr`. Use `p#` keys in code and use `v#` rows only to understand allowed values/types.\n\
- Unary identity rows often teach `e#(p#)` using the opaque `p#` for the entity `id_field` (same token as gloss); substitute the real wire id — do not treat it as a literal API value.\n\
- `$` appears only in taught examples when no opaque id slot is shown. Replace every `$`; never emit `$`.\n\
- `<id>`, `<value>`, `<receiver>`, and `elem` in this contract are meta-variables, not syntax tokens.\n\
- If a copied row contains `..`, it is an ellipsis for omitted optional keys. Remove `..` or replace it with additional `p#=<value>` assignments before final output.\n\
- If `Meaning` says `optional params: pA, pB`, those keys may be added only as keyed assignments with real values.\n",
        );
    } else {
        s.push_str(
            "- Entity names, method names, and field names are literal code tokens when they appear in executable left cells.\n\
- `$`, `<id>`, `<value>`, `<receiver>`, and `elem` are fill/meta placeholders; replace them before final output.\n\
- If a copied row contains `..`, remove it or replace it with additional keyed assignments before final output.\n",
        );
    }
    let _ = writeln!(
        s,
        "- Projection rows ending `{projection}` teach a valid field set. Reuse that suffix only on another expression returning the same entity or list type.",
        projection = projection
    );
    s.push_str("- Relation rows end with `.p#`/`.field`; apply them to any executable receiver row for that same entity type.\n");
    s.push_str("- `page(sN_pgM)` uses a continuation handle returned by a prior response; copy the handle exactly and optionally add `, limit=N`.\n\n");

    s.push_str("Grammar:\n");
    let _ = writeln!(s, "plasm_program ::= plasm_expr | binding+ roots");
    let _ = writeln!(s, "binding       ::= ident \"=\" node");
    let _ = writeln!(s, "roots         ::= root (\",\" root)*");
    let _ = writeln!(s, "root          ::= ident | plasm_expr");
    let _ = writeln!(
        s,
        "node          ::= (plasm_expr | ident) postfix* row_template? | ident \"=>\" value_or_template"
    );
    let _ = writeln!(s, "plasm_expr    ::= entity_expr [projection] | page");
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
    let _ = writeln!(s, "page          ::= page(sN_pgM) | page(sN_pgM, limit=N)");
    let _ = writeln!(
        s,
        "projection    ::= {} | \"[\" fields \"]\"",
        projection_form
    );
    let _ = writeln!(
        s,
        "postfix       ::= \".limit(N)\" | \".page_size(N)\" | \".sort(field[, dir])\" | \".aggregate(specs)\" | \".group_by(field, specs)\" | \".singleton()\" | \"[\" fields \"]\""
    );
    let _ = writeln!(
        s,
        "row_template  ::= (\"[\" fields \"]\")? \"<<TAG\" template_body \"TAG\""
    );
    let _ = writeln!(s, "fields        ::= {}", projection);
    let _ = writeln!(
        s,
        "specs         ::= name=count | name=sum(field) | name=avg(field) | name=min(field) | name=max(field) [, ...]"
    );
    let _ = writeln!(s, "dir           ::= desc | asc | descending | ascending");
    let _ = writeln!(
        s,
        "value_or_template ::= literal | ident | ident.field | _.field | [elem, ...] | heredoc"
    );
    let _ = writeln!(s, "literal       ::= quoted string | number | bool | null");
    s.push('\n');

    s.push_str("Composition rules:\n");
    s.push_str(
        "- Multi-line program strings use literal newlines: one binding per line, final roots last. Spaces never separate statements.\n\
- Postfix transforms and `[fields]` may chain on any bound node or expression that returns rows.\n\
- To turn rows into text, bind a template block: `report = rows[p#,…] <<TAG` newline template newline `TAG`, or `report = rows <<TAG` when columns can be inferred.\n\
- Template blocks use Minijinja with `rows` as the input array. The bound result is a row with a `content` field; pass `report.content` to string/body parameters or `=>` payloads.\n\
- Do not use `report.content` as a final root or relation receiver. Return `report` if you want the generated text row; continue relations only from row-producing query/relation/projection bindings.\n\
- Heredoc opener `<<TAG` is followed by newline; the first later line whose trimmed text is `TAG` closes it. Choose a tag not present in the body.\n",
    );
    let _ = writeln!(
        s,
        "- Examples: scoped child list `{scoped_form}`; array argument `{array_form}`. Quoted strings use only `\\\"` and `\\\\` escapes.",
        scoped_form = scoped_form,
        array_form = array_form
    );
    if spec.include_rich_string_guidance {
        s.push_str(&render_rich_string_guidance_tsv());
    }
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

/// True when `sym` is the **terminal** relation segment (`… .p#`) and the teaching row already carries
/// a result gloss — a standalone `p#` gloss row would duplicate relation target typing.
fn skip_redundant_terminal_relation_sym_gloss(
    expr: &str,
    sym: &str,
    meta: &crate::symbol_tuning::IdentMetadata,
    result_gloss: Option<&str>,
) -> bool {
    let relation_like = matches!(meta, crate::symbol_tuning::IdentMetadata::Relation { .. });
    if !relation_like {
        return false;
    }
    if !matches!(result_gloss, Some(g) if !g.trim().is_empty()) {
        return false;
    }
    let expr = crate::symbol_tuning::strip_prompt_expression_annotations(expr.trim());
    let expr = expr.trim_end();
    let Some((_, last_seg)) = expr.rsplit_once('.') else {
        return false;
    };
    last_seg == sym
}

/// Returns [`None`] when `sym` is a synonym for an earlier opaque symbol with the same `meaning`:
/// caller skips emitting a duplicate gloss row. Otherwise returns the canonical symbol for this meaning.
fn meaning_canonical_sym_for_emit(
    meaning: &str,
    sym: &str,
    meaning_to_canonical: &mut HashMap<String, String>,
    sym_alias: &mut HashMap<String, String>,
) -> Option<String> {
    match meaning_to_canonical.entry(meaning.to_string()) {
        Entry::Occupied(e) => {
            let canonical = e.get().clone();
            if canonical == sym {
                Some(canonical)
            } else {
                sym_alias.insert(sym.to_string(), canonical);
                None
            }
        }
        Entry::Vacant(v) => {
            v.insert(sym.to_string());
            Some(sym.to_string())
        }
    }
}

fn merge_opaque_alias_maps(
    p: &HashMap<String, String>,
    v: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut rep = p.clone();
    for (k, val) in v {
        if let Some(existing) = rep.get(k) {
            debug_assert_eq!(
                existing, val,
                "opaque alias collision for key {k:?}: p-map vs v-map disagree"
            );
        }
        rep.insert(k.clone(), val.clone());
    }
    rep
}

fn teaching_expr_line_fingerprint(row: &TeachingExprLine) -> String {
    format!(
        "{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
        row.expression,
        row.result_type,
        row.scope,
        row.optional_params,
        row.compact_args,
        row.description,
        row.is_projection_teaching as u8,
    )
}

fn rewrite_teaching_expr_line_opaque_tokens(
    row: &mut TeachingExprLine,
    rep: &HashMap<String, String>,
) {
    row.expression = crate::symbol_tuning::rewrite_opaque_ident_tokens(&row.expression, rep);
    row.result_type = crate::symbol_tuning::rewrite_opaque_ident_tokens(&row.result_type, rep);
    row.scope = crate::symbol_tuning::rewrite_opaque_ident_tokens(&row.scope, rep);
    row.optional_params =
        crate::symbol_tuning::rewrite_opaque_ident_tokens(&row.optional_params, rep);
    row.compact_args = crate::symbol_tuning::rewrite_opaque_ident_tokens(&row.compact_args, rep);
    row.description = crate::symbol_tuning::rewrite_opaque_ident_tokens(&row.description, rep);
}

fn rewrite_field_gloss_opaque_tokens(g: &mut TeachingFieldGloss, rep: &HashMap<String, String>) {
    g.symbol = crate::symbol_tuning::rewrite_opaque_ident_tokens(&g.symbol, rep);
    g.field_type = crate::symbol_tuning::rewrite_opaque_ident_tokens(&g.field_type, rep);
    g.allowed_values = crate::symbol_tuning::rewrite_opaque_ident_tokens(&g.allowed_values, rep);
    g.description = crate::symbol_tuning::rewrite_opaque_ident_tokens(&g.description, rep);
}

/// Tracks `p#` / `v#` gloss lines emitted before DOMAIN example rows (first-use only).
struct FieldGlossEmitState {
    /// Registry-backed opaque `p#`: compact `v# · wire` teaching string already emitted for this symbol.
    /// Slots that share one `p#` but differ in point-of-use description keep distinct strings and may re-teach.
    registry_p_slot_compact_gloss: HashMap<String, String>,
    /// Composite slot key (compact Meaning + catalog + entity + wire/param path, after `v#` normalization)
    /// → first opaque `p#` emitted for that teaching slot.
    registry_compact_meaning_canonical_p: HashMap<String, String>,
    /// Synonym `p#` → canonical `p#` when compact Meaning matches an earlier slot.
    registry_p_sym_alias: HashMap<String, String>,
    /// Full value-domain gloss body → first canonical `v#` for that Meaning.
    registry_value_gloss_canonical_v: HashMap<String, String>,
    /// Synonym `v#` → canonical `v#` when gloss body matches an earlier row.
    registry_v_sym_alias: HashMap<String, String>,
    /// Relation edges and non-value-domain fallbacks: retain full metadata inequality for re-teach decisions.
    non_registry_slots: HashMap<String, IdentMetadata>,
    defined_value_domains: HashSet<String>,
}

/// Per-entity field gloss rows built directly into [`TeachingFieldGloss`] (no compact transcript).
struct GlossScratch<'a> {
    field_gloss: &'a mut Vec<TeachingFieldGloss>,
    state: &'a mut FieldGlossEmitState,
    map: &'a SymbolMap,
    meta: &'a HashMap<IdentMetaKey, IdentMetadata>,
    catalog_entry_id: &'a str,
    entity: &'a str,
    cgs: &'a CGS,
}

impl GlossScratch<'_> {
    fn emit_before_teaching_example(
        &mut self,
        expr: &str,
        cap_legend: Option<&str>,
        result_gloss: Option<&str>,
    ) {
        emit_field_def_lines_before_example(
            self.field_gloss,
            expr,
            cap_legend,
            result_gloss,
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
fn emit_field_def_lines_before_example(
    out: &mut Vec<TeachingFieldGloss>,
    expr: &str,
    cap_legend: Option<&str>,
    result_gloss: Option<&str>,
    map: &SymbolMap,
    entity: &str,
    catalog_entry_id: &str,
    ident_meta: &HashMap<IdentMetaKey, IdentMetadata>,
    state: &mut FieldGlossEmitState,
    cgs: &CGS,
) {
    let en = EntityName::from(entity.to_string());
    let cid = catalog_entry_id.to_string();
    for sym in crate::symbol_tuning::field_syms_for_teaching_row(expr, result_gloss, cap_legend) {
        let field_name = map.resolve_ident(&sym).unwrap_or(&sym);
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
                if let Some(vg) = map.value_domain_gloss_for_v_sym(vs) {
                    if let Some(v_canon) = meaning_canonical_sym_for_emit(
                        vg,
                        vs,
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
                    let path_key = map
                        .capability_param_quad_for_p_sym(sym.as_str())
                        .map(|(_, _, _, path)| path)
                        .unwrap_or_else(|| m.wire_name().to_string());
                    let p_meaning_key = format!(
                        "{}\x1f{}\x1f{}\x1f{}",
                        compact,
                        m.catalog_entry_id(),
                        m.entity().as_str(),
                        path_key
                    );
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
            if let Some(m) = &meta {
                if skip_redundant_terminal_relation_sym_gloss(expr, sym.as_str(), m, result_gloss) {
                    state.non_registry_slots.remove(&sym);
                    continue;
                }
            }
            let gloss = match meta {
                Some(m) => m.render_gloss_with_cgs(Some(map), Some(cgs)),
                None => field_name.to_string(),
            };
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
fn render_domain_table_resolved<'b, F>(
    mut resolve: F,
    full_entities: &[&str],
    map: Option<&SymbolMap>,
    exposure_for_ident: Option<&DomainExposureSession>,
    teaching_blocks_out: &mut Vec<EntityTeachingBlock>,
    model_out: &mut Vec<EntityDomainPrompt>,
    fill_model: bool,
    _include_contract_preamble: bool,
    emit_entity_blocks: Option<&[&str]>,
) where
    F: FnMut(&str) -> &'b CGS,
{
    let surface_filter = exposure_for_ident.map(|e| &e.surface);
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

    let mut gloss_emit_state = FieldGlossEmitState {
        registry_p_slot_compact_gloss: HashMap::new(),
        registry_compact_meaning_canonical_p: HashMap::new(),
        registry_p_sym_alias: HashMap::new(),
        registry_value_gloss_canonical_v: HashMap::new(),
        registry_v_sym_alias: HashMap::new(),
        non_registry_slots: HashMap::new(),
        defined_value_domains: HashSet::new(),
    };
    let mut line_valid_cache: HashMap<DomainLineValidCacheKey, bool> = HashMap::with_capacity(8192);

    let block_iter: Vec<&str> = if let Some(e) = emit_entity_blocks {
        e.to_vec()
    } else {
        full_entities.to_vec()
    };

    for &ename in &block_iter {
        let cgs = resolve(ename);
        let collect_meta = fill_model;
        let mut field_gloss_accum = Vec::new();
        let mut gloss_emit: Option<GlossScratch<'_>> = match (map, ident_meta.as_ref()) {
            (Some(m), Some(meta)) => Some(GlossScratch {
                field_gloss: &mut field_gloss_accum,
                state: &mut gloss_emit_state,
                map: m,
                meta,
                catalog_entry_id: cgs.entry_id.as_deref().unwrap_or(""),
                entity: ename,
                cgs,
            }),
            _ => None,
        };
        let block = collect_entity_teaching_block(
            cgs,
            ename,
            map,
            ident_meta.as_ref(),
            collect_meta,
            &mut line_valid_cache,
            &mut gloss_emit,
            surface_filter,
        );
        if block.teaching_rows.is_empty() {
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
        let mut seen_expr: HashSet<TeachingRowDedupeKey> = HashSet::new();
        let mut emitted_metas: Vec<DomainLineMeta> = Vec::new();
        let mut kept_rows: Vec<EntityTeachingExprRow> = Vec::new();
        for row in block.teaching_rows {
            if seen_expr.insert(row.dedupe_key.clone()) {
                if collect_meta {
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
        if fill_model {
            model_out.push(EntityDomainPrompt {
                entity: ename.to_string(),
                lines: emitted_metas,
            });
        }
    }

    let rep = merge_opaque_alias_maps(
        &gloss_emit_state.registry_p_sym_alias,
        &gloss_emit_state.registry_v_sym_alias,
    );
    if !rep.is_empty() {
        if fill_model {
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
                    let fp = teaching_expr_line_fingerprint(&row.teaching_expr);
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
                    let fp = teaching_expr_line_fingerprint(&row.teaching_expr);
                    seen.insert(fp)
                });
            }
        }
    }
}

/// Per-entity many-shot examples using a single [`CGS`].
#[allow(clippy::too_many_arguments)]
fn render_domain_table(
    cgs: &CGS,
    full_entities: &[&str],
    map: Option<&SymbolMap>,
    teaching_blocks_out: &mut Vec<EntityTeachingBlock>,
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
        teaching_blocks_out,
        model_out,
        fill_model,
        include_contract_preamble,
        emit_entity_blocks,
    );
}

/// `p#  ;;  …` field gloss lines (not Plasm expressions).
#[cfg(test)]
fn is_field_gloss_line(trimmed: &str) -> bool {
    let t = trimmed.trim_start();
    let rest = if let Some(r) = t.strip_prefix('p') {
        r
    } else if let Some(r) = t.strip_prefix('v') {
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
    let rest = if let Some(r) = s.strip_prefix('p') {
        r
    } else if let Some(r) = s.strip_prefix('v') {
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
        CapabilityMapping, CapabilitySchema, FieldSchema, FieldValueKind, NamedValueSchema,
        RelationSchema, ResourceSchema, ValueDomainKey,
    };
    use crate::symbol_tuning::{
        entity_slices_for_render, resolve_prompt_surface_entities, symbol_map_for_prompt,
        DomainExposureSession, ExposureEntityKey, FocusSpec,
    };
    use crate::CapabilityKind;
    use crate::Cardinality;
    use crate::EntityName;
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

    /// Locks Proof `Document`-focused symbolic DOMAIN TSV (`apis/proof`): union ctor teaching rows,
    /// value-domain gloss, and `document_edit_v2` witness line. Update with
    /// `INSTA_UPDATE=1 cargo test -p plasm-core proof_document_domain_tsv_snapshot`.
    #[test]
    fn proof_document_domain_tsv_snapshot() {
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
            insta::assert_snapshot!("proof_document_domain_tsv", tsv);
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
                "capability `blocks` ctor params must not reuse relation-style `=> Block` gloss (symbol {sym}); relation nav stays on `e1($).p6`-style rows.\n{tsv}"
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
                "{cap_name}: expected at least one required parameter so DOMAIN cannot teach a no-arg bug report"
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
        let contract = render_plasm_mcp_language_frontmatter();
        assert!(
            contract.contains("TSV table semantics:"),
            "contract should teach TSV interpretation before catalog rows"
        );
        assert!(
            contract.contains("p#=<value>"),
            "symbolic value placeholders must use <value>, not bare v"
        );
        assert!(
            contract.contains("page(sN_pgM)"),
            "page continuation handles are taught by responses and must remain in the contract"
        );
        assert!(
            !contract.contains("DOMAIN") && !contract.contains(";;") && !contract.contains("p#=v"),
            "contract must not reintroduce legacy DOMAIN/compact separators or bare-v placeholders:\n{contract}"
        );
    }

    #[test]
    fn redundant_relation_sym_gloss_skipped_for_terminal_chain_line() {
        use crate::symbol_tuning::IdentMetadata;
        use crate::EntityName;
        let user = EntityName::from("User".to_string());
        let issue = EntityName::from("Issue".to_string());
        let rel_meta = IdentMetadata::Relation {
            catalog_entry_id: String::new(),
            entity: issue.clone(),
            wire_name: "reporter".into(),
            description: String::new(),
            target: user.clone(),
        };
        assert!(skip_redundant_terminal_relation_sym_gloss(
            "e5(p64=$, p80=$, p59=$).p101",
            "p101",
            &rel_meta,
            Some("e18"),
        ));
        assert!(!skip_redundant_terminal_relation_sym_gloss(
            "e5{p101=$, p64=$, p80=$}",
            "p101",
            &rel_meta,
            Some("[e5]"),
        ));
        let title_meta = IdentMetadata::SyntheticUnknown {
            catalog_entry_id: String::new(),
            entity: issue,
            wire_name: "title".into(),
            description: String::new(),
        };
        assert!(!skip_redundant_terminal_relation_sym_gloss(
            "e5(p64=$, p80=$, p59=$)[p96]",
            "p96",
            &title_meta,
            Some("[p96]"),
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
        let exposure =
            crate::symbol_tuning::domain_exposure_session_from_focus(&cgs, FocusSpec::All);
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
        let lines = domain_example_lines(&cgs, "Issue", map.as_ref(), surface);
        let bracket_lines = lines
            .iter()
            .filter(|l| l.contains("[p") && l.contains(']'))
            .count();
        assert_eq!(
            bracket_lines, 1,
            "expect exactly one DOMAIN example line with a full scalar projection list (bracket_lines={})",
            bracket_lines,
        );
        let out = render_prompt_with_config(&cgs, cfg);
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
        let exposure =
            crate::symbol_tuning::domain_exposure_session_from_focus(&cgs, FocusSpec::All);
        let surface = Some(&exposure.surface);
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true);
        let cfg = RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact);
        let br = domain_projection_bracket_from_final_bundle(&cgs, &exposure, cfg, "Issue")
            .expect("Linear Issue should carry a full projection bracket (heading or primary get)");
        assert!(
            br.starts_with('[') && br.contains('p'),
            "unexpected projection bracket: {br}"
        );
        let lines = domain_example_lines(&cgs, "Issue", map.as_ref(), surface);
        let bracket_lines = lines
            .iter()
            .filter(|l| l.contains("[p") && l.contains(']'))
            .count();
        assert_eq!(
            bracket_lines, 1,
            "expect exactly one DOMAIN example line with a full scalar projection list (bracket_lines={})",
            bracket_lines,
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
            crate::symbol_tuning::domain_exposure_session_from_focus(&cgs, FocusSpec::All);
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

    #[test]
    fn rendered_domain_tsv_teaching_rows_single_tab_separator() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let (_, body) = split_tsv_domain_contract_and_table(&tsv);
        validate_domain_tsv_teaching_table(&body)
            .expect("every teaching row must be expr\\tMeaning");
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

    /// `Profile.recorded_matches` targets `RecordedContent`, which has Search/Query but no Get — DOMAIN
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
            tsv.lines().any(|l| {
                l.contains("Content scoped to this profile") && l.contains(".p") && l.contains("e7")
            }),
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
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
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
                cols.len() == 2
                    && cols[0].starts_with("e5(")
                    && !cols[0].contains('[')
                    && cols[1].starts_with("returns e5")
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
            .skip_while(|line| *line != TSV_DOMAIN_TABLE_HEADER.trim_end())
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
            .expect("IssueComment invoke DOMAIN row");
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
                    && parse_trailing_projection_bracket(cols[0].trim()).is_none()
                    && cols[0].starts_with(&format!("{contrib_ent}{{"))
                    && cols[0].contains(&format!("{p_repo}="))
                    && cols[0].contains(&format!("{p_anon}="))
                    && (cols[1].contains("optional params:") || cols[1].contains("[scope"))
            })
            .expect("Contributor list DOMAIN row (non-projection query exemplar)");
        assert!(
            contrib.starts_with('e') && contrib.contains("{p"),
            "contributor query row should be a brace-query exemplar: {contrib:?}"
        );
        assert!(
            !contrib.contains("args:"),
            "capability legends omit inline `args:`; contributor row was: {contrib:?}"
        );
        assert!(
            contrib.contains("optional params:") || contrib.contains("[scope"),
            "contributor query Meaning should carry optionality or scope context: {contrib:?}"
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
        let Some(idx) = prompt.find(TSV_DOMAIN_TABLE_HEADER) else {
            panic!(
                "expected {} in rendered prompt",
                TSV_DOMAIN_TABLE_HEADER.trim_end()
            );
        };
        let table = &prompt[idx..];
        validate_domain_tsv_teaching_table(table).expect("TSV teaching invariant");
        for line in table.lines().skip(1) {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            assert!(
                !line.contains(";;"),
                "direct TSV emission must not leak compact DOMAIN transcript tokens: {line:?}"
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
        let lines = domain_example_lines(&cgs, "Zone", Some(&map), None);
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
            &mut gloss_emit_none,
            None,
        );
        let witness_row = block.teaching_rows.iter().find(|r| {
            r.teaching_expr.is_projection_teaching
                && parse_trailing_projection_bracket(r.teaching_expr.expression.trim()).is_some()
        });
        let Some(row) = witness_row else {
            panic!("expected a projection witness row for Zone DOMAIN; lines={lines:?}");
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
        let work = domain_line_work_string(expr, Some(&map));
        assert!(
            domain_line_valid_work(&cgs, &work),
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
        let contract = render_plasm_mcp_language_frontmatter();
        assert!(
            contract.contains("ref:Zone") && contract.contains("str · Zone identifier"),
            "contract must teach entity-ref Meaning shape with canonical entity (not e#):\n{contract}"
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
            &["Ruleset".to_string()],
            &["Ruleset".to_string()],
            None,
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
            &mut gloss_emit_none,
            Some(&delta.required),
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
            &mut gloss_emit_none2,
            Some(&surface_with_zone),
        );
        assert!(
            block2.teaching_rows.iter().any(|r| {
                let ex = r.teaching_expr.expression.as_str();
                ex.contains('.') && ex.contains(zone_nav_sym.as_str())
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
        let unfiltered = super::incoming_relation_nav_bases_to_entity(
            &cgs,
            "Ruleset",
            Some(&map),
            None,
            entry.as_str(),
        );
        assert!(
            unfiltered.iter().any(|line| line.contains(zone_es.as_str())),
            "without surface filter expect Zone-anchored incoming bases toward Ruleset; got {unfiltered:?}"
        );

        let delta = crate::discovery::derive_intent_exposure_surface_batch(
            &cgs,
            entry.as_str(),
            "rules traffic handling Cloudflare zone firewall WAF",
            &["Ruleset".to_string()],
            &["Ruleset".to_string()],
            None,
        );
        let filtered = super::incoming_relation_nav_bases_to_entity(
            &cgs,
            "Ruleset",
            Some(&map),
            Some(&delta.required),
            entry.as_str(),
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
            &mut gloss_emit_none,
            None,
        );
        let witness_row = block.teaching_rows.iter().find(|r| {
            r.teaching_expr.is_projection_teaching
                && parse_trailing_projection_bracket(r.teaching_expr.expression.trim()).is_some()
        });
        let Some(row) = witness_row else {
            panic!("expected a projection witness row for Zone DOMAIN");
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
            "DOMAIN row must use exactly one U+0009 column delimiter; line={line:?}"
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
        let bundle = render_domain_prompt_bundle(&cgs, RenderConfig::for_eval(None));
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
            &mut gloss_emit_none,
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
            // Abstract entities are omitted from default DOMAIN slices — explicit teaching
            // collection still synthesizes witness rows for tooling/tests.
            let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
            assert!(
                !prompt.lines().any(|l| {
                    !l.starts_with('#')
                        && !l.is_empty()
                        && l.split_once('\t').is_some_and(|(e, _)| e == expr)
                }),
                "abstract WafPackage lines must not appear in default DOMAIN TSV: {expr:?}"
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
    fn prompt_matrix_duplicate_registry_p_slot_gloss_suppressed() {
        let dir = fixtures_schemas_dir("plasm_prompt_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let Some(idx) = prompt.find(TSV_DOMAIN_TABLE_HEADER) else {
            panic!("expected DOMAIN TSV header");
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
            "Space scoped query with team_id should remain in DOMAIN (canonical)"
        );
        assert!(
            sym.contains("Space{")
                || (sym.contains("{p") && sym.contains(&format!("={}(", team_sym)))
                || raw.contains("Space{"),
            "Space scoped query should remain in DOMAIN"
        );
    }

    /// `team_query` is query-shaped (`e1` in DOMAIN); capability prose is intentionally omitted from
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
                line.split_once('\t').is_some_and(|(expr, meaning)| {
                    expr.starts_with(team_sym.as_str())
                        && meaning.contains("returns")
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
            "query DOMAIN brace form must not teach concrete ISO datetimes or `>=` date literals"
        );
        assert!(
            !domain_block.contains("List all accessible workspaces"),
            "query capability long-form description must not surface in TSV Meaning"
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
                invoke_preflight: None,
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
                invoke_preflight: None,
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
            count, 1,
            "expected one p# gloss row when teaching strings match across entities; domain excerpt:\n{domain}"
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
