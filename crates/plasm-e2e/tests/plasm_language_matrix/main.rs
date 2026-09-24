//! Hermit-backed matrix: parse → DAG compile → dry validate → live plan run for Plasm programs.
//!
//! Conformance surface for user-visible Plasm syntax against `plasm_language_matrix` fixtures.
//! Coverage contract: see `features::REQUIRED_FEATURE_TAGS` and row `features` columns.

#![allow(dead_code)]

#[path = "../common/hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

#[path = "../common/language_matrix.rs"]
mod language_matrix;

mod assert_live;
mod assert_planning;
mod features;
mod harness;
mod ir_helpers;
mod program;
mod row;
mod rows;

use plasm_agent::plasm_compile::{compile_plasm_expression, compile_plasm_program};
use plasm_agent::plasm_plan_run::evaluate_plasm_comp_dry;
use plasm_core::{Expr, PromptPipelineConfig};
use plasm_runtime::{ExecutionConfig, ExecutionEngine};

use assert_live::assert_comp_witness;
use assert_planning::assert_planning_ir;
use harness::{matrix_live_run_row, plasm_language_matrix_live_runs_async};
use program::matrix_program_for_row;
use rows::{all_rows, find_row, row_count};

mod relation_fanout;

#[tokio::test]
async fn isolation_sidecar_serves_vault_and_lanes() {
    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let client = reqwest::Client::new();
    let vault = client
        .get(format!("{base}/language/v1/vaults/venmo"))
        .send()
        .await
        .expect("vault get")
        .json::<serde_json::Value>()
        .await
        .expect("vault json");
    assert_eq!(
        vault.get("id").and_then(|v| v.as_str()),
        Some("venmo"),
        "sidecar must serve LangVault Get, got {vault}"
    );
    let lanes = client
        .get(format!("{base}/language/v1/lanes?shelf=alpha"))
        .send()
        .await
        .expect("lanes get")
        .json::<serde_json::Value>()
        .await
        .expect("lanes json");
    assert!(
        lanes.as_array().is_some_and(|a| a.len() == 2),
        "sidecar must serve LangLane multi-row, got {lanes}"
    );
}

#[tokio::test]
async fn plasm_language_matrix_cgs_templates_validate() {
    let cgs = language_matrix::load_language_matrix_cgs();
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("capability CML templates");
}

#[test]
fn http2_operation_feedback_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("http2 operation feedback runtime");
            rt.block_on(http2_operation_feedback_live_async());
        })
        .expect("spawn http2 operation feedback harness")
        .join()
        .expect("join http2 operation feedback harness");
}

async fn http2_operation_feedback_live_async() {
    use std::sync::Arc;

    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let cgs = language_matrix::load_language_matrix_cgs();
    let mut cgs_live = (*cgs).clone();
    cgs_live.http_backend = base.clone();
    let cgs_live = Arc::new(cgs_live);
    let es = Arc::new(language_matrix::matrix_execute_session(cgs_live.clone()));
    let st = Arc::new(language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs_live,
    ));
    for id in [
        "lang_effect_delete",
        "lang_effect_action_ping",
        "lang_for_each_empty_ping",
    ] {
        let row = find_row(id).unwrap_or_else(|| panic!("missing matrix row {id}"));
        matrix_live_run_row(row, es.as_ref(), st.as_ref()).await;
    }
}

#[test]
fn plasm_language_matrix_live_runs() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .expect("matrix live runtime");
            rt.block_on(plasm_language_matrix_live_runs_async());
        })
        .expect("spawn matrix live harness")
        .join()
        .expect("join matrix live harness");
}

/// Focused CUGA-shaped dual-auth witness: two logins → two Bearer surfaces in one program.
#[test]
fn lang_federated_auth_session_bearer_hole_fill_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("auth hole-fill runtime");
            rt.block_on(lang_federated_auth_session_bearer_hole_fill_live_async());
        })
        .expect("spawn auth hole-fill harness")
        .join()
        .expect("join auth hole-fill harness");
}

async fn lang_federated_auth_session_bearer_hole_fill_live_async() {
    use std::sync::Arc;

    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let cgs = language_matrix::load_language_matrix_cgs();
    let mut cgs_live = (*cgs).clone();
    cgs_live.http_backend = base;
    let cgs_live = Arc::new(cgs_live);

    let es = Arc::new(language_matrix::matrix_federated_auth_session_session(
        cgs_live.clone(),
    ));
    let st = Arc::new(
        language_matrix::matrix_federated_duplicate_entity_host_state(
            ExecutionEngine::new(ExecutionConfig {
                base_url: Some(cgs_live.http_backend.clone()),
                ..Default::default()
            })
            .expect("ExecutionEngine"),
            cgs_live,
        ),
    );
    let row = find_row("lang_federated_auth_session_provides_mutation")
        .expect("auth hole-fill matrix row");
    matrix_live_run_row(row, es.as_ref(), st.as_ref()).await;
}

/// RA-2: live `| select handle = owner | where handle = "alice"` on projection grain.
#[test]
fn lang_select_alias_where_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("select alias where runtime");
            rt.block_on(lang_select_alias_where_live_async());
        })
        .expect("spawn select alias where harness")
        .join()
        .expect("join select alias where harness");
}

