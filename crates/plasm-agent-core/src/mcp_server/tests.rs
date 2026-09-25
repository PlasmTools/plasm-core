use super::mcp_plasm_invoke::McpPlasmRunTarget;

fn default_plasm_tools() -> Vec<rust_mcp_sdk::schema::Tool> {
    super::tools::plasm_tools(
        crate::mcp_run_markdown::ArtifactAccessMode::ResourcesRead,
        true,
    )
}

#[test]
fn plasm_tools_omit_ui_metadata_when_apps_disabled() {
    let tools = super::tools::plasm_tools(
        crate::mcp_run_markdown::ArtifactAccessMode::ResourcesRead,
        false,
    );
    let plasm = tools.iter().find(|t| t.name == "plasm").expect("plasm");
    let plasm_run = tools
        .iter()
        .find(|t| t.name == "plasm_run")
        .expect("plasm_run");
    assert!(plasm.meta.is_none());
    assert!(plasm_run.meta.is_none());
    assert!(!tools.iter().any(|t| t.name == "plasm_ui_read_plan"));
}

#[test]
fn plasm_tools_include_ui_metadata_when_apps_enabled() {
    let tools = super::tools::plasm_tools(
        crate::mcp_run_markdown::ArtifactAccessMode::ResourcesRead,
        true,
    );
    let plasm = tools.iter().find(|t| t.name == "plasm").expect("plasm");
    assert_eq!(
        plasm
            .meta
            .as_ref()
            .and_then(|m| m.get("ui"))
            .and_then(|u| u.get("resourceUri"))
            .and_then(|v| v.as_str()),
        Some(crate::plan_ui_mcp::PLAN_REVIEW_UI_URI)
    );
    assert!(tools.iter().any(|t| t.name == "plasm_ui_read_plan"));
}

#[test]
fn parse_logical_session_ref_rejects_legacy_slot() {
    let err = super::parse_logical_session_ref_arg(
        "plasm",
        &serde_json::json!({ "logical_session_ref": "s0" }),
    )
    .expect_err("legacy slot");
    assert!(err.to_string().contains("legacy transport slot"));
}

#[test]
fn parse_logical_session_ref_accepts_wire_ref() {
    let wire = "l_AAAAAAAAQACAAAAAAAAAAQ";
    let got = super::parse_logical_session_ref_arg(
        "plasm",
        &serde_json::json!({ "logical_session_ref": wire }),
    )
    .expect("wire ref");
    assert_eq!(got, wire);
}

#[test]
fn resolve_logical_session_wire_ref_round_trip() {
    let wire = "l_AAAAAAAAQACAAAAAAAAAAQ";
    let id = crate::mcp_logical_ref::parse_logical_session_wire_ref(wire).expect("parse");
    assert_eq!(
        crate::mcp_logical_ref::format_logical_session_wire_ref(id),
        wire
    );
}

#[test]
fn plasm_invocation_char_count_sums_program_and_reasoning() {
    assert_eq!(super::plasm_invocation_char_count("a", None), 1);
    assert_eq!(super::plasm_invocation_char_count("a", Some("#c")), 1 + 2);
}

