//! MCP `tools/list` tool definitions and gate-aware descriptions.

use std::collections::BTreeMap;

use rust_mcp_sdk::schema::{
    Tool, ToolAnnotations, ToolExecution, ToolExecutionTaskSupport, ToolInputSchema,
};

use crate::mcp_run_markdown::ArtifactAccessMode;
use plasm_core::prompt_render::{
    DISCOVER_TOOL_DESCRIPTION, PLASM_CONTEXT_TOOL_DESCRIPTION, PLASM_PROGRAM_PARAM_DESCRIPTION,
    PLASM_READ_RUN_ARTIFACT_TOOL_DESCRIPTION, PLASM_RUN_TOOL_ARTIFACT_RESOURCES,
    PLASM_RUN_TOOL_ARTIFACT_TOOL, PLASM_RUN_TOOL_DESCRIPTION_BASE, PLASM_TOOL_DESCRIPTION,
};

use super::schema::{
    json_schema_non_empty_object_array, json_schema_non_empty_string_type, json_schema_string_type,
};

pub(crate) fn plasm_run_tool_description(mode: ArtifactAccessMode) -> String {
    let suffix = match mode {
        ArtifactAccessMode::ResourcesRead => PLASM_RUN_TOOL_ARTIFACT_RESOURCES,
        ArtifactAccessMode::ToolFallback => PLASM_RUN_TOOL_ARTIFACT_TOOL,
    };
    format!("{}{}", PLASM_RUN_TOOL_DESCRIPTION_BASE, suffix)
}