async fn lang_select_alias_where_live_async() {
    use std::sync::Arc;

    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let cgs = language_matrix::load_language_matrix_cgs();
    let mut cgs_live = (*cgs).clone();
    cgs_live.http_backend = base.clone();
    let cgs_live = Arc::new(cgs_live);
    let es = Arc::new(language_matrix::matrix_execute_session(cgs_live.clone()));
    let st = Arc::new(language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs_live,
    ));
    let row = find_row("lang_select_alias_where").expect("select alias where matrix row");
    matrix_live_run_row(row, es.as_ref(), st.as_ref()).await;
}

/// CEP-5/6: `LangItem("i1").lines` must yield embedded LangLine rows from parent GET.
#[test]
fn lang_relation_lines_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("relation lines runtime");
            rt.block_on(lang_relation_lines_live_async());
        })
        .expect("spawn relation lines harness")
        .join()
        .expect("join relation lines harness");
}

async fn lang_relation_lines_live_async() {
    use std::sync::Arc;

    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let cgs = language_matrix::load_language_matrix_cgs();
    let mut cgs_live = (*cgs).clone();
    cgs_live.http_backend = base.clone();
    let cgs_live = Arc::new(cgs_live);
    let es = Arc::new(language_matrix::matrix_execute_session(cgs_live.clone()));
    let st = Arc::new(language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs_live,
    ));
    let row = find_row("lang_relation_lines").expect("relation lines matrix row");
    matrix_live_run_row(row, es.as_ref(), st.as_ref()).await;
}

/// PLP-8: stuck cursor must fail closed on bound exhaustion (not succeed as Ready).
#[test]
fn lang_iterate_bound_exhausted_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("iterate bound exhausted runtime");
            rt.block_on(lang_iterate_bound_exhausted_live_async());
        })
        .expect("spawn iterate bound exhausted harness")
        .join()
        .expect("join iterate bound exhausted harness");
}

async fn lang_iterate_bound_exhausted_live_async() {
    use std::sync::Arc;

    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let cgs = language_matrix::load_language_matrix_cgs();
    let mut cgs_live = (*cgs).clone();
    cgs_live.http_backend = base.clone();
    let cgs_live = Arc::new(cgs_live);
    let es = Arc::new(language_matrix::matrix_execute_session(cgs_live.clone()));
    let st = Arc::new(language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs_live,
    ));
    let row = find_row("lang_iterate_bound_exhausted").expect("iterate bound exhausted matrix row");
    matrix_live_run_row(row, es.as_ref(), st.as_ref()).await;
}

/// RA-8: dry + live `| where score > 0` on an integer catalog field (typed dry stubs).
#[test]
fn lang_integer_where_gt_dry_coerce_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("integer where coerce runtime");
            rt.block_on(lang_integer_where_gt_dry_coerce_live_async());
        })
        .expect("spawn integer where coerce harness")
        .join()
        .expect("join integer where coerce harness");
}

async fn lang_integer_where_gt_dry_coerce_live_async() {
    use std::sync::Arc;

    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let cgs = language_matrix::load_language_matrix_cgs();
    let mut cgs_live = (*cgs).clone();
    cgs_live.http_backend = base.clone();
    let cgs_live = Arc::new(cgs_live);
    let es = Arc::new(language_matrix::matrix_execute_session(cgs_live.clone()));
    let st = Arc::new(language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs_live,
    ));
    let row =
        find_row("lang_integer_where_gt_dry_coerce").expect("integer where coerce matrix row");
    matrix_live_run_row(row, es.as_ref(), st.as_ref()).await;
}

/// PLP-7: Minijinja `.content` stitch into a later string param (abolished `${…}`).
#[test]
fn lang_utf8_minijinja_content_stitch_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("minijinja content stitch runtime");
            rt.block_on(lang_utf8_minijinja_content_stitch_live_async());
        })
        .expect("spawn minijinja content stitch harness")
        .join()
        .expect("join minijinja content stitch harness");
}

async fn lang_utf8_minijinja_content_stitch_live_async() {
    use std::sync::Arc;

    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let cgs = language_matrix::load_language_matrix_cgs();
    let mut cgs_live = (*cgs).clone();
    cgs_live.http_backend = base.clone();
    let cgs_live = Arc::new(cgs_live);
    let es = Arc::new(language_matrix::matrix_execute_session(cgs_live.clone()));
    let st = Arc::new(language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs_live,
    ));
    let row = find_row("lang_utf8_minijinja_content_stitch")
        .expect("minijinja content stitch matrix row");
    matrix_live_run_row(row, es.as_ref(), st.as_ref()).await;
}

#[test]
fn matrix_coverage_contract_all_rows_require_live_execution() {
    const RUNTIME_PROGRAM_ROW_IDS: &[&str] = &[
        "lang_relation_opaque_r_symbol",
        "lang_homograph_lhs_coercion",
        "lang_federated_duplicate_entity_relation_r",
        "lang_federated_duplicate_entity_mutator_m",
        "lang_federated_duplicate_entity_pathless_action",
        "lang_federated_auth_session_provides_mutation",
        "lang_federated_relation_target_entry",
        "lang_bind_template_inline_on_e1",
    ];
    for row in all_rows() {
        assert!(
            row.min_node_results >= 1,
            "row {} must declare min_node_results for live execute",
            row.id
        );
        assert!(
            !row.program.trim().is_empty() || RUNTIME_PROGRAM_ROW_IDS.contains(&row.id),
            "row {} must include a program (or runtime synthesis in matrix_program_for_row)",
            row.id
        );
        assert!(
            !row.expect_markdown_substrings.is_empty()
                || row.features.contains(&"host_wait_cancel")
                || row.expect_live_error.is_some()
                || row.features.contains(&"iterate_bound_exhausted"),
            "row {} must declare live markdown/IO expectations",
            row.id
        );
    }
    assert!(
        row_count() >= 40,
        "matrix fixture unexpectedly shrunk — audit coverage manifest"
    );
}