/// Serialize Insta reads/writes under `src/snapshots` (parallel `cargo nextest` threads otherwise flake).
fn with_insta_snapshots<R>(f: impl FnOnce() -> R) -> R {
    static INSTA_SNAPSHOT_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = INSTA_SNAPSHOT_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut settings = insta::Settings::clone_current();
    settings
        .set_snapshot_path(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/snapshots"));
    settings.bind(f)
}

/// Model-facing copy; update with `just update-insta-snapshots` (not `INSTA_UPDATE=always` in your shell).
#[test]
fn mcp_plasm_tool_description_snapshot() {
    with_insta_snapshots(|| {
        insta::assert_snapshot!(
            "mcp_plasm_tool_description",
            plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION
        );
    });
}

#[test]
fn mcp_plasm_run_tool_description_snapshot() {
    with_insta_snapshots(|| {
        insta::assert_snapshot!(
            "mcp_plasm_run_tool_description",
            plasm_core::prompt_render::PLASM_RUN_TOOL_DESCRIPTION
        );
    });
}

#[test]
fn plasm_read_run_artifact_gated_on_tool_fallback_mode() {
    use crate::mcp_run_markdown::ArtifactAccessMode;
    let resources = super::tools::plasm_tools(ArtifactAccessMode::ResourcesRead, true);
    assert!(
        !resources
            .iter()
            .any(|t| t.name == "plasm_read_run_artifact"),
        "default tool list must not expose read tool"
    );
    let tool_only = super::tools::plasm_tools(ArtifactAccessMode::ToolFallback, true);
    assert!(
        tool_only
            .iter()
            .any(|t| t.name == "plasm_read_run_artifact"),
        "tool-only list must expose read tool"
    );
}

#[test]
fn tool_fallback_omits_output_schema_but_keeps_ui_resource_meta() {
    use crate::mcp_run_markdown::ArtifactAccessMode;
    let tool_only = super::tools::plasm_tools(ArtifactAccessMode::ToolFallback, true);
    let plasm = tool_only
        .iter()
        .find(|t| t.name == "plasm")
        .expect("plasm tool");
    let plasm_run = tool_only
        .iter()
        .find(|t| t.name == "plasm_run")
        .expect("plasm_run tool");
    assert!(
        plasm.output_schema.is_none(),
        "ToolFallback must not advertise structuredContent outputSchema"
    );
    assert!(
        plasm_run.output_schema.is_none(),
        "ToolFallback must not advertise structuredContent outputSchema"
    );
    assert!(
        plasm
            .meta
            .as_ref()
            .and_then(|m| m.get("ui"))
            .and_then(|u| u.get("resourceUri"))
            .is_some(),
        "ToolFallback may still mount Plan Review via _meta.ui.resourceUri"
    );
    let full = super::tools::plasm_tools(ArtifactAccessMode::ResourcesRead, true);
    let full_plasm = full.iter().find(|t| t.name == "plasm").expect("plasm");
    assert!(
        full_plasm.output_schema.is_some(),
        "full Apps lane must advertise outputSchema"
    );
}

#[test]
fn mcp_server_initialize_instructions_snapshot() {
    with_insta_snapshots(|| {
        insta::assert_snapshot!(
            "mcp_server_initialize_instructions",
            plasm_core::prompt_render::MCP_INITIALIZE_WORKFLOW
        );
    });
}

#[test]
fn mcp_server_initialize_workflow_uses_session_mode_not_intent_key() {
    let text = plasm_core::prompt_render::MCP_INITIALIZE_WORKFLOW;
    assert!(text.contains("session_mode"));
    assert!(text.contains("logical_session_ref"));
    assert!(!text.contains(plasm_core::prompt_render::TEACHING_VALID_EXPR_MARKER));
    assert!(!text.contains("Row text:"));
    assert!(!text.contains("Heredoc:"));
    assert!(text.contains("`intent`"));
    assert!(!text.contains("one stable `intent`"));
    assert!(!text.contains("several discovery calls"));
    assert!(!text.contains("pass **`query`**"));
    assert!(!text.contains("syntax guide in MCP initialize"));
    assert!(text.contains("Reuse it for `plasm` / `plasm_run`"));
    assert!(text.contains("no tool call"));
    assert!(!text.contains("until observation matches"));
    assert!(!text.contains("Then reply with exactly: DONE"));
    assert!(!text.contains("discover_capabilities` first only"));
    assert!(!text.contains("routing_ref") && !text.contains("clarify_choices"));
    assert!(text.contains("declared prerequisites"));
    assert!(!text.contains("provider brand"));
    assert!(!text.contains("semantic-auto-seed"));
}

#[test]
fn mcp_tool_descriptions_are_self_contained_without_initialize() {
    let plasm_desc = plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION;
    assert!(plasm_desc.contains(plasm_core::prompt_render::MCP_TOOL_SYNTAX_CONTRACT_MARKER));
    assert!(plasm_desc.contains("logical_session_ref") && plasm_desc.contains("run_ref"));
    assert!(plasm_desc.contains("@compute"));
    assert!(plasm_desc.contains("incremental domain declarations"));
    assert!(plasm_desc.contains(plasm_core::prompt_render::TEACHING_VALID_EXPR_MARKER));

    assert!(plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION
        .contains(plasm_core::prompt_render::MCP_TOOL_SYNTAX_CONTRACT_MARKER));
    assert!(plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION.contains("run_ref"));
    assert!(plasm_core::prompt_render::PLASM_CONTEXT_TOOL_DESCRIPTION.contains("symbol table"));
    assert!(
        plasm_core::prompt_render::PLASM_CONTEXT_TOOL_DESCRIPTION.contains("Call before `plasm`")
    );
    assert!(plasm_core::prompt_render::PLASM_CONTEXT_TOOL_DESCRIPTION.contains("session_mode"));
    assert!(plasm_core::prompt_render::PLASM_CONTEXT_TOOL_DESCRIPTION
        .contains("does not select the session"));
    assert!(
        plasm_core::prompt_render::PLASM_CONTEXT_TOOL_DESCRIPTION.contains("Call before `plasm`")
    );
    assert!(plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION.contains("do not echo the program"));
    assert!(!plasm_core::prompt_render::PLASM_RUN_TOOL_DESCRIPTION.contains("echo the program"));
    let program_param_violations = plasm_core::prompt_render::program_param_contract_violations(
        plasm_core::prompt_render::PLASM_PROGRAM_PARAM_DESCRIPTION,
    );
    assert!(
        program_param_violations.is_empty(),
        "program param contract violations: {program_param_violations:?}"
    );
    assert!(!plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION.contains("MCP initialize"));
    assert!(!plasm_core::prompt_render::PLASM_CONTEXT_TOOL_DESCRIPTION.contains("MCP initialize"));
    for tool in default_plasm_tools() {
        let desc = tool.description.as_deref().unwrap_or("");
        assert!(
            !desc.contains("MCP initialize"),
            "{} description leaks hidden initialize dependency",
            tool.name
        );
    }
    let tools_json = serde_json::to_string(&default_plasm_tools()).expect("serialize tools");
    assert!(!tools_json.contains("MCP initialize"));
}

/// MCP `tools/list` must serve the full grammar and program contracts byte-for-byte.
/// Host-side clipping (e.g. Cursor) is a client concern; the server must never be the source.
#[test]
fn mcp_tool_list_wire_carries_full_untruncated_descriptions() {
    let tools = default_plasm_tools();
    let plasm = tools
        .iter()
        .find(|t| t.name == "plasm")
        .expect("plasm tool");
    assert_eq!(
        plasm.description.as_deref(),
        Some(plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION),
        "served plasm tool description must equal the full asset byte-for-byte"
    );

    let schema = serde_json::to_value(plasm.input_schema.clone()).expect("plasm input_schema json");
    let program_desc = schema
        .get("properties")
        .and_then(|p| p.get("program"))
        .and_then(|s| s.get("description"))
        .and_then(|d| d.as_str())
        .expect("plasm program param description");
    assert_eq!(
        program_desc,
        plasm_core::prompt_render::PLASM_PROGRAM_PARAM_DESCRIPTION,
        "served program param description must equal the full asset byte-for-byte"
    );

    // Wire JSON must embed the full strings (guards serializers that clip long fields).
    let tools_json = serde_json::to_string(&tools).expect("serialize tools");
    for (label, asset) in [
        (
            "plasm tool description",
            plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION,
        ),
        (
            "program param description",
            plasm_core::prompt_render::PLASM_PROGRAM_PARAM_DESCRIPTION,
        ),
    ] {
        let escaped = serde_json::to_string(asset).expect("escape asset");
        let body = escaped
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .expect("serde_json::to_string wraps strings in quotes");
        assert!(
            tools_json.contains(body),
            "serialized tools/list must contain the full {label}"
        );
    }
}

#[test]
fn mcp_tool_list_hides_internal_auth_and_registry_tools() {
    let names: Vec<String> = default_plasm_tools().into_iter().map(|t| t.name).collect();
    assert!(!names.iter().any(|n| n == "plasm_incoming_auth"));
    assert!(!names.iter().any(|n| n == "list_registry"));
    assert!(names.iter().any(|n| n == "plasm_context"));
    assert!(!names.iter().any(|n| n == "discover_capabilities"));
    let removed_init_tool = format!("plasm_{}", "session_init");
    let removed_add_tool = format!("add_{}", "capabilities");
    assert!(!names.iter().any(|n| n == &removed_init_tool));
    assert!(!names.iter().any(|n| n == &removed_add_tool));
    assert!(!names.iter().any(|n| n == "add_code_capabilities"));
    assert!(!names.iter().any(|n| n == "evaluate_code_plan"));
    assert!(!names.iter().any(|n| n == "execute_code_plan"));
    assert!(!names.iter().any(|n| n == "execute"));
    for workflow_tool in ["open_workflow", "dry_workflow", "run_workflow"] {
        assert!(
            !names.iter().any(|n| n == workflow_tool),
            "{workflow_tool} must remain feature-gated off by default"
        );
    }
    assert!(names.iter().any(|n| n == "plasm"));
    assert!(names.iter().any(|n| n == "plasm_run"));
}

#[test]
fn plasm_context_tool_description_snapshot() {
    with_insta_snapshots(|| {
        let tools = default_plasm_tools();
        let context = tools
            .iter()
            .find(|t| t.name == "plasm_context")
            .and_then(|t| t.description.as_ref())
            .expect("plasm_context description")
            .clone();
        insta::assert_snapshot!("plasm_context_tool_description", context);
    });
}

#[test]
fn plasm_context_tool_description_contract_append_vs_refresh() {
    let tools = default_plasm_tools();
    let desc = tools
        .iter()
        .find(|t| t.name == "plasm_context")
        .and_then(|t| t.description.as_deref())
        .expect("plasm_context description");
    let workflow = plasm_core::prompt_render::MCP_INITIALIZE_WORKFLOW;
    assert!(
        desc.contains("session_mode: \"extend\"")
            && !desc.contains("effect_slots")
            && desc.contains("Jev judges relevance"),
        "expected relevance guidance in plasm_context description"
    );
    assert!(
        workflow.contains("session_mode"),
        "expected session_mode discipline in initialize workflow"
    );
    assert!(
        !workflow.contains("one stable `intent`"),
        "initialize workflow must not treat intent as session key"
    );
    assert!(
        workflow.contains("Reuse it for `plasm` / `plasm_run`"),
        "expected steady-state guidance in initialize workflow"
    );
}

#[test]
fn plasm_context_input_schema_requires_intent_not_effect_slots_or_seeds() {
    let tools = default_plasm_tools();
    let ctx = tools
        .iter()
        .find(|t| t.name == "plasm_context")
        .expect("plasm_context tool");
    let v = serde_json::to_value(&ctx.input_schema).expect("input_schema json");
    let required = v
        .get("required")
        .and_then(|x| x.as_array())
        .expect("required array");
    assert!(required.iter().any(|x| x.as_str() == Some("session_mode")));
    assert!(required.iter().any(|x| x.as_str() == Some("intent")));
    assert!(!required.iter().any(|x| x.as_str() == Some("effect_slots")));
    assert!(!required.iter().any(|x| x.as_str() == Some("seeds")));
    assert!(!required
        .iter()
        .any(|x| x.as_str() == Some("client_session_key")));
    let props = v
        .get("properties")
        .and_then(|x| x.as_object())
        .expect("properties object");
    assert!(
        props.contains_key("session_mode"),
        "expected `session_mode` property"
    );
    assert!(
        props.contains_key("logical_session_ref"),
        "expected optional `logical_session_ref` property"
    );
    assert!(!props.contains_key("client_session_key"));
    assert_eq!(
        props
            .get("intent")
            .and_then(|x| x.get("type"))
            .and_then(|x| x.as_str()),
        Some("string")
    );
    assert!(!props.contains_key("effect_slots"));
    assert!(
        !props.contains_key("ranked_capabilities"),
        "caller-supplied rankings must not bypass the capability selector"
    );
}

/// MCP hosts may validate `tools/call` args against the advertised JSON Schema.
/// Discovery accepts one original `intent` plus explicit effect slots; array-shaped
/// legacy `query` remains a removed interface.

#[test]
fn plasm_input_schema_advertises_single_program_string() {
    let tools = default_plasm_tools();
    let plasm = tools
        .iter()
        .find(|t| t.name == "plasm")
        .expect("plasm tool");
    let v = serde_json::to_value(&plasm.input_schema).expect("input_schema json");
    let required = v
        .get("required")
        .and_then(|x| x.as_array())
        .expect("required array");
    assert!(required.iter().any(|x| x.as_str() == Some("program")));
    assert!(!required.iter().any(|x| x.as_str() == Some("expressions")));
    let props = v
        .get("properties")
        .and_then(|x| x.as_object())
        .expect("properties object");
    assert_eq!(
        props
            .get("program")
            .and_then(|x| x.get("type"))
            .and_then(|x| x.as_str()),
        Some("string")
    );
    assert!(!props.contains_key("expressions"));
    assert!(
        !props.contains_key("execute"),
        "`plasm` input_schema must not advertise `execute` (use `plasm_run` for live execution)"
    );
}

#[test]
fn plasm_run_invocation_rejects_program_and_wait_arguments() {
    for (key, value, expected) in [
        (
            "program",
            serde_json::json!("e1"),
            "no longer accepts `program`",
        ),
        ("wait", serde_json::json!(false), "does not accept `wait`"),
        (
            "cancel",
            serde_json::json!(true),
            "does not accept `cancel`",
        ),
        ("force", serde_json::json!(true), "does not accept `force`"),
        (
            "execute",
            serde_json::json!(true),
            "does not accept `execute`",
        ),
    ] {
        let mut args = serde_json::json!({
            "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
            "run_ref": "pc0"
        });
        args.as_object_mut()
            .expect("object args")
            .insert(key.into(), value);
        let err = match super::parse_mcp_plasm_invocation("plasm_run", &args, false) {
            Ok(_) => panic!("{key} should be rejected"),
            Err(err) => err,
        };
        let rendered = format!("{err:?}");
        assert!(
            rendered.contains(expected),
            "unexpected {key} error: {rendered}"
        );
    }
}

#[test]
fn plasm_run_invocation_accepts_run_ref_pc_or_page_handle() {
    let commit = super::parse_mcp_plasm_invocation(
        "plasm_run",
        &serde_json::json!({
            "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
            "run_ref": "pc12"
        }),
        false,
    )
    .expect("commit invocation");
    let Some(McpPlasmRunTarget::Commit(pc)) = commit.run_target() else {
        panic!("expected commit invocation");
    };
    assert_eq!(pc.as_str(), "pc12");
    assert!(commit.program().is_none());

    let page = super::parse_mcp_plasm_invocation(
        "plasm_run",
        &serde_json::json!({
            "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
            "run_ref": "l_AAAAAAAAQACAAAAAAAAAAQ_pg3"
        }),
        false,
    )
    .expect("page invocation");
    match page.run_target() {
        Some(McpPlasmRunTarget::Page(h)) => {
            assert_eq!(h.as_str(), "l_AAAAAAAAQACAAAAAAAAAAQ_pg3");
        }
        _ => panic!("expected page invocation"),
    }
}

#[test]
fn plasm_run_rejects_deprecated_transitional_params() {
    let page_err = match super::parse_mcp_plasm_invocation(
        "plasm_run",
        &serde_json::json!({
            "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
            "page_handle": "l_AAAAAAAAQACAAAAAAAAAAQ_pg1"
        }),
        false,
    ) {
        Ok(_) => panic!("page_handle param should be rejected"),
        Err(err) => format!("{err:?}"),
    };
    assert!(
        page_err.contains("page_handle"),
        "unexpected error: {page_err}"
    );
    assert!(page_err.contains("run_ref"), "unexpected error: {page_err}");

    let pc_err = match super::parse_mcp_plasm_invocation(
        "plasm_run",
        &serde_json::json!({
            "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
            "plan_commit_ref": "pc0"
        }),
        false,
    ) {
        Ok(_) => panic!("plan_commit_ref param should be rejected"),
        Err(err) => format!("{err:?}"),
    };
    assert!(
        pc_err.contains("plan_commit_ref"),
        "unexpected error: {pc_err}"
    );
    assert!(pc_err.contains("run_ref"), "unexpected error: {pc_err}");
}

#[test]
fn parse_plasm_context_session_mode_new_and_extend() {
    use super::tool_parse::parse_plasm_context_session_mode;

    let (mode, r) = parse_plasm_context_session_mode(
        "plasm_context",
        &serde_json::json!({
            "session_mode": "new",
            "intent": "goal",
            "seeds": [{"api": "x", "entity": "Y"}]
        }),
    )
    .expect("new");
    assert_eq!(mode, crate::session_identity::PlasmContextSessionMode::New);
    assert!(r.is_none());

    parse_plasm_context_session_mode(
        "plasm_context",
        &serde_json::json!({
            "session_mode": "new",
            "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
            "intent": "goal",
            "seeds": [{"api": "x", "entity": "Y"}]
        }),
    )
    .expect_err("new rejects ref");

    let (mode, r) = parse_plasm_context_session_mode(
        "plasm_context",
        &serde_json::json!({
            "session_mode": "extend",
            "logical_session_ref": "l_AAAAAAAAQACAAAAAAAAAAQ",
            "intent": "goal",
            "seeds": [{"api": "x", "entity": "Y"}]
        }),
    )
    .expect("extend");
    assert_eq!(
        mode,
        crate::session_identity::PlasmContextSessionMode::Extend
    );
    assert_eq!(r.as_deref(), Some("l_AAAAAAAAQACAAAAAAAAAAQ"));
}

#[test]
fn context_tool_has_intent_slots_and_typed_continuation_without_seed_selection() {
    let tools = super::tools::plasm_tools(
        crate::mcp_run_markdown::ArtifactAccessMode::ResourcesRead,
        false,
    );
    let tool = tools
        .iter()
        .find(|tool| tool.name == "plasm_context")
        .unwrap();
    let schema = serde_json::to_value(&tool.input_schema).unwrap();
    assert!(schema["properties"].get("effect_slots").is_none());
    assert!(schema["properties"].get("seeds").is_none());
    assert!(schema["properties"].get("ranked_capabilities").is_none());
    assert!(schema["properties"].get("clarify_choices").is_none());
    assert!(!tools
        .iter()
        .any(|tool| tool.name == "discover_capabilities"));
}
