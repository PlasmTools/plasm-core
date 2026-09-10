//! Hermit-backed live execute harness for matrix rows.

use std::collections::BTreeSet;

use plasm_agent::execute_session::ExecuteSession;
use plasm_agent::plasm_compile::{compile_plasm_expression, compile_plasm_program};
use plasm_agent::plasm_plan_run::{evaluate_plasm_comp_dry, run_plasm_comp};
use plasm_agent::server_state::PlasmHostState;
use plasm_core::PromptPipelineConfig;
use plasm_runtime::{ExecutionConfig, ExecutionEngine};

use super::assert_live::{assert_comp_witness, assert_row};
use super::assert_planning::assert_planning_ir;
use super::features::REQUIRED_FEATURE_TAGS;
use super::hermit_lang_matrix;
use super::language_matrix;
use super::program::matrix_program_for_row;
use super::row::MatrixRow;
use super::rows;

pub(crate) async fn plasm_language_matrix_live_runs_async() {
    let base = hermit_lang_matrix::language_matrix_suite_hermit_base_url()
        .await
        .clone();
    plasm_language_matrix_live_runs_body(base).await;
}

pub(crate) async fn plasm_language_matrix_live_runs_body(base: String) {
    use std::sync::Arc;

    let cgs = language_matrix::load_language_matrix_cgs();
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("templates");

    let es = Arc::new(language_matrix::matrix_execute_session(cgs.clone()));
    let cgs_federated_primary = {
        let mut primary = language_matrix::cgs_with_registry_entry_id(cgs.as_ref(), "linear");
        primary.http_backend = base.clone();
        Arc::new(primary)
    };
    let cgs_secondary = {
        let mut secondary = language_matrix::cgs_with_registry_entry_id(cgs.as_ref(), "pokeapi");
        secondary.http_backend = base.clone();
        Arc::new(secondary)
    };
    let es_federated = Arc::new(language_matrix::matrix_federated_relation_target_session(
        cgs_federated_primary.clone(),
        cgs_secondary.clone(),
    ));
    let st = Arc::new(language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base.clone()),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs.clone(),
    ));
    let st_federated = Arc::new(language_matrix::matrix_federated_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base.clone()),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs_federated_primary,
        cgs_secondary.clone(),
    ));
    let cgs_live = {
        let mut live = (*cgs).clone();
        live.http_backend = base.clone();
        Arc::new(live)
    };
    let es_federated_dup = Arc::new(language_matrix::matrix_federated_duplicate_entity_session(
        cgs_live.clone(),
    ));
    let es_federated_auth = Arc::new(language_matrix::matrix_federated_auth_session_session(
        cgs_live.clone(),
    ));
    let st_federated_dup = Arc::new(
        language_matrix::matrix_federated_duplicate_entity_host_state(
            ExecutionEngine::new(ExecutionConfig {
                base_url: Some(base.clone()),
                ..Default::default()
            })
            .expect("ExecutionEngine"),
            cgs_live,
        ),
    );

    let mut tags_seen: BTreeSet<String> = BTreeSet::new();

    for row in rows::all_rows() {
        let (row_es, row_st) = if matches!(
            row.id,
            "lang_federated_duplicate_entity_e1_query"
                | "lang_federated_duplicate_entity_e2_search"
                | "lang_federated_duplicate_entity_relation_r"
                | "lang_federated_duplicate_entity_mutator_m"
                | "lang_federated_duplicate_entity_pathless_action"
                | "lang_federated_parallel_roots"
                | "lang_federated_group_by_on_e1"
                | "lang_bind_template_inline_on_e1"
        ) {
            (Arc::clone(&es_federated_dup), Arc::clone(&st_federated_dup))
        } else if row.id == "lang_federated_auth_session_provides_mutation" {
            (
                Arc::clone(&es_federated_auth),
                Arc::clone(&st_federated_dup),
            )
        } else if row.federated {
            (Arc::clone(&es_federated), Arc::clone(&st_federated))
        } else {
            (Arc::clone(&es), Arc::clone(&st))
        };

        matrix_live_run_row(row, row_es.as_ref(), row_st.as_ref()).await;

        for t in row.features {
            tags_seen.insert((*t).to_string());
        }
    }

    let required: BTreeSet<String> = REQUIRED_FEATURE_TAGS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    tags_seen.insert("host_wait_cancel".to_string());
    tags_seen.insert("monadic_comp_witness".to_string());
    let missing: Vec<_> = required.difference(&tags_seen).cloned().collect();
    assert!(
        missing.is_empty(),
        "missing required feature tag coverage: {missing:?}"
    );
}

pub(crate) async fn matrix_live_run_row(
    row: &'static MatrixRow,
    row_es: &ExecuteSession,
    row_st: &PlasmHostState,
) {
    if row.expect_live_error.is_some()
        || row
            .features
            .iter()
            .any(|t| t.starts_with("iterate_until_") || *t == "iterate_bound_exhausted")
    {
        hermit_lang_matrix::language_matrix_reset_lang_cursors_on(&row_es.cgs.http_backend).await;
    }

    let program = matrix_program_for_row(row, row_es);
    let bundle = if row.surface_line {
        compile_plasm_expression(
            &PromptPipelineConfig::default(),
            None,
            row_es,
            row.id,
            &program,
        )
    } else {
        compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            row_es,
            row.id,
            &program,
        )
    }
    .unwrap_or_else(|e| panic!("row {} compile: {e}", row.id));

    let comp_json = serde_json::to_value(&bundle.artifact().comp)
        .unwrap_or_else(|e| panic!("row {} comp json: {e}", row.id));

    let dry = evaluate_plasm_comp_dry(row_es, &bundle)
        .unwrap_or_else(|e| panic!("row {} evaluate_plasm_comp_dry: {e}", row.id));
    assert_planning_ir(row, &dry, &comp_json)
        .unwrap_or_else(|e| panic!("row {} planning IR: {e}", row.id));
    assert_comp_witness(&dry)
        .unwrap_or_else(|e| panic!("row {} monadic comp witness: {e}", row.id));

    if let Some(err_sub) = row.expect_live_error {
        let live = Box::pin(run_plasm_comp(
            row_es,
            row_st,
            row_es.prompt_hash.as_str(),
            "matrix_sess",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        ))
        .await;
        let err = live.expect_err(&format!(
            "row {}: expected live failure containing {err_sub:?}",
            row.id
        ));
        assert!(
            err.contains(err_sub),
            "row {}: live error missing {err_sub:?}: {err}",
            row.id
        );
        return;
    }

    let live = Box::pin(run_plasm_comp(
        row_es,
        row_st,
        row_es.prompt_hash.as_str(),
        "matrix_sess",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    ))
    .await
    .unwrap_or_else(|e| panic!("row {} run_plasm_comp: {e}", row.id));

    assert_row(row, &live).unwrap_or_else(|e| panic!("row {} assertion: {e}", row.id));
}
