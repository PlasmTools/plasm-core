//! Prompt render configuration and teaching-table model types.

use crate::schema::RelationMaterialization;
use crate::symbol_tuning::{FocusSpec, SymbolMapCrossRequestCache, TeachingExposureSession};
use serde::{Deserialize, Serialize};

use super::gloss_dedup::{FieldGlossMeaning, GlossEmitIdentity};
use super::input_legend::TeachingExprLine;

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
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachingHeading {
    /// Human prose merged into TSV identity Meaning for this entity block (typically the CGS entity `description`).
    /// Projection bracket for the heading is inferred from teaching rows, not from this string.
    pub description: String,
}

impl TeachingHeading {
    pub(crate) fn from_entity_banner_description(desc: Option<&str>) -> Self {
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
    /// Canonical typed gloss semantics — source of truth for TSV Meaning projection.
    #[serde(skip, default = "TeachingFieldGloss::default_meaning")]
    pub meaning: FieldGlossMeaning,
    #[serde(skip, default)]
    pub(crate) catalog_entry_id: String,
    #[serde(skip, default)]
    pub(crate) entity: String,
    #[serde(skip, default)]
    pub(crate) emit_identity: Option<GlossEmitIdentity>,
}

impl TeachingFieldGloss {
    fn default_meaning() -> FieldGlossMeaning {
        FieldGlossMeaning::OpaqueLegend {
            description: String::new(),
        }
    }
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
    pub(crate) dedupe_key: TeachingRowDedupeKey,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub(crate) struct TeachingRowDedupeKey {
    expr: String,
    gloss: Option<String>,
    cap: Option<String>,
}

impl TeachingRowDedupeKey {
    pub(crate) fn new(expr: &str, gloss: Option<&String>, cap: Option<&String>) -> Self {
        Self {
            expr: expr.trim().to_string(),
            gloss: gloss
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            cap: cap.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        }
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