/// RA-2 Search polarity: `~` and `{q=}` stamp Search; miss literal is not Query::all.
#[test]
fn language_matrix_search_tilde_and_brace_q_dry_comp_witness() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    for row_id in ["lang_search", "lang_search_miss", "lang_search_brace_q"] {
        let row = find_row(row_id).unwrap_or_else(|| panic!("missing matrix row {row_id}"));
        let program = matrix_program_for_row(row, &es);
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            &es,
            row.id,
            &program,
        )
        .unwrap_or_else(|e| panic!("row {} compile: {e}", row.id));
        let comp_json = serde_json::to_value(&bundle.artifact().comp)
            .unwrap_or_else(|e| panic!("row {} comp json: {e}", row.id));
        let dry = evaluate_plasm_comp_dry(&es, &bundle)
            .unwrap_or_else(|e| panic!("row {} evaluate_plasm_comp_dry: {e}", row.id));
        assert_planning_ir(row, &dry, &comp_json)
            .unwrap_or_else(|e| panic!("row {} planning IR: {e}", row.id));
        assert_comp_witness(&dry)
            .unwrap_or_else(|e| panic!("row {} monadic comp witness: {e}", row.id));
    }
}

/// RA-13: `| where field in rhs` / `not in` against a one-column rowset.
#[test]
fn where_in_rowset_ra13_dry_comp_witness() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    for row_id in [
        "lang_where_literal_boolean_sugar",
        "lang_where_boolean_rowset_sugar",
        "lang_where_in_rowset",
        "lang_where_not_in_rowset",
        "lang_where_in_rowset_paren",
        "lang_where_not_in_universe_left",
        "lang_where_not_in_universe_right",
    ] {
        let row = find_row(row_id).unwrap_or_else(|| panic!("missing matrix row {row_id}"));
        let program = matrix_program_for_row(row, &es);
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            &es,
            row.id,
            &program,
        )
        .unwrap_or_else(|e| panic!("row {} compile: {e}", row.id));
        let comp_json = serde_json::to_value(&bundle.artifact().comp)
            .unwrap_or_else(|e| panic!("row {} comp json: {e}", row.id));
        let dry = evaluate_plasm_comp_dry(&es, &bundle)
            .unwrap_or_else(|e| panic!("row {} evaluate_plasm_comp_dry: {e}", row.id));
        assert_planning_ir(row, &dry, &comp_json)
            .unwrap_or_else(|e| panic!("row {} planning IR: {e}", row.id));
        assert_comp_witness(&dry)
            .unwrap_or_else(|e| panic!("row {} monadic comp witness: {e}", row.id));
    }
}

/// RA-13 universe: both `not in` polarities compile; results ⊆ pipe-left.
#[test]
fn where_not_in_universe_ra13_dry_comp_witness() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    for row_id in [
        "lang_where_not_in_universe_left",
        "lang_where_not_in_universe_right",
    ] {
        let row = find_row(row_id).unwrap_or_else(|| panic!("missing matrix row {row_id}"));
        let program = matrix_program_for_row(row, &es);
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            &es,
            row.id,
            &program,
        )
        .unwrap_or_else(|e| panic!("row {} compile: {e}", row.id));
        let comp_json = serde_json::to_value(&bundle.artifact().comp)
            .unwrap_or_else(|e| panic!("row {} comp json: {e}", row.id));
        let dry = evaluate_plasm_comp_dry(&es, &bundle)
            .unwrap_or_else(|e| panic!("row {} evaluate_plasm_comp_dry: {e}", row.id));
        assert_planning_ir(row, &dry, &comp_json)
            .unwrap_or_else(|e| panic!("row {} planning IR: {e}", row.id));
        assert_comp_witness(&dry)
            .unwrap_or_else(|e| panic!("row {} monadic comp witness: {e}", row.id));
    }
}

#[test]
fn isolation_patterns_dry_comp_witness() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    for row_id in [
        "lang_get_singleton_field_scalar",
        "lang_get_singleton_field_argument",
        "lang_get_singleton_field_password",
        "lang_get_singleton_field_empty",
        "lang_required_selection_default",
        "lang_required_selection_multi",
        "lang_required_selection_empty",
        "lang_union_empty_right",
        "lang_quoted_binding_literal",
        "lang_quoted_binding_field_literal",
    ] {
        let row = find_row(row_id).unwrap_or_else(|| panic!("missing matrix row {row_id}"));
        let program = matrix_program_for_row(row, &es);
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            &es,
            row.id,
            &program,
        )
        .unwrap_or_else(|e| panic!("row {} compile: {e}", row.id));
        let comp_json = serde_json::to_value(&bundle.artifact().comp)
            .unwrap_or_else(|e| panic!("row {} comp json: {e}", row.id));
        let dry = evaluate_plasm_comp_dry(&es, &bundle)
            .unwrap_or_else(|e| panic!("row {} evaluate_plasm_comp_dry: {e}", row.id));
        assert_planning_ir(row, &dry, &comp_json)
            .unwrap_or_else(|e| panic!("row {} planning IR: {e}", row.id));
        assert_comp_witness(&dry)
            .unwrap_or_else(|e| panic!("row {} monadic comp witness: {e}", row.id));
    }
}

