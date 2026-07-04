//! Static MCP tool descriptions — canonical home of Plasm language grammar.
//!
//! MCP initialize is poorly supported by clients; **`tools/list` descriptions carry the canonical truth**.
//! The same bytes are prepended to eval/REPL/`plasm init`/terminal surfaces via [`PLASM_TOOL_DESCRIPTION`].

/// Canonical `plasm` tool description: plan-only framing + full Plasm grammar contract.
pub const PLASM_TOOL_DESCRIPTION: &str = include_str!("assets/plasm_tool.txt");

/// Canonical `plasm_run` tool body (without transport-specific artifact-read suffix).
pub const PLASM_RUN_TOOL_DESCRIPTION_BASE: &str = include_str!("assets/plasm_run_tool_base.txt");

/// Artifact-read suffix when the host exposes MCP `resources/read`.
pub const PLASM_RUN_TOOL_ARTIFACT_RESOURCES: &str =
    include_str!("assets/plasm_run_tool_artifact_resources.txt");

/// Artifact-read suffix for tool-only MCP hosts (`plasm_read_run_artifact`).
pub const PLASM_RUN_TOOL_ARTIFACT_TOOL: &str =
    include_str!("assets/plasm_run_tool_artifact_tool.txt");

/// Canonical `plasm_read_run_artifact` tool description (tool-only MCP hosts).
pub const PLASM_READ_RUN_ARTIFACT_TOOL_DESCRIPTION: &str =
    include_str!("assets/plasm_read_run_artifact_tool.txt");

/// Default static `plasm_run` description (resource-capable hosts).
pub const PLASM_RUN_TOOL_DESCRIPTION: &str = concat!(
    include_str!("assets/plasm_run_tool_base.txt"),
    include_str!("assets/plasm_run_tool_artifact_resources.txt"),
);

/// Canonical `plasm_context` tool description.
pub const PLASM_CONTEXT_TOOL_DESCRIPTION: &str = include_str!("assets/plasm_context_tool.txt");

/// Canonical `discover_capabilities` tool description.
pub const DISCOVER_TOOL_DESCRIPTION: &str = include_str!("assets/discover_tool.txt");

/// JSON-schema description for the MCP `plasm` tool `program` parameter.
///
/// Field-attached surface: hosts often clip the long [`PLASM_TOOL_DESCRIPTION`] but keep
/// per-parameter schema text, so this must be a self-sufficient minimum program-authoring
/// contract (not a pointer-only stub). Budget: [`PLASM_PROGRAM_PARAM_MAX_BYTES`].
pub const PLASM_PROGRAM_PARAM_DESCRIPTION: &str = include_str!("assets/program_param.txt");

/// Max bytes for [`PLASM_PROGRAM_PARAM_DESCRIPTION`] (truncation-resistant field surface).
pub const PLASM_PROGRAM_PARAM_MAX_BYTES: usize = 2048;

/// Max bytes for [`PLASM_TOOL_DESCRIPTION`].
pub const PLASM_TOOL_DESCRIPTION_MAX_BYTES: usize = 8000;

/// Host-truncation prefixes that must still carry program-authoring mandates.
pub const PLASM_TOOL_DESCRIPTION_PREFIX_BYTES: usize = 2048;
pub const PLASM_TOOL_DESCRIPTION_WIDE_PREFIX_BYTES: usize = 4096;

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

/// Substrings that must appear in [`PLASM_PROGRAM_PARAM_DESCRIPTION`].
const PROGRAM_PARAM_CONTRACT_MARKERS: &[&str] = &[
    "not JSON data",
    "plasm_context",
    "teaching TSV",
    "final return line",
    "<<TAG",
    "session_mode: \"extend\"",
    "`plasm` tool description",
];

/// Returns human-readable contract violations for the `program` param description, or empty if ok.
pub fn program_param_contract_violations(param: &str) -> Vec<String> {
    let mut violations = Vec::new();
    if param.len() > PLASM_PROGRAM_PARAM_MAX_BYTES {
        violations.push(format!(
            "program param description too long: {} bytes (max {PLASM_PROGRAM_PARAM_MAX_BYTES})",
            param.len()
        ));
    }
    for marker in PROGRAM_PARAM_CONTRACT_MARKERS {
        if !param.contains(marker) {
            violations.push(format!("missing required marker {marker:?}"));
        }
    }
    if !(param.contains("e#")
        && param.contains("m#")
        && param.contains("p#")
        && param.contains("r#"))
    {
        violations.push("must carry the symbol legend e#/m#/p#/r#".into());
    }
    violations
}
