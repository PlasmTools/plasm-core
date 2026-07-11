use std::path::PathBuf;
use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_core::schema::CGS;
use plasm_discovery_eval::{baseline_discover, case_intents, default_cases_path, load_cases};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn load_github() -> CGS {
    load_schema_dir(&repo_root().join("apis/github")).expect("github schema")
}

fn github_registry() -> InMemoryCgsRegistry {
    let cgs = Arc::new(load_github());
    InMemoryCgsRegistry::from_pairs(vec![(
        "github".to_string(),
        "GitHub".to_string(),
        vec![],
        cgs,
    )])
}

fn bench_schema_load(c: &mut Criterion) {
    let dir = repo_root().join("apis/github");
    c.bench_function("load_schema_dir/github", |b| {
        b.iter(|| black_box(load_schema_dir(&dir).unwrap()))
    });
}

fn bench_catalog_hash(c: &mut Criterion) {
    let cgs = load_github();
    c.bench_function("catalog_cgs_hash_hex/github", |b| {
        b.iter(|| black_box(cgs.catalog_cgs_hash_hex()))
    });
}

fn bench_validate(c: &mut Criterion) {
    let cgs = load_github();
    c.bench_function("CGS::validate/github", |b| {
        b.iter(|| {
            cgs.validate().unwrap();
        })
    });
}

fn bench_legacy_discover(c: &mut Criterion) {
    let reg = github_registry();
    let path = default_cases_path();
    if !path.is_file() {
        return;
    }
    let intents = case_intents(&load_cases(&path).expect("cases"));
    c.bench_function("legacy_discover/eval_cases", |b| {
        b.iter(|| {
            for intent in &intents {
                black_box(baseline_discover(&reg, intent, 24).unwrap());
            }
        })
    });
}

criterion_group!(
    benches,
    bench_schema_load,
    bench_catalog_hash,
    bench_validate,
    bench_legacy_discover
);
criterion_main!(benches);
