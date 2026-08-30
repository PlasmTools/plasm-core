//! Regression: teaching-table render must not require process-wide `RUST_MIN_STACK`.

use std::path::PathBuf;

use crate::loader::load_schema_dir;
use crate::prompt_pipeline::PromptPipelineConfig;
use crate::symbol_tuning::{teaching_exposure_session_from_focus, FocusSpec, TeachingExposureSession};

fn stack_bytes() -> usize {
    std::env::var("PLASM_TEST_STACK_MIB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2)
        * 1024
        * 1024
}

fn matrix_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix")
}

fn run_on_stack<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    let stack = stack_bytes();
    eprintln!("stack_budget_bytes={stack}");
    let join = std::thread::Builder::new()
        .name("teaching-render-stack-budget".into())
        .stack_size(stack)
        .spawn(f)
        .expect("spawn stack-budget thread");
    join.join()
        .expect("worker stack overflow — teaching render must not rely on RUST_MIN_STACK");
}

#[test]
fn teaching_first_wave_render_survives_two_mib_worker_stack_lang_item() {
    run_on_stack(|| {
        let dir = matrix_fixture_dir();
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
        let exposure = TeachingExposureSession::new(&cgs, "matrix", &["LangItem"]);
        let pipeline = PromptPipelineConfig::default();
        let out = pipeline.render_teaching_first_wave_for_session(&cgs, &exposure, None);
        assert!(
            !out.is_empty(),
            "first-wave teaching TSV must render non-empty output"
        );
    });
}

#[test]
fn teaching_first_wave_render_survives_two_mib_worker_stack_all_focus() {
    run_on_stack(|| {
        let dir = matrix_fixture_dir();
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
        let exposure = teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
        let pipeline = PromptPipelineConfig::default();
        let out = pipeline.render_teaching_first_wave_for_session(&cgs, &exposure, None);
        assert!(
            !out.is_empty(),
            "All-focus teaching TSV must render non-empty output"
        );
    });
}

#[test]
fn teaching_first_wave_render_survives_two_mib_worker_stack_prompt_matrix() {
    run_on_stack(|| {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_prompt_matrix");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("plasm_prompt_matrix");
        let exposure = teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
        let pipeline = PromptPipelineConfig::default();
        let out = pipeline.render_teaching_first_wave_for_session(&cgs, &exposure, None);
        assert!(
            !out.is_empty(),
            "prompt_matrix All-focus teaching TSV must render non-empty output"
        );
    });
}
