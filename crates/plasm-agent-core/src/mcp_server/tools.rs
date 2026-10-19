//! MCP `tools/list` tool definitions and gate-aware descriptions.

use std::collections::BTreeMap;

use rmcp::model::{Tool, ToolAnnotations};
use serde_json::Map;

use crate::mcp_delivery::McpDeliveryProfile;
use crate::mcp_run_markdown::ArtifactAccessMode;
use plasm_core::prompt_render::{
    PLASM_CONTEXT_TOOL_DESCRIPTION, PLASM_MCP_TOOL_DESCRIPTION, PLASM_PROGRAM_PARAM_DESCRIPTION,
    PLASM_READ_RUN_ARTIFACT_TOOL_DESCRIPTION, PLASM_RUN_TOOL_ARTIFACT_RESOURCES,
    PLASM_RUN_TOOL_ARTIFACT_TOOL, PLASM_RUN_TOOL_DESCRIPTION_BASE,
};

use super::schema::{json_schema_non_empty_string_type, json_schema_string_type};

pub(crate) fn plasm_run_tool_description(mode: ArtifactAccessMode) -> String {
    let suffix = match mode {
        ArtifactAccessMode::ResourcesRead => PLASM_RUN_TOOL_ARTIFACT_RESOURCES,
        ArtifactAccessMode::DagCompute => "",
        ArtifactAccessMode::ToolFallback => PLASM_RUN_TOOL_ARTIFACT_TOOL,
    };
    format!("{}{}", PLASM_RUN_TOOL_DESCRIPTION_BASE, suffix)
}