#[test]
fn union_rowset_ra14_dry_comp_witness() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    for row_id in [
        "lang_union_rowset",
        "lang_union_rowset_alias",
        "lang_union_rowset_alias_distinct",
        "lang_union_rowset_alias_existing",
        "lang_union_rowset_paren",
        "lang_union_empty_right",
    ] {
        let row = find_row(row_id).unwrap_or_else(|| panic!("missing matrix row {row_id}"));
        let program = matrix_program_for_row(row, &es);
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            &es,
            row.id,
            &program,
        )
        .unwrap_or_else(|e| panic!("row {} compile: {e}", row.id));
        let comp_json = serde_json::to_value(&bundle.artifact().comp)
            .unwrap_or_else(|e| panic!("row {} comp json: {e}", row.id));
        let dry = evaluate_plasm_comp_dry(&es, &bundle)
            .unwrap_or_else(|e| panic!("row {} evaluate_plasm_comp_dry: {e}", row.id));
        assert_planning_ir(row, &dry, &comp_json)
            .unwrap_or_else(|e| panic!("row {} planning IR: {e}", row.id));
        assert_comp_witness(&dry)
            .unwrap_or_else(|e| panic!("row {} monadic comp witness: {e}", row.id));
    }
}

/// RA-14 dest reject: literal `| union ("a", "b")` is not a rowset.
#[test]
fn union_literal_list_is_compile_reject() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    let err = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "union_literal_list_is_compile_reject",
        r#"LangItem | select owner | union ("alice", "bob")"#,
    )
    .expect_err("literal union list must fail compile");
    let err = err.to_string();
    assert!(
        err.contains("not a literal list"),
        "RA-14 dest reject must name the list ban, got: {err}"
    );
}

/// RA-14: mismatched column names are a compile reject, not a silent drop.
#[test]
fn union_column_mismatch_is_compile_reject() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    let err = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_union_column_mismatch",
        r#"LangLane {shelf="alpha"} | select title | union (LangLane {shelf="alpha"} | select shelf)"#,
    )
    .expect_err("mismatched union columns must fail compile");
    let err = err.to_string();
    assert!(
        err.contains("union requires the same columns") || err.contains("RA-14"),
        "RA-14 mismatch must name the column law, got: {err}"
    );
}

/// RA-15: omitting a required selection with no authored default is diagnosed.
#[test]
fn required_selection_omitted_is_compile_reject() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    let err = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_required_selection_omitted",
        "LangLane",
    )
    .expect_err("omitted shelf must fail compile");
    let err = err.to_string();
    assert!(
        err.contains("shelf"),
        "RA-15 diagnostic must name the omitted parameter, got: {err}"
    );
    assert!(
        err.contains("LangLane"),
        "RA-15 diagnostic must point at the query expression, got: {err}"
    );
}

/// PLP-11: quoted in-scope binding spelling stays a string literal.
#[test]
fn quoted_binding_literal_is_preserved() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    let row = find_row("lang_quoted_binding_literal")
        .unwrap_or_else(|| panic!("missing matrix row lang_quoted_binding_literal"));
    let program = matrix_program_for_row(row, &es);
    assert_eq!(
        program,
        "item = LangItem(\"i1\")\nLangItem | where title = \"item\""
    );
    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        row.id,
        &program,
    )
    .unwrap_or_else(|e| panic!("quoted title literal must compile: {e}"));
    let comp_json =
        serde_json::to_value(&bundle.artifact().comp).unwrap_or_else(|e| panic!("comp json: {e}"));
    let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap_or_else(|e| panic!("dry: {e}"));
    assert_planning_ir(row, &dry, &comp_json)
        .unwrap_or_else(|e| panic!("PLP-11 filter literal: {e}"));
    assert_comp_witness(&dry).unwrap_or_else(|e| panic!("comp witness: {e}"));
}

/// PLP-11: quoted `binding.field` spelling stays a string literal.
#[test]
fn quoted_binding_field_literal_is_preserved() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    let row = find_row("lang_quoted_binding_field_literal")
        .unwrap_or_else(|| panic!("missing matrix row lang_quoted_binding_field_literal"));
    let program = matrix_program_for_row(row, &es);
    assert_eq!(
        program,
        "item = LangItem(\"i1\")\nLangItem(\"i1\").update(title=\"item.title\", score=1, owner=\"alice\")"
    );
    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        row.id,
        &program,
    )
    .unwrap_or_else(|e| panic!("quoted field literal must compile: {e}"));
    let comp_json =
        serde_json::to_value(&bundle.artifact().comp).unwrap_or_else(|e| panic!("comp json: {e}"));
    let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap_or_else(|e| panic!("dry: {e}"));
    assert_planning_ir(row, &dry, &comp_json)
        .unwrap_or_else(|e| panic!("PLP-11 update literal: {e}"));
    assert_comp_witness(&dry).unwrap_or_else(|e| panic!("comp witness: {e}"));
}

