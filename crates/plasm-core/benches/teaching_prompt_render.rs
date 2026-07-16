//! teaching table / teaching TSV synthesis benchmarks (CGS materialization + prompt render).
//!
//! Run (from `plasm-oss/`): `cargo bench -p plasm-core --bench teaching_prompt_render`
//!
//! Fast CI wall-time guard (same hot path, best-of-three):
//! `cargo test -p plasm-core prompt_matrix_full_tsv_synthesis_benchmark`
//! Override cap: `PLASM_PROMPT_MATRIX_SYNTH_MAX_MS=<ms>`.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use plasm_core::discovery::{
    derive_intent_exposure_surface_batch, ExposureSurfaceOptions, MutatorAdmit,
};
use plasm_core::loader::load_schema_dir;
use plasm_core::prompt_render::{
    render_prompt_tsv_with_config, render_teaching_prompt_bundle_for_exposure, RenderConfig,
};
use plasm_core::symbol_tuning::TeachingExposureSession;
use plasm_core::{relation_endpoint_keys, CGS};
use std::path::PathBuf;
use std::sync::Arc;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_schema(name: &str) -> Option<PathBuf> {
    let p = manifest_dir().join("../../fixtures/schemas").join(name);
    p.is_dir().then_some(p)
}

fn apis_schema(name: &str) -> Option<PathBuf> {
    let p = manifest_dir().join("../../apis").join(name);
    p.is_dir().then_some(p)
}

fn load_cgs(path: &std::path::Path) -> Arc<CGS> {
    Arc::new(load_schema_dir(path).expect("load schema dir"))
}

fn teaching_prompt_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("teaching_prompt");
    group.sample_size(30);
    let config = RenderConfig::for_eval(None);
    let validation_config = RenderConfig::for_expression_surface_validation();

    if let Some(path) = fixture_schema("plasm_prompt_matrix") {
        let cgs = load_cgs(&path);
        group.bench_with_input(
            BenchmarkId::new("render_prompt_tsv_all_entities", "plasm_prompt_matrix"),
            &cgs,
            |b, cgs: &Arc<CGS>| {
                b.iter(|| render_prompt_tsv_with_config(black_box(cgs.as_ref()), config));
            },
        );
        group.bench_with_input(
            BenchmarkId::new(
                "render_prompt_tsv_validation_all_entities",
                "plasm_prompt_matrix",
            ),
            &cgs,
            |b, cgs: &Arc<CGS>| {
                b.iter(|| {
                    render_prompt_tsv_with_config(black_box(cgs.as_ref()), validation_config)
                });
            },
        );
    }

    if let Some(path) = fixture_schema("plasm_language_matrix") {
        let cgs = load_cgs(&path);
        group.bench_with_input(
            BenchmarkId::new("render_prompt_tsv_all_entities", "plasm_language_matrix"),
            &cgs,
            |b, cgs: &Arc<CGS>| {
                b.iter(|| render_prompt_tsv_with_config(black_box(cgs.as_ref()), config));
            },
        );
    }

    if let Some(path) = apis_schema("github") {
        let cgs = load_cgs(&path);
        let entity = "Repository";
        let intent = "list filter aggregate repositories by owner";
        group.bench_with_input(
            BenchmarkId::new("execute_first_wave_bundle", "github_repository"),
            &cgs,
            |b, cgs: &Arc<CGS>| {
                b.iter(|| {
                    let entry = cgs.entry_id.as_deref().unwrap_or("github");
                    let relation_keys = relation_endpoint_keys(entry, &[entity.to_string()]);
                    let delta = derive_intent_exposure_surface_batch(
                        cgs.as_ref(),
                        entry,
                        intent,
                        &relation_keys,
                        &[entity.to_string()],
                        None,
                        ExposureSurfaceOptions {
                            mutator_admit: MutatorAdmit::AlwaysOnSeeds,
                        },
                    );
                    let exposure = TeachingExposureSession::new_with_intent_delta(
                        cgs.as_ref(),
                        entry,
                        &[entity],
                        delta,
                    );
                    black_box(render_teaching_prompt_bundle_for_exposure(
                        cgs.as_ref(),
                        config,
                        &exposure,
                        None,
                    ));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("render_prompt_tsv_validation_all_entities", "github"),
            &cgs,
            |b, cgs: &Arc<CGS>| {
                b.iter(|| {
                    render_prompt_tsv_with_config(black_box(cgs.as_ref()), validation_config)
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, teaching_prompt_benchmarks);
criterion_main!(benches);
