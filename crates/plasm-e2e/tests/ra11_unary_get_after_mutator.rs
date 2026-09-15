//! Live RA-11 witness: unary Get after an in-session mutator must re-observe.
//!
//! Graph execute forks at each node (`branch_commit.rs` `seed_read_branch`).
//! If fork resets inherited recorded-read reuse, the trailing Get serves the
//! pre-write Complete row.

#[path = "common/hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

#[path = "common/language_matrix.rs"]
mod language_matrix;

use plasm_agent::plasm_compile::compile_plasm_program;
use plasm_agent::plasm_plan_run::run_plasm_comp;
use plasm_core::PromptPipelineConfig;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionSource};

#[test]
fn ra11_unary_get_after_mutator_reobserves_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(ra11_unary_get_after_mutator_reobserves_live_impl());
        })
        .expect("spawn")
        .join()
        .expect("join");
}

async fn ra11_unary_get_after_mutator_reobserves_live_impl() {
    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs.clone());
    let st = language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .expect("engine"),
        cgs,
    );

    let program = r#"seed = LangItem("i1")
patched = LangItem("i1").update(title="PostMutatorTitle", score=1, owner="alice")
again = LangItem("i1")
again"#;
    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "ra11_unary_get_after_mutator",
        program,
    )
    .expect("compile");
    let live = run_plasm_comp(
        &es,
        &st,
        es.prompt_hash.as_str(),
        "ra11_reobserve",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("live");

    let again = live
        .return_steps
        .iter()
        .find(|s| s.node_id.as_deref() == Some("again"))
        .unwrap_or_else(|| {
            panic!(
                "missing again Get step; steps={:?}",
                live.return_steps
                    .iter()
                    .map(|s| (
                        s.node_id.clone(),
                        s.display.clone(),
                        s.result.source,
                        s.result.stats.network_requests
                    ))
                    .collect::<Vec<_>>()
            )
        });
    assert_ne!(
        again.result.source,
        ExecutionSource::Cache,
        "RA-11: unary Get after mutator must not consult the pre-write snapshot (source={:?}, net={})",
        again.result.source,
        again.result.stats.network_requests
    );
    assert!(
        again.result.stats.network_requests >= 1 || again.result.source == ExecutionSource::Live,
        "RA-11: trailing Get must re-observe live HTTP (source={:?}, net={})",
        again.result.source,
        again.result.stats.network_requests
    );
}
