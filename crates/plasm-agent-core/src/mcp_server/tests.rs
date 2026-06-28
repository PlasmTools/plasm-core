use super::mcp_plasm_invoke::McpPlasmRunTarget;
use super::*;

fn default_plasm_tools() -> Vec<rust_mcp_sdk::schema::Tool> {
    super::tools::plasm_tools(crate::mcp_run_markdown::ArtifactAccessMode::ResourcesRead)
}

#[test]
fn mcp_discover_maps_intent_to_capability_query() {
    let v = serde_json::json!({
        "intent": "Find electric type chart data for Pokemon",
    });
    let q = mcp_discover_query_from_arguments(&v).expect("deserialize");
    assert_eq!(q.tokens, vec!["Find electric type chart data for Pokemon"]);
    assert!(q.phrases.is_empty());
    assert!(q.entity_hints.is_empty());
    assert!(q.pick_entry.is_none());
    assert!(q.kinds.is_empty());
}

#[test]
fn mcp_discover_rejects_legacy_query_array() {
    let v = serde_json::json!({
        "query": ["github", "repository commits"],
    });
    let err = mcp_discover_query_from_arguments(&v).expect_err("legacy query rejected");
    assert!(
        err.contains("requires `intent`") && err.contains("not accepted"),
        "unexpected: {err}"
    );
}

