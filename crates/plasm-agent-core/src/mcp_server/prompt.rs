//! MCP initialize workflow and tool description strings.

/// Model-facing **`plasm`** tool description: **plan-only** program construction (session setup is in initialize instructions).
pub(crate) const MCP_PLASM_TOOL_DESCRIPTION: &str =
    include_str!("../mcp_prompt/plasm_tool_head.txt");

/// Model-facing **`plasm_run`** tool description: live execution after plan review.
pub(crate) const MCP_PLASM_RUN_TOOL_DESCRIPTION: &str =
    include_str!("../mcp_prompt/plasm_run_tool.txt");

/// Model-facing **`plasm_context`** tool description (teaching TSV + continuity; federation in initialize workflow).
pub(crate) fn mcp_plasm_context_tool_description() -> &'static str {
    static DESC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DESC.get_or_init(|| include_str!("../mcp_prompt/plasm_context_tool_body.txt").to_string())
        .as_str()
}

/// One-line JSON-schema description for the **`program`** parameter on **`plasm`**.
pub(crate) const MCP_PROGRAM_PARAM_DESCRIPTION: &str =
    "Plasm program JSON string. Grammar is in MCP initialize; symbols come from `plasm_context` TSV.";

/// MCP initialize workflow text (orchestration + canonical grammar; async poll appended at runtime).
pub(crate) fn mcp_server_initialize_workflow() -> String {
    format!(
        "{head}     {session}\n{program}\n\n{grammar}\n{tail}",
        head = include_str!("../mcp_prompt/workflow_head.txt"),
        session = plasm_core::prompt_render::SESSION_DISCIPLINE_MCP,
        program = plasm_core::prompt_render::SESSION_DISCIPLINE_PROGRAM,
        grammar = plasm_core::prompt_render::render_plasm_mcp_language_frontmatter(),
        tail = include_str!("../mcp_prompt/workflow_tail.txt"),
    )
}

pub(crate) fn mcp_server_initialize_instructions() -> String {
    mcp_server_initialize_workflow()
}
