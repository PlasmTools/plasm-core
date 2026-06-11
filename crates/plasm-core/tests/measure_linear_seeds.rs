use plasm_core::loader::load_schema_dir;
use plasm_core::prompt_render::{render_prompt_tsv_with_config, PromptRenderMode, RenderConfig};
use plasm_core::symbol_tuning::FocusSpec;
use plasm_core::PromptPipelineConfig;
use std::path::PathBuf;

fn measure(api: &str, seeds: &[&str]) {
    let schema = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apis")
        .join(api);
    let cgs = load_schema_dir(&schema).unwrap();
    let cfg = RenderConfig {
        focus: FocusSpec::SeedsExact(seeds),
        render_mode: PromptRenderMode::Tsv,
        include_domain_execution_model: true,
        symbol_map_cross_cache: None,
    };
    let prompt = render_prompt_tsv_with_config(&cgs, cfg);
    let st = PromptPipelineConfig::default().prompt_surface_stats(&cgs, None, &prompt);
    eprintln!(
        "{} SEEDS_EXACT {:?} | {} | bytes={}",
        api,
        seeds,
        st.summary_line_body(),
        prompt.len()
    );
}

#[test]
fn measure_task_slices() {
    measure("linear", &["Team", "Issue"]);
    measure("github", &["Repository", "Issue"]);
    measure("github", &["Repository", "Issue", "PullRequest", "User"]);
}
