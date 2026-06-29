//! View DAG conformance — preflight + catalog validation against `plasm_language_matrix_views`.
//!
//! Hermit is reserved for transport/decode regressions; view wiring lives here (fast, deterministic).

#[path = "common/hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

#[path = "common/language_matrix_views.rs"]
mod language_matrix_views;

use std::sync::Arc;

use plasm_agent::plasm_compile::compile_plasm_program;
use plasm_agent::plasm_plan_run::{evaluate_plasm_comp_dry, run_plasm_comp};
use plasm_compile::{validate_cgs_capability_templates, validate_cgs_views};
use plasm_core::PromptPipelineConfig;
use plasm_core::QueryExpr;
use plasm_runtime::{
    preflight_view_query,
    view_test_support::{matrix_view_query, matrix_views_cgs, MATRIX_VIEW_PREFLIGHT_CASES},
    ViewAmbientContext,
};
use plasm_runtime::{ExecutionConfig, ExecutionEngine};

use language_matrix_views::{
    load_language_matrix_views_cgs, views_execute_session, views_matrix_host_state,
    VIEWS_MATRIX_ENTRY_ID,
};

#[test]
fn matrix_views_catalog_passes_static_validation() {
    let cgs = matrix_views_cgs();
    validate_cgs_capability_templates(&cgs).expect("CML templates");
    validate_cgs_views(&cgs).expect("views DAG");
}

#[test]
fn matrix_views_all_preflight() {
    let cgs = matrix_views_cgs();
    let ambient = ViewAmbientContext::default();
    for &(view_name, entity) in MATRIX_VIEW_PREFLIGHT_CASES {
        let query = matrix_view_query(entity);
        preflight_view_query(view_name, &query, &cgs, &ambient)
            .unwrap_or_else(|err| panic!("{view_name} preflight: {err}"));
    }
}

#[test]
fn matrix_views_missing_scope_preflight_errors() {
    let cgs = matrix_views_cgs();
    let query = QueryExpr::all("LangDigest");
    let err = preflight_view_query("lang_digest", &query, &cgs, &ViewAmbientContext::default())
        .expect_err("missing scope");
    assert!(err.to_string().contains("item_id"), "{err}");
}

#[tokio::test]
async fn matrix_views_row_to_text_p_symbol_template_body() {
    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let cgs = load_language_matrix_views_cgs();
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("templates");
    let es = Arc::new(views_execute_session(cgs.clone()));
    let map = es
        .teaching_exposure
        .as_ref()
        .expect("views session exposure")
        .symbol_map_arc();
    let p_id = map.ident_sym_entity_field_for(VIEWS_MATRIX_ENTRY_ID, "LangItem", "id");
    let p_title = map.ident_sym_entity_field_for(VIEWS_MATRIX_ENTRY_ID, "LangItem", "title");
    assert!(
        p_id.starts_with('p'),
        "expected p# for LangItem.id, got {p_id}"
    );
    assert!(
        p_title.starts_with('p'),
        "expected p# for LangItem.title, got {p_title}"
    );
    let program = format!(
        "items = LangItem(\"i1\")\nreport = items[{p_id},{p_title}] <<PLASM_VIEWS_P_BODY\n{{% for r in rows %}}- {{{{ r.{p_id} }}}}: {{{{ r.{p_title} }}}}\n{{% endfor %}}\nPLASM_VIEWS_P_BODY\nreport"
    );
    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        es.as_ref(),
        "matrix_views_p_body_render",
        &program,
    )
    .expect("compile row-to-text p# body");
    evaluate_plasm_comp_dry(es.as_ref(), &bundle).expect("dry row-to-text p# body");
    let comp_wire = serde_json::to_string(&bundle.artifact().comp).expect("comp json");
    assert!(
        comp_wire.contains("column_aliases"),
        "comp wire must persist render column_aliases"
    );
    assert!(
        comp_wire.contains(&p_id),
        "comp wire must retain teaching token {p_id} as alias key"
    );
    let st = Arc::new(views_matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs,
    ));
    let live = run_plasm_comp(
        es.as_ref(),
        st.as_ref(),
        es.prompt_hash.as_str(),
        "matrix_views_sess",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("live row-to-text p# body");
    let md = live
        .run_markdown
        .as_deref()
        .expect("run markdown for render row");
    assert!(md.contains("i1"), "expected rendered id in markdown: {md}");
    let content = live
        .node_results
        .iter()
        .find_map(|nr| nr.get("rows").and_then(|r| r.as_array()))
        .and_then(|rows| rows.iter().find(|r| r.get("content").is_some()))
        .and_then(|r| r.get("content"))
        .and_then(|v| v.as_str());
    if let Some(text) = content {
        assert!(
            text.contains("i1"),
            "rendered content should include id via p# alias: {text}"
        );
    }
    assert!(
        live.node_results.len() >= 2,
        "expected render + return nodes, got {}",
        live.node_results.len()
    );
}

#[tokio::test]
async fn matrix_views_row_to_text_source_alias_iteration() {
    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let cgs = load_language_matrix_views_cgs();
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("templates");
    let es = Arc::new(views_execute_session(cgs.clone()));
    let map = es
        .teaching_exposure
        .as_ref()
        .expect("views session exposure")
        .symbol_map_arc();
    let p_id = map.ident_sym_entity_field_for(VIEWS_MATRIX_ENTRY_ID, "LangItem", "id");
    let p_title = map.ident_sym_entity_field_for(VIEWS_MATRIX_ENTRY_ID, "LangItem", "title");
    let p_score = map.ident_sym_entity_field_for(VIEWS_MATRIX_ENTRY_ID, "LangItem", "score");
    let program = format!(
        "items = LangItem(\"i1\")\nreport = items[{p_id},{p_title},{p_score}] <<PLASM_VIEWS_ALIAS_BODY\n{{% for r in items %}}- {{{{ r.{p_id} }}}}: {{{{ r.{p_title} }}}} (score: {{{{ r.{p_score} or \"—\" }}}})\n{{% endfor %}}\nPLASM_VIEWS_ALIAS_BODY\nreport"
    );
    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        es.as_ref(),
        "matrix_views_alias_render",
        &program,
    )
    .expect("compile row-to-text source alias");
    evaluate_plasm_comp_dry(es.as_ref(), &bundle).expect("dry row-to-text source alias");
    let comp_wire = serde_json::to_string(&bundle.artifact().comp).expect("comp json");
    assert!(
        comp_wire.contains("collection_alias"),
        "comp wire must persist render collection_alias"
    );
    assert!(
        comp_wire.contains("\"items\""),
        "comp wire must retain items source alias"
    );
    let st = Arc::new(views_matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs,
    ));
    let live = run_plasm_comp(
        es.as_ref(),
        st.as_ref(),
        es.prompt_hash.as_str(),
        "matrix_views_alias_sess",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("live row-to-text source alias");
    let md = live
        .run_markdown
        .as_deref()
        .expect("run markdown for render row");
    assert!(md.contains("i1"), "expected rendered id in markdown: {md}");
    assert!(
        md.contains("avalanche fracture") || md.contains("score:"),
        "source-alias iteration should render item row: {md}"
    );
}