/// Literal membership repair must reject unsupported values without dropping them.
#[test]
fn where_in_invalid_literal_list_is_compile_reject() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    for source in [
        r#"LangItem | where owner in ("alice", null)"#,
        r#"LangItem | where owner in ("alice", {})"#,
        r#"LangItem | where score in (1, true)"#,
    ] {
        compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            &es,
            "invalid_membership",
            source,
        )
        .expect_err("invalid list entry must reject the entire selection");
    }
}

/// RA-2: `| select handle = owner | where handle = …` binds the projection grain.
#[test]
fn select_alias_where_ra2_dry_comp_witness() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    let row = find_row("lang_select_alias_where")
        .unwrap_or_else(|| panic!("missing matrix row lang_select_alias_where"));
    let program = matrix_program_for_row(row, &es);
    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        row.id,
        &program,
    )
    .unwrap_or_else(|e| panic!("row {} compile: {e}", row.id));
    let comp_json = serde_json::to_value(&bundle.artifact().comp)
        .unwrap_or_else(|e| panic!("row {} comp json: {e}", row.id));
    let dry = evaluate_plasm_comp_dry(&es, &bundle)
        .unwrap_or_else(|e| panic!("row {} evaluate_plasm_comp_dry: {e}", row.id));
    assert_planning_ir(row, &dry, &comp_json)
        .unwrap_or_else(|e| panic!("row {} planning IR: {e}", row.id));
    assert_comp_witness(&dry)
        .unwrap_or_else(|e| panic!("row {} monadic comp witness: {e}", row.id));
}

/// Host-only `wait` / `cancel` continuations (parse smoke; no Hermit live execute).
#[test]
fn program_return_semantics_dry_comp_witness() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    for row_id in [
        "lang_program_return_binding_only_last",
        "lang_program_return_pipeline_filter_sort",
        "lang_program_return_consecutive_writes",
    ] {
        let row = find_row(row_id).unwrap_or_else(|| panic!("missing matrix row {row_id}"));
        let program = matrix_program_for_row(row, &es);
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            &es,
            row.id,
            &program,
        )
        .unwrap_or_else(|e| panic!("row {} compile: {e}", row.id));
        let comp_json = serde_json::to_value(&bundle.artifact().comp)
            .unwrap_or_else(|e| panic!("row {} comp json: {e}", row.id));
        let dry = evaluate_plasm_comp_dry(&es, &bundle)
            .unwrap_or_else(|e| panic!("row {} evaluate_plasm_comp_dry: {e}", row.id));
        assert_planning_ir(row, &dry, &comp_json)
            .unwrap_or_else(|e| panic!("row {} planning IR: {e}", row.id));
        assert_comp_witness(&dry)
            .unwrap_or_else(|e| panic!("row {} monadic comp witness: {e}", row.id));
    }
}

/// RA-4: monolith vs bind-cut share a valid monadic Comp spine (witness, not label-identical).
#[test]
fn ra4_pipe_apply_factor_dry_comp_witness() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    for row_id in [
        "lang_ra4_pipe_monolith",
        "lang_ra4_pipe_bind_cut",
        "lang_ra4_apply_monolith",
        "lang_ra4_apply_bind_cut",
        "lang_ra4_apply_relation_monolith",
        "lang_ra4_apply_relation_bind_cut",
        "lang_ra4_apply_render_bind_cut",
        "lang_ra4_apply_foreach_monolith",
        "lang_ra4_apply_foreach_bind_cut",
        "lang_ra4_apply_derive_message_field",
    ] {
        let row = find_row(row_id).unwrap_or_else(|| panic!("missing matrix row {row_id}"));
        let program = matrix_program_for_row(row, &es);
        let bundle = if row.surface_line {
            compile_plasm_expression(
                &PromptPipelineConfig::default(),
                None,
                &es,
                row.id,
                &program,
            )
        } else {
            compile_plasm_program(
                &PromptPipelineConfig::default(),
                None,
                &es,
                row.id,
                &program,
            )
        }
        .unwrap_or_else(|e| panic!("row {} compile: {e}", row.id));
        let dry = evaluate_plasm_comp_dry(&es, &bundle)
            .unwrap_or_else(|e| panic!("row {} evaluate_plasm_comp_dry: {e}", row.id));
        assert_comp_witness(&dry)
            .unwrap_or_else(|e| panic!("row {} monadic comp witness: {e}", row.id));
    }
}

/// Host-only `wait` / `cancel` continuations (parse smoke; no Hermit live execute).
#[test]
fn lang_wait_cancel_operation_parse() {
    use plasm_core::expr_parser::parse;
    let cgs = language_matrix::load_language_matrix_cgs();
    let wait = parse("wait(l_AAAAAAAAQACAAAAAAAAAAQ_o1)", cgs.as_ref()).expect("wait parse");
    match wait.expr {
        Expr::Wait(w) => assert_eq!(w.handle.as_str(), "l_AAAAAAAAQACAAAAAAAAAAQ_o1"),
        other => panic!("expected Wait, got {other:?}"),
    }
    let cancel = parse("cancel(l_AAAAAAAAQACAAAAAAAAAAQ_o2)", cgs.as_ref()).expect("cancel parse");
    match cancel.expr {
        Expr::Cancel(c) => assert_eq!(c.handle.as_str(), "l_AAAAAAAAQACAAAAAAAAAAQ_o2"),
        other => panic!("expected Cancel, got {other:?}"),
    }
}

