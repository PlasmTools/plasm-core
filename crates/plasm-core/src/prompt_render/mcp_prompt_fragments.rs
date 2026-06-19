//! Shared MCP / reuse prompt copy — single source for contract, initialize workflow, tool docs, and reuse surfaces.

use crate::TeachingExposureSession;

/// Marker for tests; compact tool-order line in MCP tool descriptions.
pub const MCP_TOOL_SEQUENCING_MARKER: &str =
    "Tool order: optional `discover_capabilities` → `plasm_context` → `plasm` (dry-run) → `plasm_run` (live).";

/// MCP initialize workflow + tool-description session line.
pub const SESSION_DISCIPLINE_MCP: &str = "**One goal → one stable `intent` → one `logical_session_ref`.** No new `intent` per API or message. Multi-API: one **`seeds`** array on the same intent.";

/// `plasm` program-contract header plus `plasm_run` review-gate note (MCP initialize).
pub const SESSION_DISCIPLINE_PROGRAM: &str = "**Session:** one goal → one **`intent`** → one **`logical_session_ref`**; use **`e#`/`m#`/`p#`/`r#` from this session's teaching TSV only** (contract examples are shapes; substitute from your table).";

/// Program construction preference for MCP `plasm` tool descriptions.
pub const MCP_PROGRAM_CONSTRUCTION_LINE: &str =
    "Prefer: bind rows → filter/project/sort/limit → few final roots. Plan with `plasm`; pass returned `pcN` to `plasm_run` only.";

/// Compact `e#=Entity` map for reuse responses (federated rows prefix `entry_id:` only when entity names collide).
pub fn render_compact_exposure_symbol_map(exp: &TeachingExposureSession) -> String {
    let mut name_counts = std::collections::HashMap::<&str, usize>::new();
    for entity in &exp.entities {
        *name_counts.entry(entity.as_str()).or_insert(0) += 1;
    }
    let needs_catalog_prefix = name_counts.values().any(|&c| c > 1);

    exp.entities
        .iter()
        .zip(exp.entity_catalog_entry_ids.iter())
        .enumerate()
        .map(|(i, (entity, entry_id))| {
            let sym = exp
                .qualified_entity_symbol(entry_id, entity)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("e{}", i + 1));
            let label = if needs_catalog_prefix {
                format!("{entry_id}:{entity}")
            } else {
                entity.clone()
            };
            format!("{sym}={label}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// MCP unified discovery TSV preamble (language flow + decision semantics).
pub const DISCOVER_TSV_LANGUAGE_PREAMBLE: &str = "\
# Plasm is a source language. These rows are NOT a program.\n\
# Next: pass selected api/entity rows to plasm_context.seeds, then write plasm.program using returned e#/m#/p#/r# symbols.";

/// Discovery decision values embedded as `# decision: …` TSV comment lines.
pub const DISCOVER_DECISION_MATCH: &str = "match";
pub const DISCOVER_DECISION_CLARIFY: &str = "clarify";
pub const DISCOVER_DECISION_NO_MATCH: &str = "no_match";

/// One-line tool sequencing for model-visible MCP `tools/list` descriptions.
pub fn render_plasm_mcp_tool_sequencing_line() -> String {
    MCP_TOOL_SEQUENCING_MARKER.to_string()
}

/// Session discipline block (intent stability + TSV symbol scope).
pub fn render_plasm_mcp_session_discipline_block() -> String {
    format!("{SESSION_DISCIPLINE_MCP}\n{SESSION_DISCIPLINE_PROGRAM}")
}

/// Program construction preference (bind → narrow → roots; plan/run split).
pub fn render_plasm_mcp_program_construction_line() -> String {
    MCP_PROGRAM_CONSTRUCTION_LINE.to_string()
}

/// Initialize / rollup header: tool order and discover → context flow.
pub fn render_plasm_mcp_initialize_workflow_head() -> String {
    "Plasm MCP tools (in order): **`plasm_context`** (session + teaching TSV), **`plasm`** (dry-run plan), **`plasm_run`** (live execute), **`discover_capabilities`** (when **`api`/`entity`** unknown).\n     **`discover_capabilities`**: one **`intent`** per goal; returns fenced **`tsv`** catalog picks (not a program). Merge rows into **`plasm_context`** **`seeds`**.\n     **`plasm_context`**: **`intent`** + **`seeds`** → ref + teaching TSV. Reuse ref for **`plasm`** / **`plasm_run`**; extend via same **`intent`**, more **`seeds`** (delta **`e#`**)."
        .to_string()
}

/// Initialize / rollup tail: plan-only, run gate, snapshots.
pub fn render_plasm_mcp_initialize_workflow_tail() -> String {
    "     **`plasm`** is plan-only (no live HTTP). One `program` per call; bare comma-separated roots (no `return`). Pass returned **`plan_commit_ref`** (`pcN`) to **`plasm_run`** only — do not re-send the program on live execute.\n     **`plasm_run`**: **`logical_session_ref`** + **`plan_commit_ref`** only; live execute the stored reviewed plan; may return snapshots.\n     **Snapshots:** `(in artifact)` / **`resource_link`** / `_meta.plasm.steps` → **`resources/read`** required before concluding data is missing."
        .to_string()
}

/// Model-facing `discover_capabilities` tool description.
pub fn render_plasm_mcp_discover_tool_description() -> String {
    format!(
        "Plasm is a source language. Pick catalogs/entities for one user goal — this tool does **not** produce program symbols. {}\n     **Next:** copy TSV `api`/`entity` rows into one **`plasm_context`** **`seeds`** array on the same **`intent`**, then write **`plasm.program`** from returned teaching TSV symbols. Skip when you already know every `api`/`entity`. There is no alternate JSON discovery mode for agents.",
        MCP_TOOL_SEQUENCING_MARKER,
    )
}

/// Workflow lines prepended to the `plasm_context` tool description.
pub fn render_plasm_mcp_context_tool_workflow_lines() -> String {
    format!(
        "{}\n     **Call before `plasm` / `plasm_run`.** {}\n",
        MCP_TOOL_SEQUENCING_MARKER,
        render_plasm_mcp_session_discipline_block(),
    )
}

/// Operational tail for the `plasm_run` tool description (live execute + snapshots).
pub fn render_plasm_mcp_run_tool_operational_tail() -> String {
    "**Review gate:** `plasm_run` executes exactly the reviewed plan stored under the **`plan_commit_ref`**. If the token is missing, expired, or from another plan, call **`plasm`** again.\n\n**Live execute:** server spawns one async operation and awaits terminal rows in the tool response. Progress uses standard `notifications/plasm/op` on the registered handle.\n\n**Live results:** `## {return_label} ({n} rows)` + TSV/table; multi-return programs use `# Results` with `### {label} ({n} rows)` per root. Truncated rows: `_meta.plasm.steps` + MCP **`resources/read`** on snapshot URIs — required before concluding fields are absent."
        .to_string()
}
