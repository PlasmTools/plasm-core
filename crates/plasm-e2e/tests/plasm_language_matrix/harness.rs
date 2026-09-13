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

    let cgs_live = {
        let mut live = (*cgs).clone();
        live.http_backend = base.clone();
        Arc::new(live)
    };
    let es = Arc::new(language_matrix::matrix_execute_session(cgs_live.clone()));
    let cgs_federated_primary = {
        let mut primary = language_matrix::cgs_with_registry_entry_id(
            cgs.as_ref(),
            language_matrix::MATRIX_FED_A,
        );
        primary.http_backend = base.clone();
        Arc::new(primary)
    };
    let cgs_secondary = {
        let mut secondary = language_matrix::cgs_with_registry_entry_id(
            cgs.as_ref(),
            language_matrix::MATRIX_FED_B,
        );
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
        cgs_live.clone(),
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
            cgs_live.clone(),
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

        // PLP-8 iterate rows must not inherit Completeness::Complete LangCursor rows
        // from a prior program on the shared suite host.
        let isolated_iterate_host;
        let (row_es, row_st) = if row.federated {
            (row_es, row_st)
        } else if row
            .features
            .iter()
            .any(|t| t.starts_with("iterate_until_") || *t == "iterate_bound_exhausted")
        {
            isolated_iterate_host = Arc::new(language_matrix::matrix_host_state(
                ExecutionEngine::new(ExecutionConfig {
                    base_url: Some(base.clone()),
                    ..Default::default()
                })
                .expect("ExecutionEngine"),
                cgs_live.clone(),
            ));
            (
                Arc::new(language_matrix::matrix_execute_session(cgs_live.clone())),
                isolated_iterate_host,
            )
        } else {
            (row_es, row_st)
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
        let reset_base = row_st
            .engine
            .config()
            .base_url
            .as_deref()
            .unwrap_or(row_es.cgs.http_backend.as_str());
        hermit_lang_matrix::language_matrix_reset_lang_cursors_on(reset_base).await;
    }

    let program = matrix_program_for_row(row, row_es);
    let compiled = if row.surface_line {
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
    };
    if let Some(err_sub) = row.expect_live_error {
        if let Err(e) = compiled {
            let msg = e.to_string();
            assert!(
                msg.contains(err_sub),
                "row {}: compile error missing {err_sub:?}: {msg}",
                row.id
            );
            return;
        }
    }
    let bundle = compiled.unwrap_or_else(|e| panic!("row {} compile: {e}", row.id));

    let comp_json = serde_json::to_value(&bundle.artifact().comp)
        .unwrap_or_else(|e| panic!("row {} comp json: {e}", row.id));

    let dry = evaluate_plasm_comp_dry(row_es, &bundle)
        .unwrap_or_else(|e| panic!("row {} evaluate_plasm_comp_dry: {e}", row.id));
    assert_planning_ir(row, &dry, &comp_json)
        .unwrap_or_else(|e| panic!("row {} planning IR: {e}", row.id));
    assert_comp_witness(&dry)
        .unwrap_or_else(|e| panic!("row {} monadic comp witness: {e}", row.id));

    // Each matrix program is an independent execute. Sharing one session id lets a prior
    // Completeness::Complete unary Get (e.g. LangCursor("c_done")) satisfy a later Get
    // of a different key and silence PLP-8 bound exhaustion.
    let session_id = row.id;

    if let Some(err_sub) = row.expect_live_error {
        let live = Box::pin(run_plasm_comp(
            row_es,
            row_st,
            row_es.prompt_hash.as_str(),
            session_id,
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
        session_id,
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
