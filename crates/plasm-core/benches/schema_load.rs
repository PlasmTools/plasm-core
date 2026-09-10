use std::path::PathBuf;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use plasm_core::loader::load_schema_dir;
use plasm_core::schema::CGS;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn load_github() -> CGS {
    load_schema_dir(&repo_root().join("apis/github")).expect("github schema")
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

criterion_group!(
    benches,
    bench_schema_load,
    bench_catalog_hash,
    bench_validate,
);
criterion_main!(benches);