/// PLP-8: query / `| take` seed is a compile reject that names the taught Get-identity form.
#[test]
fn lang_iterate_seed_must_be_get_identity() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    let program = r#"items = LangItem
cur = items | take 1
done = iterate cur step LangItem(_.id).ping() until title = "x" take 3
done"#;
    let err = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_iterate_seed_must_be_get_identity",
        program,
    )
    .expect_err("non-Get iterate seed must fail compile");
    let err = err.to_string();
    assert!(
        err.contains("cur = e#(\"id\")")
            && err.contains("e#(tok)")
            && err.contains("e#{id_field=tok}")
            && err.contains("iterate cur step"),
        "diagnostic must name taught Get-identity family, got: {err}"
    );
    assert!(
        !err.contains("carry ir"),
        "must not leak compiler-IR jargon: {err}"
    );

    let rel_err = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_iterate_seed_rejects_relation",
        r#"item = LangItem("i1")
cur = item.tags
done = iterate cur step LangItem(_.id).ping() until title = "x" take 3
done"#,
    )
    .expect_err("relation iterate seed must fail compile");
    let rel_err = rel_err.to_string();
    assert!(
        rel_err.contains("e#(tok)") && rel_err.contains("e#{id_field=tok}"),
        "relation reject must name Get-identity family, got: {rel_err}"
    );

    compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_iterate_until_bound",
        r#"cur = LangCursor("c1")
done = iterate cur step LangCursor(_.id).tick() until phase = "done" take 4
done"#,
    )
    .expect("Get-identity iterate seed must remain executable");

    compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_iterate_bound_identity_seed",
        r#"seed = LangCursor("c1")
cid = seed.id
cur = LangCursor(cid)
done = iterate cur step LangCursor(_.id).tick() until phase = "done" take 4
done"#,
    )
    .expect("bound Get-identity iterate seed must compile");
}

/// PLP-10: unfilled card holes in an executable program are a parse reject, not a value.
#[test]
fn lang_teaching_hole_must_be_filled() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    let assert_fill = |id: &str, program: &str, hole: &str| {
        let err = compile_plasm_program(&PromptPipelineConfig::default(), None, &es, id, program)
            .expect_err(id);
        let err = err.to_string();
        assert!(
            err.contains("PLP-10")
                && err.contains(hole)
                && err.contains("fill it with a bound value")
                && err.contains("row field"),
            "{id} diagnostic must name the fill law, got: {err}"
        );
        assert!(
            !err.to_ascii_lowercase().contains("email"),
            "{id} must stay domain-general: {err}"
        );
    };
    assert_fill("lang_teaching_hole_id", "LangItem(<id>)", "<id>");
    assert_fill(
        "lang_teaching_hole_wire",
        "LangItem{title=<wire>}",
        "<wire>",
    );
    compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_teaching_hole_filled_id",
        r#"LangItem("i1")"#,
    )
    .expect("filled Get identity must remain executable");
    compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_teaching_hole_filled_query",
        r#"LangItem{tags="alpha"}"#,
    )
    .expect("filled query string must remain executable");
}

/// PLP-7 / PLP-12: a taught Minijinja filter as a `|` stage is a parse reject.
/// The diagnostic must name the render/filter lane — not only list row-algebra stages.
#[test]
fn lang_minijinja_filter_pipe_stage_names_render_lane() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    let assert_lane = |id: &str, program: &str, token: &str| {
        let err = compile_plasm_program(&PromptPipelineConfig::default(), None, &es, id, program)
            .expect_err(id);
        let err = err.to_string();
        assert!(
            err.contains("unknown pipe stage")
                && err.contains(token)
                && err.contains("Minijinja")
                && err.contains("{{")
                && err.contains("=> <<TAG"),
            "{id} must name the Minijinja filter lane, got: {err}"
        );
        assert!(
            !err.contains("dirname") && !err.contains("__"),
            "{id} must stay domain-general: {err}"
        );
    };
    assert_lane(
        "lang_minijinja_filter_stage_split_part",
        r#"items = LangItem
out = items | select id | distinct | select dest = id | split_part(id, "-", 0)
out"#,
        "split_part",
    );
    assert_lane(
        "lang_minijinja_filter_stage_urlencode",
        r#"items = LangItem
out = items | urlencode
out"#,
        "urlencode",
    );
    compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_render_split_part_still_lawful",
        r#"items = LangItem("i1") | select id
hdr = items => <<MD
split_part_ok={{ id | split_part('1', 0) }}
MD
hdr"#,
    )
    .expect("lawful per-row `=> <<TAG` split_part must remain executable");
}

