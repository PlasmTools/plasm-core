//! MCP workflow and tool description strings.

/// Model-facing **`plasm`** tool description: plan-only program construction + syntax contract.
pub(crate) fn mcp_plasm_tool_description() -> &'static str {
    static DESC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DESC.get_or_init(|| {
        format!(
            "{}\n\n{}\n\n{}",
            include_str!("../mcp_prompt/plasm_tool_head.txt").trim_end(),
            plasm_core::prompt_render::render_plasm_mcp_program_construction_line().trim_end(),
            plasm_core::prompt_render::render_plasm_mcp_tool_syntax_contract().trim_end()
        )
    })
    .as_str()
}

/// Model-facing **`plasm_run`** tool description: live execution after plan review.
pub(crate) fn mcp_plasm_run_tool_description() -> &'static str {
    static DESC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DESC.get_or_init(|| {
        format!(
            "{}\n\n{}",
            include_str!("../mcp_prompt/plasm_run_tool_head.txt").trim_end(),
            plasm_core::prompt_render::render_plasm_mcp_run_tool_operational_tail().trim_end()
        )
    })
    .as_str()
}

/// Model-facing **`plasm_context`** tool description (workflow + teaching TSV + continuity).
pub(crate) fn mcp_plasm_context_tool_description() -> &'static str {
    static DESC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DESC.get_or_init(|| {
        format!(
            "{}\n{}",
            plasm_core::prompt_render::render_plasm_mcp_context_tool_workflow_lines().trim_end(),
            include_str!("../mcp_prompt/plasm_context_tool_body.txt").trim_start()
        )
    })
    .as_str()
}

/// Model-facing **`discover_capabilities`** tool description.
pub(crate) fn mcp_discover_tool_description() -> &'static str {
    static DESC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DESC.get_or_init(plasm_core::prompt_render::render_plasm_mcp_discover_tool_description)
        .as_str()
}

/// One-line JSON-schema description for the **`program`** parameter on **`plasm`**.
pub(crate) fn mcp_program_param_description() -> &'static str {
    static DESC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DESC.get_or_init(plasm_core::prompt_render::render_plasm_mcp_program_param_description)
        .as_str()
}

/// MCP initialize workflow text: canonical rollup of the same fragments as `tools/list`.
pub(crate) fn mcp_server_initialize_workflow() -> String {
    format!(
        "{head}\n     {session}\n{program}\n\n{tail}",
        head = plasm_core::prompt_render::render_plasm_mcp_initialize_workflow_head(),
        session = plasm_core::prompt_render::SESSION_DISCIPLINE_MCP,
        program = plasm_core::prompt_render::SESSION_DISCIPLINE_PROGRAM,
        tail = plasm_core::prompt_render::render_plasm_mcp_initialize_workflow_tail(),
    )
}

pub(crate) fn mcp_server_initialize_instructions() -> String {
    mcp_server_initialize_workflow()
}
