//! MCP `tools/list` tool definitions and gate-aware descriptions.

use std::collections::BTreeMap;

use rust_mcp_sdk::schema::{
    Tool, ToolAnnotations, ToolExecution, ToolExecutionTaskSupport, ToolInputSchema,
};
use serde_json::Map;

use crate::mcp_delivery::McpDeliveryProfile;
use crate::mcp_run_markdown::ArtifactAccessMode;
use plasm_core::prompt_render::{
    DISCOVER_TOOL_DESCRIPTION, PLASM_CONTEXT_TOOL_DESCRIPTION, PLASM_PROGRAM_PARAM_DESCRIPTION,
    PLASM_READ_RUN_ARTIFACT_TOOL_DESCRIPTION, PLASM_RUN_TOOL_ARTIFACT_RESOURCES,
    PLASM_RUN_TOOL_ARTIFACT_TOOL, PLASM_RUN_TOOL_DESCRIPTION_BASE, PLASM_TOOL_DESCRIPTION,
};

use super::schema::{json_schema_non_empty_string_type, json_schema_string_type};

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

pub(crate) fn mcp_discover_tool_enabled() -> bool {
    !mcp_semantic_auto_seed_enabled()
}

/// Runtime gate shared with [`mcp_discover_tool_enabled`] / plasm_context seeds schema.
pub(crate) fn mcp_semantic_auto_seed_enabled() -> bool {
    #[cfg(feature = "semantic-auto-seed")]
    {
        crate::discovery_seed_select::semantic_auto_seed_enabled()
    }
    #[cfg(not(feature = "semantic-auto-seed"))]
    {
        false
    }
}

fn plasm_context_seeds_schema_description() -> &'static str {
    if mcp_semantic_auto_seed_enabled() {
        "Required on session_mode \"extend\" only. Omit on \"new\" (intent-only auto-seed; rejected if passed). Each object is `{api, entity}` (or `{entry_id, entity}`); entity names resolve case-insensitively to catalog keys."
    } else {
        "Required non-empty on session_mode \"new\" and \"extend\". Each object is `{api, entity}` (or `{entry_id, entity}`); entity names resolve case-insensitively to catalog keys."
    }
}

