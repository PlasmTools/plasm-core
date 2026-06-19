//! Shared MCP / reuse prompt copy — single source for contract, initialize workflow, tool docs, and reuse cheat sheet.

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

/// Reuse one-liner: same-ref discipline (embedded in `_Session unchanged …_` wrapper).
pub const REUSE_SESSION_UNCHANGED_DISCIPLINE: &str = "Reuse this `logical_session_ref`; do not re-open `plasm_context` with identical seeds or a new `intent` for the same goal.";

/// Reuse-path cheat sheet tail (after entity-range notice).
pub const REUSE_CHEATSHEET_TAIL: &str = "\
**Symbols:** only `e#`/`m#`/`p#`/`r#` from the teaching TSV bound to this ref — not contract examples or other sessions.\n\
**Expand:** same `intent`, more `seeds` — read delta TSV for new symbols. Search: `e#~$` or `e#~\"text\"`; scoped: `e#{{p#=…}}`. Row compute: bind first; `.group_by` / `.filter` on **`rows:` fields only**.\n\
Use `plasm` / `plasm_run` with this ref.\n";

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
    "Plasm MCP tools (in order): **`plasm_context`** (session + teaching TSV), **`plasm`** (dry-run plan), **`plasm_run`** (live execute), **`discover_capabilities`** (when **`api`/`entity`** unknown).\n     **`discover_capabilities`**: one **`intent`** per goal. Default **fenced `tsv`** — skip **`typed`** unless the TSV note requires it. Merge rows into **`plasm_context`** **`seeds`**.\n     **`plasm_context`**: **`intent`** + **`seeds`** → ref + teaching TSV. Reuse ref for **`plasm`** / **`plasm_run`**; extend via same **`intent`**, more **`seeds`** (delta **`e#`**)."
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
        "Resolve one user goal to catalog capabilities. {}\n     **Next:** merge TSV rows into one **`plasm_context`** **`seeds`** array on the same **`intent`**. **Default:** fenced **`tsv`** table (`api`, `entity`, `description`). Skip when you already know every `api`/`entity`. Set **`typed: true`** only when the TSV ambiguity note requires structured disambiguation (returns fenced **`json`** instead).",
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