#[test]
fn mcp_discover_rejects_non_string_intent() {
    let v = serde_json::json!({
        "intent": ["github", "commits"],
    });
    let err = mcp_discover_query_from_arguments(&v).expect_err("array intent rejected");
    assert!(err.contains("single string"), "unexpected: {err}");
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
    let resources = super::tools::plasm_tools(ArtifactAccessMode::ResourcesRead);
    assert!(
        !resources
            .iter()
            .any(|t| t.name == "plasm_read_run_artifact"),
        "default tool list must not expose read tool"
    );
    let tool_only = super::tools::plasm_tools(ArtifactAccessMode::ToolFallback);
    assert!(
        tool_only
            .iter()
            .any(|t| t.name == "plasm_read_run_artifact"),
        "tool-only list must expose read tool"
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
fn mcp_server_initialize_workflow_uses_intent_not_query() {
    let text = plasm_core::prompt_render::MCP_INITIALIZE_WORKFLOW;
    assert!(text.contains("one stable `intent`"));
    assert!(text.contains("one **`intent`** per goal"));
    assert!(!text.contains(plasm_core::prompt_render::TEACHING_VALID_EXPR_MARKER));
    assert!(!text.contains("Row text:"));
    assert!(!text.contains("Heredoc:"));
    assert!(text.contains("`intent`"));
    assert!(text.contains("One goal"));
    assert!(text.contains("one **`intent`** per goal"));
    assert!(!text.contains("several discovery calls"));
    assert!(!text.contains("pass **`query`**"));
    assert!(!text.contains("syntax guide in MCP initialize"));
    assert!(text.contains("Multi-API"));
    assert!(text.contains("Reuse ref"));
    let discover = default_plasm_tools()
        .into_iter()
        .find(|t| t.name == "discover_capabilities")
        .expect("discover_capabilities");
    let discover_desc = discover.description.as_deref().unwrap_or("");
    assert!(
        discover_desc.len() < 550,
        "discover tool description too long: {} chars",
        discover_desc.len()
    );
    assert!(!discover_desc.contains("query"));
}

#[test]
fn mcp_tool_descriptions_are_self_contained_without_initialize() {
    let plasm_desc = plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION;
    assert!(plasm_desc.contains(plasm_core::prompt_render::MCP_TOOL_SYNTAX_CONTRACT_MARKER));
    assert!(plasm_desc.contains("literal no-op"));
    assert!(plasm_desc.contains("<<TAG"));
    assert!(plasm_desc.contains("Row text:"));
    assert!(plasm_desc.contains("binding.content"));
    assert!(plasm_desc.contains(plasm_core::prompt_render::TEACHING_VALID_EXPR_MARKER));

    assert!(plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION
        .contains(plasm_core::prompt_render::MCP_TOOL_SYNTAX_CONTRACT_MARKER));
    assert!(plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION.contains("literal no-op"));
    assert!(plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION.contains("pcN"));
    assert!(
        plasm_core::prompt_render::PLASM_CONTEXT_TOOL_DESCRIPTION.contains("active symbol table")
    );
    assert!(
        plasm_core::prompt_render::PLASM_CONTEXT_TOOL_DESCRIPTION.contains("Call before `plasm`")
    );
    assert!(
        plasm_core::prompt_render::PLASM_CONTEXT_TOOL_DESCRIPTION.contains("msg 3: sort moves"),
        "expected stable-intent anti-pattern in plasm_context description"
    );
    assert!(
        plasm_core::prompt_render::PLASM_CONTEXT_TOOL_DESCRIPTION.contains("Call before `plasm`")
    );
    assert!(
        plasm_core::prompt_render::DISCOVER_TOOL_DESCRIPTION.contains("Plasm is a source language")
    );
    assert!(plasm_core::prompt_render::DISCOVER_TOOL_DESCRIPTION.contains("plasm.program"));
    assert!(plasm_core::prompt_render::DISCOVER_TOOL_DESCRIPTION.contains("alternate JSON"));
    assert!(
        plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION.contains("do **not** echo the program")
    );
    assert!(!plasm_core::prompt_render::PLASM_RUN_TOOL_DESCRIPTION.contains("echo the program"));
    assert!(plasm_core::prompt_render::PLASM_PROGRAM_PARAM_DESCRIPTION.contains("not JSON data"));
    assert!(plasm_core::prompt_render::PLASM_PROGRAM_PARAM_DESCRIPTION
        .contains("e3(p15=\"value\").r2[p4]"));
    assert!(!plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION.contains("MCP initialize"));
    assert!(!plasm_core::prompt_render::PLASM_CONTEXT_TOOL_DESCRIPTION.contains("MCP initialize"));
    for tool in default_plasm_tools() {
        let desc = tool.description.as_deref().unwrap_or("");
        assert!(
            !desc.contains("MCP initialize"),
            "{} description leaks hidden initialize dependency",
            tool.name
        );
        if tool.name == "discover_capabilities" {
            let v = serde_json::to_value(tool.input_schema.clone()).expect("discover schema");
            let props = v
                .get("properties")
                .and_then(|p| p.as_object())
                .expect("discover schema properties");
            assert!(
                !props.contains_key("typed"),
                "discover_capabilities must not expose typed to agents"
            );
            assert!(
                !props.contains_key("allowed_entry_ids"),
                "discover_capabilities must not expose allowed_entry_ids to agents"
            );
        }
    }
    let tools_json = serde_json::to_string(&default_plasm_tools()).expect("serialize tools");
    assert!(!tools_json.contains("MCP initialize"));
}

/// Static MCP tool descriptions carry canonical grammar; initialize workflow stays supplementary.
#[test]
fn mcp_prompt_static_tool_descriptions() {
    let init = plasm_core::prompt_render::MCP_INITIALIZE_WORKFLOW;
    assert!(
        init.len() < 2500,
        "initialize instructions too long: {} chars",
        init.len()
    );
    assert!(
        plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION.len() < 8000,
        "plasm tool description too long: {} chars",
        plasm_core::prompt_render::PLASM_TOOL_DESCRIPTION.len()
    );
    assert!(
        plasm_core::prompt_render::PLASM_CONTEXT_TOOL_DESCRIPTION.len() < 1400,
        "plasm_context tool description too long: {} chars",
        plasm_core::prompt_render::PLASM_CONTEXT_TOOL_DESCRIPTION.len()
    );
    assert!(
        plasm_core::prompt_render::DISCOVER_TOOL_DESCRIPTION.len() < 550,
        "discover tool description too long: {} chars",
        plasm_core::prompt_render::DISCOVER_TOOL_DESCRIPTION.len()
    );
    assert!(
        plasm_core::prompt_render::PLASM_PROGRAM_PARAM_DESCRIPTION.len() < 200,
        "program param description too long: {} chars",
        plasm_core::prompt_render::PLASM_PROGRAM_PARAM_DESCRIPTION.len()
    );
    let tools = default_plasm_tools();
    let v = serde_json::to_value(
        tools
            .iter()
            .find(|t| t.name == "plasm")
            .expect("plasm tool")
            .input_schema
            .clone(),
    )
    .expect("input_schema json");
    let program_desc = v
        .get("properties")
        .and_then(|p| p.get("program"))
        .and_then(|s| s.get("description"))
        .and_then(|d| d.as_str())
        .expect("plasm program param description");
    assert_eq!(
        program_desc,
        plasm_core::prompt_render::PLASM_PROGRAM_PARAM_DESCRIPTION
    );
}

#[test]
fn mcp_tool_list_hides_internal_auth_and_registry_tools() {
    let names: Vec<String> = default_plasm_tools().into_iter().map(|t| t.name).collect();
    assert!(!names.iter().any(|n| n == "plasm_incoming_auth"));
    assert!(!names.iter().any(|n| n == "list_registry"));
    assert!(names.iter().any(|n| n == "plasm_context"));
    assert!(names.iter().any(|n| n == "discover_capabilities"));
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
        desc.contains("**Extend picks:**"),
        "expected append guidance in plasm_context description"
    );
    assert!(
        workflow.contains("one stable `intent`"),
        "expected session discipline in initialize workflow"
    );
    assert!(
        workflow.contains("Reuse ref"),
        "expected steady-state guidance in initialize workflow"
    );
}

#[test]
fn discover_capabilities_input_schema() {
    let tools = default_plasm_tools();
    let discover = tools
        .iter()
        .find(|t| t.name == "discover_capabilities")
        .expect("discover_capabilities tool");
    let v = serde_json::to_value(&discover.input_schema).expect("input_schema json");
    let required = v
        .get("required")
        .and_then(|x| x.as_array())
        .expect("required array");
    assert_eq!(required.len(), 1);
    assert_eq!(required[0].as_str(), Some("intent"));
    let props = v
        .get("properties")
        .and_then(|x| x.as_object())
        .expect("properties object");
    assert!(props.contains_key("intent"));
    assert!(!props.contains_key("typed"));
    assert!(!props.contains_key("allowed_entry_ids"));
    assert!(!props.contains_key("query"));
    with_insta_snapshots(|| {
        insta::assert_json_snapshot!("discover_capabilities_input_schema", v);
    });
}

#[test]
fn plasm_context_input_schema_requires_intent_and_seeds() {
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
    assert!(required.iter().any(|x| x.as_str() == Some("intent")));
    assert!(required.iter().any(|x| x.as_str() == Some("seeds")));
    assert!(!required
        .iter()
        .any(|x| x.as_str() == Some("client_session_key")));
    let props = v
        .get("properties")
        .and_then(|x| x.as_object())
        .expect("properties object");
    assert!(
        props.contains_key("intent"),
        "expected `intent` property, got keys: {:?}",
        props.keys().collect::<Vec<_>>()
    );
    assert!(!props.contains_key("client_session_key"));
    assert_eq!(
        props
            .get("intent")
            .and_then(|x| x.get("type"))
            .and_then(|x| x.as_str()),
        Some("string")
    );
    assert!(
        props.contains_key("ranked_capabilities"),
        "expected optional ranked_capabilities on plasm_context"
    );
}

/// MCP hosts (e.g. Cursor) may validate `tools/call` args against the advertised JSON Schema
/// from `tools/list`. Discovery accepts one `intent` string only; array-shaped `query` is a
/// removed interface, not a compatibility path.
#[test]
fn discover_capabilities_input_schema_requires_single_intent_string() {
    let tools = default_plasm_tools();
    let discover = tools
        .iter()
        .find(|t| t.name == "discover_capabilities")
        .expect("discover_capabilities tool");
    let v = serde_json::to_value(&discover.input_schema).expect("input_schema json");
    let required = v
        .get("required")
        .and_then(|x| x.as_array())
        .expect("required array");
    assert!(required.iter().any(|x| x.as_str() == Some("intent")));
    let props = v.get("properties").and_then(|p| p.as_object()).unwrap();
    assert!(!props.contains_key("query"));
    assert!(!props.contains_key("utterance"));
    let intent = v
        .get("properties")
        .and_then(|p| p.get("intent"))
        .expect("intent property in input_schema");
    assert_eq!(intent.get("type").and_then(|x| x.as_str()), Some("string"));
    assert_eq!(intent.get("minLength").and_then(|x| x.as_u64()), Some(1));
}

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
fn mcp_discover_ignores_unknown_json_keys() {
    let v = serde_json::json!({
        "intent": "find x resources",
        "kinds": ["query"],
    });
    let q = mcp_discover_query_from_arguments(&v).expect("deserialize");
    assert_eq!(q.tokens, vec!["find x resources"]);
    assert!(q.kinds.is_empty());
}

/// Reference output for `discover_capabilities` Markdown (fenced tabular block; same columns as discovery).
#[test]
fn discover_markdown_emits_tsv_snapshot() {
    use plasm_core::discovery::{CapabilityQuery, DiscoveryResult, EntitySummary, RankedCandidate};
    let r = DiscoveryResult {
        contexts: vec![],
        candidates: vec![RankedCandidate {
            entry_id: "demo".into(),
            entity: "Widget".into(),
            capability_name: "list".into(),
            score: 2,
            reason_codes: vec![],
            capability_description: "List widgets".into(),
        }],
        ambiguities: vec![],
        applied_query_echo: CapabilityQuery::default(),
        closure_stats: None,
        schema_neighborhoods: vec![],
        entity_summaries: vec![EntitySummary {
            entry_id: "demo".into(),
            name: "Widget".into(),
            description: " A contrived \t widget \n line ".into(),
        }],
    };
    with_insta_snapshots(|| {
        insta::assert_snapshot!(
            "discover_markdown_emits_tsv_snapshot",
            crate::discovery_human_format::format_discovery_markdown(&r)
        );
    });
}

#[test]
fn plasm_context_requires_non_empty_seeds() {
    let err = parse_tool_seeds("plasm_context", &serde_json::json!({ "seeds": [] }))
        .expect_err("expected invalid seeds");
    assert!(
        err.to_string().contains("non-empty array"),
        "unexpected error: {err}"
    );
}

#[test]
fn plasm_context_legacy_shape_returns_actionable_error() {
    let err = parse_tool_seeds(
        "plasm_context",
        &serde_json::json!({ "entry_id": "pokeapi", "entities": ["Pokemon"] }),
    )
    .expect_err("expected invalid legacy shape");
    assert!(
        err.to_string()
            .contains("old top-level `{entry_id, entities}`"),
        "unexpected error: {err}"
    );
}

#[test]
fn plasm_context_seeds_accept_api_or_entry_id_alias() {
    let api = parse_tool_seeds(
        "plasm_context",
        &serde_json::json!({ "seeds": [{ "api": "pokeapi", "entity": "Pokemon" }] }),
    )
    .expect("api key");
    assert_eq!(api.len(), 1);
    assert_eq!(api[0].entry_id, "pokeapi");
    assert_eq!(api[0].entity, "Pokemon");

    let legacy = parse_tool_seeds(
        "plasm_context",
        &serde_json::json!({ "seeds": [{ "entry_id": "pokeapi", "entity": "Pokemon" }] }),
    )
    .expect("entry_id alias");
    assert_eq!(legacy.len(), 1);
    assert_eq!(legacy[0].entry_id, "pokeapi");
}