fn workflow_mcp_tools_enabled() -> bool {
    std::env::var("PLASM_MCP_WORKFLOW_TOOLS")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
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
            "This turn's current intent. Appended to the workflow provenance chain; current intent drives retrieval, ancestry supplies context for judgment. Independent of session identity.",
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
    let mut plasm_program_props = BTreeMap::new();
    plasm_program_props.insert(
            "logical_session_ref".into(),
            json_schema_string_type(
                "Same `logical_session_ref` returned by `plasm_context`. Reuse for follow-up `plasm` (reads execute when clean; writes return `run_ref`) and `plasm_run` (reviewed writes / paging).",
            ),
        );
    plasm_program_props.insert(
        "program".into(),
        json_schema_string_type(PLASM_PROGRAM_PARAM_DESCRIPTION),
    );
    let mut plasm_run_props = plasm_program_props.clone();
    plasm_run_props.remove("program");
    plasm_run_props.insert(
            "run_ref".into(),
            json_schema_string_type(
                "Token for `plasm_run`: `pcN` from `plasm`, or the page handle from a \"more pages\" line. (`page(...)` is HTTP-execute program syntax, not an MCP value.)",
            ),
        );

    let mut tools = vec![{
        let mut model = Tool::default();
        model.name = "plasm_context".into();
        model.title = Some("Open or extend Plasm context".into());
        model.description = Some(PLASM_CONTEXT_TOOL_DESCRIPTION.into());
        model.input_schema = crate::mcp_server::schema::object_schema(
            vec!["session_mode".into(), "intent".into()],
            context_props,
        );
        model.annotations = Some({
            let mut model = ToolAnnotations::default();
            model.read_only_hint = Some(false);
            model.open_world_hint = Some(true);
            model
        });
        model.icons = None;
        model.meta = None;
        model.output_schema = None;
        model
    }];

    tools.push({
        let mut model = Tool::default();
        model.name = "plasm".into();
        model.title = Some("Plan Plasm (dry-run)".into());
        model.description = Some(PLASM_MCP_TOOL_DESCRIPTION.into());
        model.input_schema = crate::mcp_server::schema::object_schema(
            vec!["logical_session_ref".into(), "program".into()],
            plasm_program_props.clone(),
        );
        model.annotations = Some({
            let mut model = ToolAnnotations::default();
            model.read_only_hint = Some(true);
            model.open_world_hint = Some(true);
            model
        });
        model.icons = None;
        model.meta = if attach_ui_meta {
            Some(crate::plan_ui_mcp::plan_review_ui_tool_meta().into())
        } else {
            None
        };
        model.output_schema = if structured_ui_lane {
            Some(crate::mcp_ui_payload::plasm_tool_output_schema())
        } else {
            None
        };
        model
    });
    tools.push({
        let mut model = Tool::default();
        model.name = "plasm_run".into();
        model.title = Some("Run Plasm (execute)".into());
        model.description = Some(plasm_run_tool_description(artifact_access).into());
        model.input_schema = crate::mcp_server::schema::object_schema(
            vec!["logical_session_ref".into(), "run_ref".into()],
            plasm_run_props,
        );
        model.annotations = Some({
            let mut model = ToolAnnotations::default();
            model.read_only_hint = Some(false);
            model.open_world_hint = Some(true);
            model
        });
        model.icons = None;
        model.meta = if attach_ui_meta {
            Some(crate::run_explorer_ui_mcp::run_explorer_ui_tool_meta().into())
        } else {
            None
        };
        model.output_schema = if structured_ui_lane {
            Some(crate::mcp_ui_payload::plasm_run_tool_output_schema())
        } else {
            None
        };
        model
    });
    if attach_ui_meta {
        tools.push({
            let mut model = Tool::default();
            model.name = "plasm_ui_list_catalogs".into();
            model.title = Some("List tenant-enabled catalogs (MCP App)".into());
            model.description = Some(
            "Returns registry `entry_id`s allowed by tenant MCP policy for MCP App bootstrap UIs."
                .into(),
        );
            model.input_schema = crate::mcp_server::schema::object_schema(vec![], BTreeMap::new());
            model.annotations = Some({
                let mut model = ToolAnnotations::default();
                model.read_only_hint = Some(true);
                model.open_world_hint = Some(false);
                model
            });
            model.icons = None;
            model.meta = Some(
                serde_json::json!({
                    "ui": { "visibility": ["app"] }
                })
                .as_object()
                .cloned()
                .expect("plasm_ui_list_catalogs meta")
                .into(),
            );
            model.output_schema = None;
            model
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
        tools.push({
            let mut model = Tool::default();
            model.name = "plasm_read_run_artifact".into();
            model.title = Some("Read Plasm run snapshot".into());
            model.description = Some(PLASM_READ_RUN_ARTIFACT_TOOL_DESCRIPTION.into());
            model.input_schema = crate::mcp_server::schema::object_schema(
                vec!["logical_session_ref".into()],
                read_props,
            );
            model.annotations = Some({
                let mut model = ToolAnnotations::default();
                model.read_only_hint = Some(true);
                model.open_world_hint = Some(false);
                model
            });
            model.icons = None;
            model.meta = None;
            model.output_schema = None;
            model
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
    {
        let mut model = Tool::default();
        model.name = "plasm_ui_read_plan".into();
        model.title = Some("Hydrate plan DAG for MCP App view".into());
        model.description = Some(
            "App-only: returns `structuredContent.ui` with `comp` + `plan_ux_reflection` for Plan Review when the host forward omits the UI lane.".into(),
        );
        model.input_schema = crate::mcp_server::schema::object_schema(
            vec!["logical_session_ref".into(), "run_ref".into()],
            props,
        );
        model.annotations = Some({
            let mut model = ToolAnnotations::default();
            model.read_only_hint = Some(true);
            model.open_world_hint = Some(false);
            model
        });
        model.icons = None;
        model.meta = Some(app_only_tool_meta().into());
        model.output_schema = Some(crate::mcp_ui_payload::plasm_tool_output_schema());
        model
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
    {
        let mut model = Tool::default();
        model.name = "plasm_ui_read_run".into();
        model.title = Some("Hydrate run snapshot for MCP App view".into());
        model.description = Some(
            "App-only: returns `structuredContent.ui.steps[].preview_entities` when the host forward omits run rows.".into(),
        );
        model.input_schema =
            crate::mcp_server::schema::object_schema(vec!["logical_session_ref".into()], props);
        model.annotations = Some({
            let mut model = ToolAnnotations::default();
            model.read_only_hint = Some(true);
            model.open_world_hint = Some(false);
            model
        });
        model.icons = None;
        model.meta = Some(app_only_tool_meta().into());
        model.output_schema = Some(crate::mcp_ui_payload::plasm_run_tool_output_schema());
        model
    }
}
