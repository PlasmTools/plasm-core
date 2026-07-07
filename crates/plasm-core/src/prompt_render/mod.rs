//! CGS prompt renderer — TSV **Plasm** many-shot examples: each teaching row is `plasm_expr`, **one tab (U+0009)**,
//! then `Meaning` (middle-dot ` · ` joins gloss **inside** Meaning only). Synthesis builds structured
//! [`EntityTeachingBlock`] rows and emits TSV directly ([`render_prompt_tsv_from_bundle`]); synthesis stays structured
//! (model → [`TeachingExprLine`] / [`TeachingFieldGloss`]) without re-parsing a compact teaching transcript in production.
//! Symbolic prompts use `p#` / `v#` glosses emitted before first use (`v#` = shared `values:` domain;
//! each distinct registry-backed wire slot teaches **`wire` tab `v#`** in Meaning (optional param doc after `v#` when distinct from the `values:` row); full typing on the `v#` row only).
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
//! taught **once** on the witness row; query/search row-producer lines omit the same bracket; Meaning
//! carries typed `inputs:` only (no redundant `rows:` when expr already has `[...]`). Divergent capability
//! `provides` attach bracket on expr only. Value domain once per `v#`, then each distinct
//! wire→`v#` link row; param-specific prose only when it differs from the shared `values:` description.
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

use std::collections::HashMap;

use crate::symbol_tuning::SymbolMap;
use crate::CGS;

mod bundle_render;
mod capability_delta;
mod contract;
mod entity_block;
mod gloss_collect;
mod gloss_dedup;
mod gloss_filter;
mod input_legend;
mod internal;
mod invoke_teaching;
mod line_validate;
mod mcp_prompt_fragments;
mod mcp_tool_descriptions;
mod query_teaching;
mod relation_teaching;
mod row_producer;
mod row_producer_teaching;
mod stats;
mod surface_filter;
mod symbol_tokens;
mod teaching_gloss_emit;
mod teaching_legend;
mod teaching_push;
mod teaching_util;
mod tsv_emit;
mod types;

#[cfg(test)]
mod query_teaching_tests;

pub use bundle_render::{
    render_prompt_tsv_with_config, render_prompt_with_config, render_teaching_bundle,
    render_teaching_prompt_bundle, render_teaching_prompt_bundle_for_exposure,
    render_teaching_prompt_bundle_for_exposure_federated, render_teaching_tsv,
};
pub use input_legend::{
    CapabilityInputLegend, RowContractLegend, RowProjectionContract, TeachingExprLine,
};
pub use types::{
    CrossEntityPlanMeta, CrossEntityStrategyKind, DomainLineKind, EntityTeachingBlock,
    EntityTeachingExprRow, EntityTeachingPrompt, PromptRenderMode, PromptSurfaceStats,
    RelationMaterializationSummary, RenderConfig, TeachingFieldGloss, TeachingHeading,
    TeachingLineMeta, TeachingPromptBundle, TeachingPromptModel, TeachingPromptSettings,
    TeachingPromptSource, TSV_TEACHING_TABLE_HEADER,
};

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
    program_param_contract_violations, DISCOVER_TOOL_DESCRIPTION, MCP_INITIALIZE_WORKFLOW,
    MCP_TOOL_SEQUENCING_MARKER, MCP_TOOL_SYNTAX_CONTRACT_MARKER, PLASM_CONTEXT_TOOL_DESCRIPTION,
    PLASM_PROGRAM_PARAM_DESCRIPTION, PLASM_PROGRAM_PARAM_MAX_BYTES,
    PLASM_READ_RUN_ARTIFACT_TOOL_DESCRIPTION, PLASM_RUN_TOOL_ARTIFACT_RESOURCES,
    PLASM_RUN_TOOL_ARTIFACT_TOOL, PLASM_RUN_TOOL_DESCRIPTION, PLASM_RUN_TOOL_DESCRIPTION_BASE,
    PLASM_TOOL_DESCRIPTION, PLASM_TOOL_DESCRIPTION_MAX_BYTES, PLASM_TOOL_DESCRIPTION_PREFIX_BYTES,
    PLASM_TOOL_DESCRIPTION_WIDE_PREFIX_BYTES, TEACHING_VALID_EXPR_MARKER,
};
pub use stats::{
    grammar_frontmatter_section_bytes, grammar_frontmatter_stats_from_contract,
    grammar_frontmatter_stats_from_prompt, json_tool_surface_counts, prompt_surface_stats,
    prompt_symbol_inflation_stats_from_prompt, strip_tsv_comment_contract_prefix,
    GrammarFrontmatterStats, PromptSymbolInflationStats,
};

pub(crate) use internal::*;

pub(crate) use bundle_render::{
    render_prompt_tsv_for_single_catalog_exposure, render_teaching_prompt_bundle_for_validation,
};
pub(crate) use relation_teaching::render_relation_edge_delta_rows;
pub(crate) use tsv_emit::{
    is_union_ctor_teaching_surface_line, parse_trailing_projection_bracket,
    render_prompt_tsv_from_bundle,
};
pub(crate) use types::{TeachingRowDedupeKey, TEACHING_OPTIONAL_LEGEND_MARK};

#[cfg(test)]
pub(crate) use gloss_filter::collect_opaque_domain_symbols;
#[cfg(test)]
pub(crate) use relation_teaching::incoming_relation_nav_bases_to_entity;
#[cfg(test)]
pub(crate) use row_producer_teaching::{projection_bracket_syms, projection_field_sets_equal};
#[cfg(test)]
pub(crate) use tsv_emit::{projection_bracket_from_teaching_rows, teaching_row_meaning_text};

#[cfg(test)]
pub(crate) use contract::validate_teaching_tsv_teaching_table;
pub(crate) use gloss_dedup::*;
#[cfg(test)]
pub(crate) use stats::domain_expression_tool_count_resolved;

/// Count of synthesized teaching example lines for an entity (same pipeline as emission).
#[cfg(test)]
pub(crate) fn domain_example_line_count(cgs: &CGS, ename: &str, map: Option<&SymbolMap>) -> usize {
    use line_validate::prompt_line_valid_cache_seed_cgs;

    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let seed = prompt_line_valid_cache_seed_cgs(cgs);
    let map_arc: Option<std::sync::Arc<SymbolMap>> = map.map(|m| std::sync::Arc::new(m.clone()));
    entity_block::collect_entity_teaching_block(
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
pub(crate) use crate::symbol_tuning::ExposureSlotKey;
#[cfg(test)]
pub(crate) use teaching_legend::teaching_expr_line_from_layers;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
