use std::path::PathBuf;
use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use plasm_core::loader::load_schema_dir;
use plasm_core::schema::CGS;
use plasm_discovery::index::CatalogIndex;
use plasm_discovery::{AgentDiscovery, CatalogIndexCache, DiscoveryQuery, TypedDiscovery};
use plasm_discovery_eval::{case_intents, default_cases_path, load_cases};
use tokio::runtime::Runtime;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn github_cgs() -> Arc<CGS> {
    Arc::new(load_schema_dir(&repo_root().join("apis/github")).expect("github schema"))
}

fn eval_intents() -> Vec<String> {
    let path = default_cases_path();
    if path.is_file() {
        case_intents(&load_cases(&path).expect("cases"))
    } else {
        Vec::new()
    }
}

fn bench_index_build(c: &mut Criterion) {
    let cgs = github_cgs();
    c.bench_function("CatalogIndex::build/github", |b| {
        b.iter(|| black_box(CatalogIndex::build("github".into(), cgs.clone())))
    });
}

fn bench_scan_utterance(c: &mut Criterion) {
    let idx = CatalogIndex::build("github".into(), github_cgs());
    let cases = eval_intents();
    if cases.is_empty() {
        return;
    }
    c.bench_function("scan_utterance/eval_cases", |b| {
        b.iter(|| {
            for intent in &cases {
                black_box(idx.scan_utterance(intent));
            }
        })
    });
}

fn bench_typed_discover(c: &mut Criterion) {
    let cgs = github_cgs();
    let rt = Runtime::new().unwrap();
    let intents = eval_intents();
    if intents.is_empty() {
        return;
    }
    c.bench_function("typed_discover/eval_cases", |b| {
        b.iter(|| {
            rt.block_on(async {
                let disc =
                    TypedDiscovery::from_cgs_entries(vec![("github".into(), cgs.clone())], None);
                for intent in &intents {
                    let q = DiscoveryQuery {
                        utterance: intent.clone(),
                        allowed_entry_ids: vec!["github".into()],
                        ..Default::default()
                    };
                    black_box(disc.discover(q).await.unwrap());
                }
            })
        })
    });
}

fn bench_index_cache_hit(c: &mut Criterion) {
    let cache = CatalogIndexCache::new();
    let cgs = github_cgs();
    c.bench_function("CatalogIndexCache/get_or_build_hit", |b| {
        cache.get_or_build("github".into(), cgs.clone());
        b.iter(|| black_box(cache.get_or_build("github".into(), cgs.clone())))
    });
}

criterion_group!(
    benches,
    bench_index_build,
    bench_scan_utterance,
    bench_typed_discover,
    bench_index_cache_hit
);
criterion_main!(benches);
