//! Focused live regression for chained one-cardinality `from_parent_get` hops.

#[path = "common/hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

#[path = "common/language_matrix.rs"]
mod language_matrix;

use plasm_agent::plasm_compile::compile_plasm_program;
use plasm_agent::plasm_plan_run::run_plasm_comp;
use plasm_core::PromptPipelineConfig;
use plasm_runtime::{ExecutionConfig, ExecutionEngine};

#[test]
fn lang_bind_relation_hop_one_one_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(lang_bind_relation_hop_one_one_live_impl());
        })
        .expect("spawn")
        .join()
        .expect("join");
}

async fn lang_bind_relation_hop_one_one_live_impl() {
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

    let program = r#"item = LangItem("i1")
summary = item.summary
detail = summary.detail
detail"#;
    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "lang_bind_relation_hop_one_one",
        program,
    )
    .expect("compile");
    let live = run_plasm_comp(
        &es,
        &st,
        es.prompt_hash.as_str(),
        "hop_test",
        &bundle,
        true,
        None,
        None,
        None,
    )
    .await
    .expect("live");

    let return_rows: usize = live.return_steps.iter().map(|s| s.result.count).sum();
    assert!(
        return_rows > 0,
        "expected non-zero detail return rows; return_steps={:?}\nmarkdown:\n{}",
        live.return_steps
            .iter()
            .map(|s| (s.display.as_str(), s.result.count))
            .collect::<Vec<_>>(),
        live.run_markdown.as_deref().unwrap_or("")
    );
}
