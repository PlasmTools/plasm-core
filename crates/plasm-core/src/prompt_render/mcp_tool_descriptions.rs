//! Static MCP tool descriptions — canonical home of Plasm language grammar.
//!
//! MCP initialize is poorly supported by clients; **`tools/list` descriptions carry the canonical truth**.
//! The same bytes are prepended to eval/REPL/`plasm init`/terminal surfaces via [`PLASM_TOOL_DESCRIPTION`].

/// Canonical `plasm` tool description: plan-only framing + full Plasm grammar contract.
pub const PLASM_TOOL_DESCRIPTION: &str = include_str!("assets/plasm_tool.txt");

/// Canonical `plasm_run` tool description.
pub const PLASM_RUN_TOOL_DESCRIPTION: &str = include_str!("assets/plasm_run_tool.txt");

/// Canonical `plasm_context` tool description.
pub const PLASM_CONTEXT_TOOL_DESCRIPTION: &str = include_str!("assets/plasm_context_tool.txt");

/// Canonical `discover_capabilities` tool description.
pub const DISCOVER_TOOL_DESCRIPTION: &str = include_str!("assets/discover_tool.txt");

/// JSON-schema description for the MCP `plasm` tool `program` parameter.
pub const PLASM_PROGRAM_PARAM_DESCRIPTION: &str = include_str!("assets/program_param.txt");

/// Supplementary MCP initialize workflow rollup (untrusted; grammar lives in [`PLASM_TOOL_DESCRIPTION`]).
pub const MCP_INITIALIZE_WORKFLOW: &str = include_str!("assets/initialize_workflow.txt");

/// Marker for tests; compact executable-syntax guard in [`PLASM_TOOL_DESCRIPTION`].
pub const MCP_TOOL_SYNTAX_CONTRACT_MARKER: &str = "`program` is Plasm source text, not JSON data.";

/// Marker for tests; tool-order line in MCP tool descriptions.
pub const MCP_TOOL_SEQUENCING_MARKER: &str =
    "Tool order: optional `discover_capabilities` → `plasm_context` → `plasm` (dry-run) → `plasm_run` (live).";

/// Marker for tests; grammar contract opener in [`PLASM_TOOL_DESCRIPTION`].
pub const TEACHING_VALID_EXPR_MARKER: &str =
    "Grammar below; symbols from `plasm_context` TSV. Reply with one valid plasm_program:";