/// T214120 29a second family (after the pipe-stage reject): `{ dest: "{{ … }}" }` is
/// lawful Derive, not a program binding of `dest`. Bare `title=dest` stays the
/// existing unknown-binding reject — do not legalize the object key as a label.
#[test]
fn lang_jinja_derive_object_key_is_not_program_binding() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    let assert_unknown_dest = |id: &str, program: &str| {
        let err = compile_plasm_program(&PromptPipelineConfig::default(), None, &es, id, program)
            .expect_err(id);
        let err = err.to_string();
        assert!(
            err.contains("unknown program binding") && err.contains("`dest`"),
            "{id} must reject the object key as a program binding, got: {err}"
        );
        assert!(
            !err.contains("dirname") && !err.contains("__"),
            "{id} must stay domain-general: {err}"
        );
    };
    assert_unknown_dest(
        "lang_jinja_derive_bare_key",
        r#"items = LangItem
dirs = items => { dest: "{{ id | split_part('-', 0) }}" }
out = dirs => LangItem("i1").update(title=dest, score=1, owner="alice")
out"#,
    );
    assert_unknown_dest(
        "lang_unknown_binding_without_derive",
        r#"items = LangItem
out = items => LangItem("i1").update(title=dest, score=1, owner="alice")
out"#,
    );
    compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_jinja_derive_row_field",
        r#"items = LangItem
dirs = items => { dest: "{{ id | split_part('-', 0) }}" }
out = dirs => LangItem("i1").update(title=_.dest, score=1, owner="alice")
out"#,
    )
    .expect("derive column is `_.dest`, not a program binding");
    compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_render_content_still_lawful",
        r#"items = LangItem("i1") | select id
hdr = items => <<MD
{{ id | split_part('-', 0) }}
MD
out = LangItem("i1").update(title=hdr.content, score=1, owner="alice")
out"#,
    )
    .expect("taught `=> <<TAG` / `.content` remains executable");
}

/// PLP-1: Get parentheses are identity-only. Extra args after a positional id are
/// agent-invented — not a taught `e#(id, field=)` form. Method args stay on `.m#`.
#[test]
fn lang_get_identity_parens_reject_extra_args() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    let err = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_get_identity_extra_args",
        r#"LangItem("i1", title="x")"#,
    )
    .expect_err("positional Get plus extra args");
    let err = err.to_string();
    assert!(
        err.contains("PLP-1")
            && err.contains("identity-only")
            && err.contains("e#(<id>).m#(key=value")
            && err.contains("e#{wire=value}"),
        "must name identity vs method parentheses, got: {err}"
    );
    assert!(
        !err.contains("single token") && !err.to_ascii_lowercase().contains("access_token"),
        "must not blame id-token spaces or task-shape the wire: {err}"
    );
    compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_get_identity_unary",
        r#"LangItem("i1")"#,
    )
    .expect("unary Get remains executable");
    compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_get_identity_then_method",
        r#"LangItem("i1").update(title="x", score=1, owner="alice")"#,
    )
    .expect("method parentheses after Get identity remain executable");
}

/// Bounded scalar extraction at bind/argument sites, including empty-source failures.
#[tokio::test]
async fn lang_take_one_field_extract_live() {
    use std::sync::Arc;
    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let mut cgs = (*language_matrix::load_language_matrix_cgs()).clone();
    cgs.http_backend = base.clone();
    let cgs = Arc::new(cgs);
    let es = language_matrix::matrix_execute_session(cgs.clone());
    let st = language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs,
    );
    for id in [
        "lang_take_one_field_bind",
        "lang_take_one_field_argument",
        "lang_take_one_field_bound_argument",
        "lang_take_one_field_empty_bind",
        "lang_take_one_field_empty_argument",
        "lang_get_singleton_field_scalar",
        "lang_get_singleton_field_argument",
        "lang_get_singleton_field_password",
        "lang_get_singleton_field_empty",
    ] {
        matrix_live_run_row(find_row(id).expect("bounded extraction row"), &es, &st).await;
    }
}

/// PLP-11: quoted binding spellings stay literals on the live stack.
#[tokio::test]
async fn quoted_binding_literal_live() {
    use std::sync::Arc;
    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let mut cgs = (*language_matrix::load_language_matrix_cgs()).clone();
    cgs.http_backend = base.clone();
    let cgs = Arc::new(cgs);
    let es = language_matrix::matrix_execute_session(cgs.clone());
    let st = language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs,
    );
    for id in [
        "lang_quoted_binding_literal",
        "lang_quoted_binding_field_literal",
    ] {
        matrix_live_run_row(find_row(id).expect("PLP-11 literal row"), &es, &st).await;
    }
}

/// RA-13: live semi-join / anti-join against a one-column rowset.
#[test]
fn lang_where_in_rowset_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("RA-13 membership runtime");
            rt.block_on(async {
                use std::sync::Arc;
                let base = hermit_lang_matrix::language_matrix_hermit_base_url()
                    .await
                    .clone();
                let mut cgs = (*language_matrix::load_language_matrix_cgs()).clone();
                cgs.http_backend = base.clone();
                let cgs = Arc::new(cgs);
                let es = language_matrix::matrix_execute_session(cgs.clone());
                let st = language_matrix::matrix_host_state(
                    ExecutionEngine::new(ExecutionConfig {
                        base_url: Some(base),
                        ..Default::default()
                    })
                    .expect("ExecutionEngine"),
                    cgs,
                );
                for id in [
                    "lang_where_literal_boolean_sugar",
                    "lang_where_boolean_rowset_sugar",
                    "lang_where_in_rowset",
                    "lang_where_not_in_rowset",
                    "lang_where_in_rowset_paren",
                    "lang_where_not_in_universe_left",
                    "lang_where_not_in_universe_right",
                ] {
                    matrix_live_run_row(find_row(id).expect("RA-13 membership row"), &es, &st)
                        .await;
                }
            });
        })
        .expect("spawn RA-13 membership harness")
        .join()
        .expect("join RA-13 membership harness");
}