pub(crate) fn plasm_tools(artifact_access: ArtifactAccessMode, ui_apps_enabled: bool) -> Vec<Tool> {
    let delivery = McpDeliveryProfile::resolve(ui_apps_enabled, artifact_access);
    let structured_ui_lane = delivery.emits_structured_ui();
    let attach_ui_meta = delivery.attaches_ui_meta();
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
        serde_json::from_value(serde_json::json!({
            "type": ["array", "null"],
            "items": {
                "type": "object",
                "properties": {
                    "api": { "type": "string" },
                    "entry_id": { "type": "string" },
                    "entity": { "type": "string" }
                },
                "required": ["entity"],
                "anyOf": [
                    { "required": ["api"] },
                    { "required": ["entry_id"] }
                ]
            },
            "description": plasm_context_seeds_schema_description()
        }))
        .expect("seeds schema"),
    );
    context_props.insert(
            "ranked_capabilities".into(),
            serde_json::from_value(serde_json::json!({
                "type": ["array", "null"],
                "items": { "type": "string" },
                "description": "Optional capability **wire names**. When non-empty, **non-seeded** mutators must appear in this list and score against **`intent`**. Seeded entities always teach **query/search/get** (and `primary_read`); **create/update/delete/action** need intent overlap (read-first open defers weak matches). Omit on expand to keep the session list; send **`null`** or **`[]`** to clear."
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

    let discover_tool = Tool {
        name: "discover_capabilities".into(),
        title: Some("Browse capabilities (recovery)".into()),
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
    };

    let mut tools = vec![Tool {
        name: "plasm_context".into(),
        title: Some("Open or extend Plasm context".into()),
        description: Some(PLASM_CONTEXT_TOOL_DESCRIPTION.into()),
        input_schema: ToolInputSchema::new(
            vec!["session_mode".into(), "intent".into()],
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
    }];

    let include_discover_tool = mcp_discover_tool_enabled();
    if include_discover_tool {
        tools.push(discover_tool);
    }

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
        meta: if attach_ui_meta {
            Some(crate::plan_ui_mcp::plan_review_ui_tool_meta())
        } else {
            None
        },
        output_schema: if structured_ui_lane {
            Some(crate::mcp_ui_payload::plasm_tool_output_schema())
        } else {
            None
        },
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
        meta: if attach_ui_meta {
            Some(crate::run_explorer_ui_mcp::run_explorer_ui_tool_meta())
        } else {
            None
        },
        output_schema: if structured_ui_lane {
            Some(crate::mcp_ui_payload::plasm_run_tool_output_schema())
        } else {
            None
        },
    });
    if attach_ui_meta {
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
        tools.push(ui_read_plan_tool());
        tools.push(ui_read_run_tool());
    }
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
                    "description": "Snapshot URI from the same `plasm_run` step (`plasm://session/…/run/pr…` or canonical `plasm://execute/…/run/pr…`). Provide exactly one of `artifact_uri` or `run_id`."
                }))
                .expect("artifact_uri schema"),
            );
        read_props.insert(
            "run_id".into(),
            serde_json::from_value(serde_json::json!({
                "type": ["string", "null"],
                "description": "Content-addressed run id (`pr` + 64 hex) from `_meta.plasm.steps[].run_id` when the URI is not at hand."
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
    if workflow_mcp_tools_enabled() && attach_ui_meta {
        tools.extend(crate::workflow_mcp::workflow_mcp_tools());
    }
    tools
}

fn app_only_tool_meta() -> Map<String, serde_json::Value> {
    serde_json::json!({
        "ui": { "visibility": ["app"] }
    })
    .as_object()
    .cloned()
    .expect("app-only tool meta")
}

fn ui_read_plan_tool() -> Tool {
    let mut props = BTreeMap::new();
    props.insert(
        "logical_session_ref".into(),
        json_schema_string_type("Same `logical_session_ref` returned by `plasm_context`."),
    );
    props.insert(
        "run_ref".into(),
        json_schema_string_type("`pcN` plan commit ref from `plasm` dry-run."),
    );
    Tool {
        name: "plasm_ui_read_plan".into(),
        title: Some("Hydrate plan DAG for MCP App view".into()),
        description: Some(
            "App-only: returns `structuredContent.ui` with `comp` + `plan_ux_reflection` for Plan Review when the host forward omits the UI lane.".into(),
        ),
        input_schema: ToolInputSchema::new(
            vec!["logical_session_ref".into(), "run_ref".into()],
            Some(props),
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
        meta: Some(app_only_tool_meta()),
        output_schema: Some(crate::mcp_ui_payload::plasm_tool_output_schema()),
    }
}

fn ui_read_run_tool() -> Tool {
    let mut props = BTreeMap::new();
    props.insert(
        "logical_session_ref".into(),
        json_schema_string_type("Same `logical_session_ref` returned by `plasm_context`."),
    );
    props.insert(
        "artifact_uri".into(),
        serde_json::from_value(serde_json::json!({
            "type": ["string", "null"],
            "description": "Run snapshot URI from `plasm_run`. Provide exactly one of `artifact_uri` or `run_id`."
        }))
        .expect("artifact_uri schema"),
    );
    props.insert(
        "run_id".into(),
        serde_json::from_value(serde_json::json!({
            "type": ["string", "null"],
            "description": "Content-addressed run id (`pr` + 64 hex) from the tool result step."
        }))
        .expect("run_id schema"),
    );
    Tool {
        name: "plasm_ui_read_run".into(),
        title: Some("Hydrate run snapshot for MCP App view".into()),
        description: Some(
            "App-only: returns `structuredContent.ui.steps[].preview_entities` when the host forward omits run rows.".into(),
        ),
        input_schema: ToolInputSchema::new(
            vec!["logical_session_ref".into()],
            Some(props),
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
        meta: Some(app_only_tool_meta()),
        output_schema: Some(crate::mcp_ui_payload::plasm_run_tool_output_schema()),
    }
}
