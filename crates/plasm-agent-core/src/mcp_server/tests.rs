use super::*;
use insta::assert_snapshot;

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
        settings.set_snapshot_path(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/snapshots"),
        );
        settings.bind(f)
    }

    /// Model-facing copy; update with `just update-insta-snapshots` (not `INSTA_UPDATE=always` in your shell).
    #[test]
    fn mcp_plasm_tool_description_snapshot() {
        with_insta_snapshots(|| {
            insta::assert_snapshot!(
                "mcp_plasm_tool_description",
                super::MCP_PLASM_TOOL_DESCRIPTION
            );
        });
    }

    #[test]
    fn mcp_plasm_run_tool_description_snapshot() {
        with_insta_snapshots(|| {
            insta::assert_snapshot!(
                "mcp_plasm_run_tool_description",
                super::MCP_PLASM_RUN_TOOL_DESCRIPTION
            );
        });
    }

    #[test]
    fn mcp_server_initialize_instructions_snapshot() {
        with_insta_snapshots(|| {
            insta::assert_snapshot!(
                "mcp_server_initialize_instructions",
                super::mcp_server_initialize_instructions()
            );
        });
    }

    #[test]
    fn mcp_server_initialize_workflow_uses_intent_not_query() {
        let text = super::mcp_server_initialize_workflow();
        assert!(text.contains(plasm_core::prompt_render::SESSION_DISCIPLINE_MCP));
        assert!(text.contains(plasm_core::prompt_render::SESSION_DISCIPLINE_PROGRAM));
        assert!(text.contains(plasm_core::prompt_render::TEACHING_VALID_EXPR_MARKER));
        assert!(text.contains("Row text:"));
        assert!(text.contains("Heredoc:"));
        assert!(text.contains("`intent`"));
        assert!(text.contains("One goal"));
        assert!(text.contains("one **`intent`** per goal"));
        assert!(!text.contains("several discovery calls"));
        assert!(!text.contains("pass **`query`**"));
        assert!(!text.contains("syntax guide in MCP initialize"));
        assert!(text.contains("Multi-API"));
        assert!(text.contains("Reuse ref"));
        let discover = super::PlasmMcpHandler::plasm_tools()
            .into_iter()
            .find(|t| t.name == "discover_capabilities")
            .expect("discover_capabilities");
        let discover_desc = discover.description.as_deref().unwrap_or("");
        assert!(
            discover_desc.len() < 400,
            "discover tool description too long"
        );
        assert!(!discover_desc.contains("query"));
    }

    /// Static MCP prompt byte budgets (UTF-8 byte length). Targets from MCP prompt dedup plan.
    #[test]
    fn mcp_prompt_char_budget() {
        let init = super::mcp_server_initialize_instructions();
        let workflow = super::mcp_server_initialize_workflow();
        assert!(
            init.len() < 6500,
            "initialize instructions too long: {} chars",
            init.len()
        );
        let head = include_str!("../mcp_prompt/workflow_head.txt");
        let tail = include_str!("../mcp_prompt/workflow_tail.txt");
        let async_poll = crate::operation_progress::ASYNC_POLL_DISCIPLINE_MCP_LINE;
        assert!(
            head.len() < 950,
            "workflow_head too long: {} chars",
            head.len()
        );
        let grammar = plasm_core::prompt_render::render_plasm_mcp_language_frontmatter();
        assert!(
            grammar.len() < 5500,
            "grammar too long: {} chars",
            grammar.len()
        );
        assert!(
            init.len().saturating_sub(grammar.len()) < 2000,
            "initialize non-grammar workflow too long: init={} grammar={}",
            init.len(),
            grammar.len()
        );
        assert!(
            tail.len() + async_poll.len() < 1500,
            "workflow_tail + async poll too long: {} chars",
            tail.len() + async_poll.len()
        );
        assert_eq!(
            init.len(),
            workflow.len() + async_poll.len(),
            "initialize instructions must equal workflow + async poll"
        );
        assert!(
            super::MCP_PLASM_TOOL_DESCRIPTION.len() < 1200,
            "plasm tool description too long: {} chars",
            super::MCP_PLASM_TOOL_DESCRIPTION.len()
        );
        assert!(
            super::mcp_plasm_context_tool_description().len() < 950,
            "plasm_context tool description too long: {} chars",
            super::mcp_plasm_context_tool_description().len()
        );
        assert!(
            super::MCP_PROGRAM_PARAM_DESCRIPTION.len() < 200,
            "program param description too long: {} chars",
            super::MCP_PROGRAM_PARAM_DESCRIPTION.len()
        );
        let tools = super::PlasmMcpHandler::plasm_tools();
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
        assert_eq!(program_desc, super::MCP_PROGRAM_PARAM_DESCRIPTION);
    }

    #[test]
    fn mcp_tool_list_hides_internal_auth_and_registry_tools() {
        let names: Vec<String> = super::PlasmMcpHandler::plasm_tools()
            .into_iter()
            .map(|t| t.name)
            .collect();
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
        assert!(names.iter().any(|n| n == "plasm"));
        assert!(names.iter().any(|n| n == "plasm_run"));
    }

    #[test]
    fn plasm_context_tool_description_snapshot() {
        with_insta_snapshots(|| {
            let tools = super::PlasmMcpHandler::plasm_tools();
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
        let tools = super::PlasmMcpHandler::plasm_tools();
        let desc = tools
            .iter()
            .find(|t| t.name == "plasm_context")
            .and_then(|t| t.description.as_deref())
            .expect("plasm_context description");
        let workflow = super::mcp_server_initialize_workflow();
        assert!(
            desc.contains("**Extend picks:**"),
            "expected append guidance in plasm_context description"
        );
        assert!(
            workflow.contains(plasm_core::prompt_render::SESSION_DISCIPLINE_MCP),
            "expected session discipline in initialize workflow"
        );
        assert!(
            workflow.contains("Reuse ref"),
            "expected steady-state guidance in initialize workflow"
        );
    }

    #[test]
    fn plasm_context_input_schema_requires_intent_and_seeds() {
        let tools = super::PlasmMcpHandler::plasm_tools();
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
        let tools = super::PlasmMcpHandler::plasm_tools();
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
        let tools = super::PlasmMcpHandler::plasm_tools();
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
    fn plasm_run_input_schema_matches_plasm_program_shape() {
        let tools = super::PlasmMcpHandler::plasm_tools();
        let plasm_run = tools
            .iter()
            .find(|t| t.name == "plasm_run")
            .expect("plasm_run tool");
        let v = serde_json::to_value(&plasm_run.input_schema).expect("input_schema json");
        let required = v
            .get("required")
            .and_then(|x| x.as_array())
            .expect("required array");
        assert!(required.iter().any(|x| x.as_str() == Some("program")));
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
        assert!(!props.contains_key("execute"));
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
        use plasm_core::discovery::{
            CapabilityQuery, DiscoveryResult, EntitySummary, RankedCandidate,
        };
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
        assert_snapshot!(
            crate::discovery_human_format::format_discovery_markdown(&r),
            @"
```tsv
api\tentity\tdescription
demo\tWidget\tA contrived widget line
```

"
        );
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