/// RA-13 universe: live A minus B and B minus A on two entities.
#[test]
fn where_not_in_universe_ra13_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("RA-13 universe runtime");
            rt.block_on(async {
                use std::sync::Arc;
                let base = hermit_lang_matrix::language_matrix_hermit_base_url()
                    .await
                    .clone();
                let mut cgs = (*language_matrix::load_language_matrix_cgs()).clone();
                cgs.http_backend = base.clone();
                let cgs = Arc::new(cgs);
                let es = language_matrix::matrix_execute_session(cgs.clone());
                let st = language_matrix::matrix_host_state(
                    ExecutionEngine::new(ExecutionConfig {
                        base_url: Some(base),
                        ..Default::default()
                    })
                    .expect("ExecutionEngine"),
                    cgs,
                );
                for id in [
                    "lang_where_not_in_universe_left",
                    "lang_where_not_in_universe_right",
                ] {
                    matrix_live_run_row(find_row(id).expect("RA-13 universe row"), &es, &st).await;
                }
            });
        })
        .expect("spawn RA-13 universe harness")
        .join()
        .expect("join RA-13 universe harness");
}

#[test]
fn lang_union_rowset_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("RA-14 union runtime");
            rt.block_on(async {
                use std::sync::Arc;
                let base = hermit_lang_matrix::language_matrix_hermit_base_url()
                    .await
                    .clone();
                let mut cgs = (*language_matrix::load_language_matrix_cgs()).clone();
                cgs.http_backend = base.clone();
                let cgs = Arc::new(cgs);
                let es = language_matrix::matrix_execute_session(cgs.clone());
                let st = language_matrix::matrix_host_state(
                    ExecutionEngine::new(ExecutionConfig {
                        base_url: Some(base),
                        ..Default::default()
                    })
                    .expect("ExecutionEngine"),
                    cgs,
                );
                for id in [
                    "lang_union_rowset",
                    "lang_union_rowset_alias",
                    "lang_union_rowset_alias_distinct",
                    "lang_union_rowset_alias_existing",
                    "lang_union_rowset_paren",
                    "lang_union_empty_right",
                    "lang_required_selection_default",
                    "lang_required_selection_multi",
                    "lang_required_selection_empty",
                ] {
                    matrix_live_run_row(
                        find_row(id).expect("RA-14 / RA-15 isolation row"),
                        &es,
                        &st,
                    )
                    .await;
                }
            });
        })
        .expect("spawn RA-14 union harness")
        .join()
        .expect("join RA-14 union harness");
}

/// Identity-preserving take-1 `.m#`, empty-before-mutation, and `rows => _.m#`.
#[test]
fn lang_take_one_method_invoke_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("take-1 method invoke runtime");
            rt.block_on(async {
                use std::sync::Arc;
                let base = hermit_lang_matrix::language_matrix_hermit_base_url()
                    .await
                    .clone();
                let mut cgs = (*language_matrix::load_language_matrix_cgs()).clone();
                cgs.http_backend = base.clone();
                let cgs = Arc::new(cgs);
                let es = language_matrix::matrix_execute_session(cgs.clone());
                let st = language_matrix::matrix_host_state(
                    ExecutionEngine::new(ExecutionConfig {
                        base_url: Some(base),
                        ..Default::default()
                    })
                    .expect("ExecutionEngine"),
                    cgs,
                );
                for id in [
                    "lang_take_one_method_invoke",
                    "lang_take_one_method_invoke_empty",
                    "lang_rows_each_method_invoke",
                ] {
                    matrix_live_run_row(find_row(id).expect("take-1 method invoke row"), &es, &st)
                        .await;
                }
            });
        })
        .expect("spawn take-1 method invoke harness")
        .join()
        .expect("join take-1 method invoke harness");
}

/// Closed scalar syntax: invalid expressions cannot become mutation payloads.
#[test]
fn lang_scalar_expression_cannot_become_literal_payload() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    for program in [
        r#"items = LangItem
out = items => LangItem.create(title=_.title | split_part("/") | last, score=0, owner="test")
out"#,
        r#"LangItem.create(title=unknown(value), score=0, owner="test")"#,
        r#"LangItem{owner=missing.field}"#,
        r#"LangItem(unknown(value))"#,
    ] {
        let error = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            &es,
            "closed_scalar_syntax",
            program,
        )
        .expect_err("invalid scalar syntax must not produce an executable plan");
        let message = error.to_string();
        assert!(
            message.contains("unquoted value") || message.contains("unknown value constructor"),
            "must diagnose the scalar boundary: {message}"
        );
    }
    compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "explicit_scalar_template",
        r#"items = LangItem("i1")
rendered = items => <<BODY
{{ title | split_part('/', 0) }}
BODY
LangItem.create(title=rendered.content, score=0, owner="test")"#,
    )
    .expect("explicit template transformation must remain executable");
}
