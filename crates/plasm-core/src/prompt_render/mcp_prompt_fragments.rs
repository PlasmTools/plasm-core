//! Shared MCP / reuse prompt copy — single source for contract, initialize workflow, tool docs, and reuse cheat sheet.

/// MCP initialize workflow + tool-description session line.
pub const SESSION_DISCIPLINE_MCP: &str = "**One goal → one stable `intent` → one `logical_session_ref`.** No new `intent` per API or message. Multi-API: one **`seeds`** array on the same intent.";

/// `plasm` / `plasm_run` program-contract header (MCP initialize).
pub const SESSION_DISCIPLINE_PROGRAM: &str = "**Session:** one goal → one **`intent`** → one **`logical_session_ref`**; use **`e#`/`m#`/`p#`/`r#` from this session's teaching TSV only** (contract examples are shapes; substitute from your table).";

/// Reuse one-liner: same-ref discipline (embedded in `_Session unchanged …_` wrapper).
pub const REUSE_SESSION_UNCHANGED_DISCIPLINE: &str = "Reuse this `logical_session_ref`; do not re-open `plasm_context` with identical seeds or a new `intent` for the same goal.";

/// Reuse-path cheat sheet tail (after entity-range notice).
pub const REUSE_CHEATSHEET_TAIL: &str = "\
**Symbols:** only `e#`/`m#`/`p#`/`r#` from the teaching TSV bound to this ref — not contract examples or other sessions.\n\
**Expand:** same `intent`, more `seeds` — read delta TSV for new symbols. Search: `e#~$` or `e#~\"text\"`; scoped: `e#{{p#=…}}`. Row compute: bind first; `.group_by` / `.filter` on **`rows:` fields only**.\n\
Use `plasm` / `plasm_run` with this ref.\n";
