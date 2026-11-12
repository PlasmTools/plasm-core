//! Public failure contract for an empty singleton input through Python admission and live execution.
#[path = "common/hermit_lang_matrix.rs"]
#[allow(dead_code)]
mod hermit_lang_matrix;
#[path = "common/language_matrix.rs"]
#[allow(dead_code)]
mod language_matrix;

use plasm_runtime::{ExecutionConfig, ExecutionEngine, FailureCause, RecoveryDisposition};

#[tokio::test]
async fn empty_singleton_input_has_repairable_redacted_failure() {
    let base = hermit_lang_matrix::fresh_python_parity_hermit_base_url().await;
    let cgs = language_matrix::load_language_matrix_cgs();
    let session = language_matrix::matrix_execute_session(cgs.clone());
    let symbols = session.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let source = format!("class Example(Program):\n    def build(self):\n        selected = {entity}.query().take(0)\n        return selected.id\n");
    let bundle = plasm_agent::plasm_compile::compile_python_program(&session, &source)
        .await
        .unwrap();
    plasm_agent::plasm_plan_run::evaluate_plasm_comp_dry(&session, &bundle).unwrap();
    let host = language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .unwrap(),
        cgs,
    );
    let failure = plasm_agent::plasm_plan_run::run_plasm_comp_python(
        &session,
        &host,
        &session.prompt_hash,
        "input-failure",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await
    .expect_err("empty singleton cannot supply a scalar");
    assert_eq!(failure.cause, FailureCause::Program, "{failure:?}");
    assert_eq!(
        failure.recovery,
        RecoveryDisposition::RepairProgram,
        "{failure:?}"
    );
    assert_eq!(
        failure.code, "plan_input_cardinality_invalid",
        "{failure:?}"
    );
    assert!(failure.diagnostic().contains("zero rows"));
    assert!(failure.node.is_some());
    assert!(failure.dispatches.is_empty());
    assert!(failure.effects.is_empty());
    assert!(!failure.diagnostic().contains("__py"));
}
