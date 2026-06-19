use super::*;

#[tokio::test]
async fn open_wire_is_table_only() {
    use plasm_core::{TeachingFenceSlice, TSV_TEACHING_TABLE_HEADER};

    let st = test_state_with_registry();
    let out = apply_capability_seeds(
        &st,
        None,
        None,
        vec![CapabilitySeed {
            entry_id: "overshow".into(),
            entity: "Profile".into(),
        }],
        None,
        None,
        None,
        "list profiles for triage",
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .expect("apply seeds");
    assert!(out.new_symbol_space, "expected fresh open");
    let open = out
        .waves
        .iter()
        .find(|w| w.mode == "open")
        .expect("open wave");
    assert!(
        !open.markdown_delta.lines().any(|l| {
            let t = l.trim_start();
            t.starts_with('#') && t.contains("Valid Plasm")
        }),
        "open wire must not include global grammar contract: {}",
        open.markdown_delta.chars().take(400).collect::<String>()
    );
    assert!(
        open.markdown_delta
            .contains(TSV_TEACHING_TABLE_HEADER.trim_end()),
        "open wire must include teaching table header"
    );
    let created = st
        .get_execute_session(&out.prompt_hash, &out.session_id)
        .await
        .expect("session row");
    let mode = st.engine.prompt_pipeline().render_mode;
    let body = plasm_core::teaching_tsv_from_wrapped_prompt(
        &created.prompt_text,
        mode.markdown_fence_info_string(),
        TeachingFenceSlice::TableOnly,
    )
    .expect("agent body slice");
    assert!(
        !body.lines().any(|l| l.starts_with('#')),
        "stored execute prompt is table-only"
    );
}

#[tokio::test]
async fn open_wire_includes_seeded_abstract_entity_row() {
    let Some(st) = test_state_with_linear_registry() else {
        return;
    };
    let out = apply_capability_seeds(
        &st,
        None,
        None,
        vec![CapabilitySeed {
            entry_id: "linear".into(),
            entity: "IssueContext".into(),
        }],
        None,
        None,
        None,
        "triage one issue with comments",
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .expect("apply seeds");
    let open = out
        .waves
        .iter()
        .find(|w| w.mode == "open")
        .expect("open wave");
    assert!(
        open.markdown_delta.contains("e1"),
        "abstract IssueContext seed must assign e1: {}",
        open.markdown_delta.chars().take(500).collect::<String>()
    );
    let data_rows: Vec<_> = open
        .markdown_delta
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("```") && l.contains('\t'))
        .collect();
    assert!(
        !data_rows.is_empty(),
        "expected executable teaching rows, not header-only TSV"
    );
}

#[tokio::test]
async fn same_intent_federated_expand_assigns_distinct_e_symbols() {
    const INTENT: &str = "matrix federated lang items same intent";
    let Some(st) = test_state_with_matrix_federated_registry() else {
        return;
    };

    let first = apply_capability_seeds(
        &st,
        None,
        None,
        vec![CapabilitySeed {
            entry_id: "github".into(),
            entity: "LangItem".into(),
        }],
        None,
        None,
        None,
        INTENT,
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .expect("first open");
    assert!(first.new_symbol_space);
    let open = first
        .waves
        .iter()
        .find(|w| w.mode == "open")
        .expect("open wave");
    assert!(
        open.markdown_delta.contains("e1"),
        "github LangItem should be e1: {}",
        open.markdown_delta.chars().take(400).collect::<String>()
    );

    let binding = (first.prompt_hash.as_str(), first.session_id.as_str());
    let expand = apply_capability_seeds(
        &st,
        None,
        Some(binding),
        vec![
            CapabilitySeed {
                entry_id: "github".into(),
                entity: "LangItem".into(),
            },
            CapabilitySeed {
                entry_id: "linear".into(),
                entity: "LangItem".into(),
            },
        ],
        None,
        None,
        None,
        INTENT,
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .expect("federated expand");
    assert!(
        !expand.new_symbol_space,
        "same intent expand must reuse symbol space"
    );
    let federated = expand
        .waves
        .iter()
        .find(|w| w.mode == "federate")
        .expect("federate wave");
    assert!(
        federated.markdown_delta.contains("e2"),
        "linear LangItem delta must assign e2: {}",
        federated
            .markdown_delta
            .chars()
            .take(400)
            .collect::<String>()
    );

    let reuse = apply_capability_seeds(
        &st,
        None,
        Some(binding),
        vec![
            CapabilitySeed {
                entry_id: "github".into(),
                entity: "LangItem".into(),
            },
            CapabilitySeed {
                entry_id: "linear".into(),
                entity: "LangItem".into(),
            },
        ],
        None,
        None,
        None,
        "different intent string same seeds",
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .expect("reuse with new intent string");
    let unchanged = reuse
        .waves
        .iter()
        .find(|w| w.mode == "expand" && w.reused_session)
        .expect("unchanged expand wave");
    assert!(
        unchanged.markdown_delta.contains("Unchanged"),
        "fully exposed seeds should return compact reuse status: {}",
        unchanged.markdown_delta
    );
    assert!(
        unchanged.markdown_delta.contains("e1=") && unchanged.markdown_delta.contains("e2="),
        "reuse must include compact entity symbol map: {}",
        unchanged.markdown_delta
    );
    assert!(
        unchanged.markdown_delta.contains("Next: `plasm`"),
        "reuse must name next step: {}",
        unchanged.markdown_delta
    );
    assert!(
        !unchanged.markdown_delta.contains("rows:` fields only"),
        "reuse must not replay grammar cheat sheet: {}",
        unchanged.markdown_delta
    );
}

#[test]
fn agent_markdown_minimal_open_shape() {
    let waves = vec![CapabilityWaveOutcome {
        mode: "open".into(),
        entry_id: "fibery".into(),
        entities: vec!["Record".into()],
        markdown_delta: "```tsv\nplasm_expr\tMeaning\ne1\trow\n```\n".into(),
        reused_session: false,
        teaching_prompt_chars_added: 10,
        relations_delta: Vec::new(),
    }];
    let md = build_plasm_context_agent_markdown("l_AAAAAAAAQACAAAAAAAAAAQ", &waves);
    assert!(md.starts_with("`l_AAAAAAAAQACAAAAAAAAAAQ`\n\n"));
    assert!(md.contains("```tsv"));
    assert!(md.contains("plasm_expr\tMeaning"));
    assert!(!md.contains("Exposed"));
    assert!(!md.contains("Added capabilities"));
}

#[test]
fn tool_meta_keeps_slim_agent_keys() {
    let out = ApplyCapabilitySeedsOutcome {
        prompt_hash: "ph".into(),
        session_id: "sid".into(),
        primary_entry_id: "fibery".into(),
        principal: None,
        waves: vec![],
        binding_updated: true,
        new_symbol_space: true,
        stale_execute_binding_recovered: false,
        stale_binding_previous: None,
    };
    let meta = build_plasm_context_tool_meta("l_AAAAAAAAQACAAAAAAAAAAQ", &out, Some(2), None, None);
    assert!(meta.contains_key("logical_session_ref"));
    assert!(meta.contains_key("continuity"));
    assert!(meta.contains_key("domain_revision"));
    assert!(!meta.contains_key("execute_binding"));
    assert!(!meta.contains_key("catalog_entry_ids"));
    assert!(!meta.contains_key("intent"));
}
