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

use plasm_agent::plasm_compile::compile_plasm_program;
use plasm_agent::plasm_plan_run::evaluate_plasm_comp_dry;
use plasm_core::{Expr, PromptPipelineConfig};
use plasm_runtime::{ExecutionConfig, ExecutionEngine};

use assert_live::assert_comp_witness;
use assert_planning::assert_planning_ir;
use harness::{matrix_live_run_row, plasm_language_matrix_live_runs_async};
use program::matrix_program_for_row;
use rows::{all_rows, find_row, row_count};

#[tokio::test]
async fn plasm_language_matrix_cgs_templates_validate() {
    let cgs = language_matrix::load_language_matrix_cgs();
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("capability CML templates");
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
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            &es,
            row.id,
            &program,
        )
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