fn workflow_mcp_tools_enabled() -> bool {
    std::env::var("PLASM_MCP_WORKFLOW_TOOLS")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

pub(crate) fn plasm_tools(artifact_access: ArtifactAccessMode) -> Vec<Tool> {
    let mut context_props = BTreeMap::new();
    context_props.insert(
        "session_mode".into(),
        json_schema_non_empty_string_type(
            "\"new\" mints a logical session and fresh symbol table (once per workflow). \"extend\" continues an existing session (requires logical_session_ref).",
        ),
    );
    context_props.insert(
        "intent".into(),
        json_schema_non_empty_string_type(
            "This turn's task description. Appended to the session on extend; used for capability scoring — not session identity.",
        ),
    );
    context_props.insert(
        "logical_session_ref".into(),
        serde_json::from_value(serde_json::json!({
            "type": ["string", "null"],
            "description": "Required when session_mode is \"extend\". Same ref returned by plasm_context and reused on plasm / plasm_run. Must be omitted when session_mode is \"new\"."
        }))
        .expect("logical_session_ref schema"),
    );
    context_props.insert(
            "seeds".into(),
            json_schema_non_empty_object_array(
                "Non-empty array of `{api, entity}` capability picks (or `{entry_id, entity}`). The `plasm_context` response returns the active symbols for `plasm` programs.",
                vec!["api", "entity"],
            ),
        );
    context_props.insert(
            "ranked_capabilities".into(),
            serde_json::from_value(serde_json::json!({
                "type": ["array", "null"],
                "items": { "type": "string" },
                "description": "Optional capability **wire names** (e.g. from `discover_capabilities`). When non-empty, **non-seeded** mutators must appear in this list and score against **`intent`**. Seeded entities always teach **query/search/get** (and `primary_read`); **create/update/delete/action** need intent overlap (read-first open defers weak matches). Omit on expand to keep the session list; send **`null`** or **`[]`** to clear."
            }))
            .expect("ranked_capabilities schema"),
        );
    let mut discover_props = BTreeMap::new();
    discover_props.insert(
            "intent".into(),
            json_schema_non_empty_string_type(
                "One plain-language task description for the whole user goal. Returns catalog `api`/`entity` picks — not program symbols. Pass picks to plasm_context with session_mode new or extend.",
            ),
        );
    let mut plasm_program_props = BTreeMap::new();
    plasm_program_props.insert(
            "logical_session_ref".into(),
            json_schema_string_type(
                "Same `logical_session_ref` returned by `plasm_context`. Reuse for follow-up `plasm` (plan) and `plasm_run` (execute) calls.",
            ),
        );
    plasm_program_props.insert(
        "program".into(),
        json_schema_string_type(PLASM_PROGRAM_PARAM_DESCRIPTION),
    );
    plasm_program_props.insert(
        "reasoning".into(),
        json_schema_string_type("Optional short note explaining the intent of this call."),
    );
    let mut plasm_run_props = plasm_program_props.clone();
    plasm_run_props.remove("program");
    plasm_run_props.insert(
            "run_ref".into(),
            json_schema_string_type(
                "Token for `plasm_run`: `pcN` from `plasm`, or the page handle from a \"more pages\" line. (`page(...)` is HTTP-execute program syntax, not an MCP value.)",
            ),
        );

    let mut tools = vec![
        Tool {
            name: "plasm_context".into(),
            title: Some("Open or extend Plasm context".into()),
            description: Some(PLASM_CONTEXT_TOOL_DESCRIPTION.into()),
            input_schema: ToolInputSchema::new(
                vec!["session_mode".into(), "intent".into(), "seeds".into()],
                Some(context_props),
                None,
            ),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(false),
                open_world_hint: Some(true),
                ..Default::default()
            }),
            execution: Some(ToolExecution {
                task_support: Some(ToolExecutionTaskSupport::Forbidden),
            }),
            icons: vec![],
            meta: None,
            output_schema: None,
        },
        Tool {
            name: "discover_capabilities".into(),
            title: Some("Resolve intent to capabilities".into()),
            description: Some(DISCOVER_TOOL_DESCRIPTION.into()),
            input_schema: ToolInputSchema::new(vec!["intent".into()], Some(discover_props), None),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(true),
                open_world_hint: Some(true),
                ..Default::default()
            }),
            execution: Some(ToolExecution {
                task_support: Some(ToolExecutionTaskSupport::Forbidden),
            }),
            icons: vec![],
            meta: None,
            output_schema: None,
        },
    ];
    tools.push(Tool {
        name: "plasm".into(),
        title: Some("Plan Plasm (dry-run)".into()),
        description: Some(PLASM_TOOL_DESCRIPTION.into()),
        input_schema: ToolInputSchema::new(
            vec!["logical_session_ref".into(), "program".into()],
            Some(plasm_program_props.clone()),
            None,
        ),
        annotations: Some(ToolAnnotations {
            read_only_hint: Some(true),
            open_world_hint: Some(true),
            ..Default::default()
        }),
        execution: Some(ToolExecution {
            task_support: Some(ToolExecutionTaskSupport::Forbidden),
        }),
        icons: vec![],
        meta: Some(crate::plan_ui_mcp::plan_review_ui_tool_meta()),
        output_schema: None,
    });
    tools.push(Tool {
        name: "plasm_run".into(),
        title: Some("Run Plasm (execute)".into()),
        description: Some(plasm_run_tool_description(artifact_access)),
        input_schema: ToolInputSchema::new(
            vec!["logical_session_ref".into(), "run_ref".into()],
            Some(plasm_run_props),
            None,
        ),
        annotations: Some(ToolAnnotations {
            read_only_hint: Some(false),
            open_world_hint: Some(true),
            ..Default::default()
        }),
        execution: Some(ToolExecution {
            task_support: Some(ToolExecutionTaskSupport::Forbidden),
        }),
        icons: vec![],
        meta: Some(crate::run_explorer_ui_mcp::run_explorer_ui_tool_meta()),
        output_schema: None,
    });
    tools.push(Tool {
        name: "plasm_ui_list_catalogs".into(),
        title: Some("List tenant-enabled catalogs (MCP App)".into()),
        description: Some(
            "Returns registry `entry_id`s allowed by tenant MCP policy for MCP App bootstrap UIs."
                .into(),
        ),
        input_schema: ToolInputSchema::new(vec![], Some(BTreeMap::new()), None),
        annotations: Some(ToolAnnotations {
            read_only_hint: Some(true),
            open_world_hint: Some(false),
            ..Default::default()
        }),
        execution: Some(ToolExecution {
            task_support: Some(ToolExecutionTaskSupport::Forbidden),
        }),
        icons: vec![],
        meta: Some(
            serde_json::json!({
                "ui": { "visibility": ["app"] }
            })
            .as_object()
            .cloned()
            .expect("plasm_ui_list_catalogs meta"),
        ),
        output_schema: None,
    });
    if artifact_access.exposes_read_tool() {
        let mut read_props = BTreeMap::new();
        read_props.insert(
                "logical_session_ref".into(),
                json_schema_string_type(
                    "Same `logical_session_ref` returned by `plasm_context`. Required for tenant/session binding.",
                ),
            );
        read_props.insert(
                "artifact_uri".into(),
                serde_json::from_value(serde_json::json!({
                    "type": ["string", "null"],
                    "description": "Short or canonical `plasm://…` snapshot URI from `plasm_run`. Provide exactly one of `artifact_uri`, `resource_index`, or `run_id`."
                }))
                .expect("artifact_uri schema"),
            );
        read_props.insert(
                "resource_index".into(),
                serde_json::from_value(serde_json::json!({
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Monotonic run index `n` from `plasm://session/{logical_session_ref}/r/{n}`."
                }))
                .expect("resource_index schema"),
            );
        read_props.insert(
            "run_id".into(),
            serde_json::from_value(serde_json::json!({
                "type": ["string", "null"],
                "description": "Canonical run id (`pr` + 64 hex) when the URI is not at hand."
            }))
            .expect("run_id schema"),
        );
        tools.push(Tool {
            name: "plasm_read_run_artifact".into(),
            title: Some("Read Plasm run snapshot".into()),
            description: Some(PLASM_READ_RUN_ARTIFACT_TOOL_DESCRIPTION.into()),
            input_schema: ToolInputSchema::new(
                vec!["logical_session_ref".into()],
                Some(read_props),
                None,
            ),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(true),
                open_world_hint: Some(false),
                ..Default::default()
            }),
            execution: Some(ToolExecution {
                task_support: Some(ToolExecutionTaskSupport::Forbidden),
            }),
            icons: vec![],
            meta: None,
            output_schema: None,
        });
    }
    if workflow_mcp_tools_enabled() {
        tools.extend(crate::workflow_mcp::workflow_mcp_tools());
    }
    tools
}
