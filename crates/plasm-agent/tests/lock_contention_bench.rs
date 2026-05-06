use indexmap::IndexMap;
use plasm_agent::execute_path_ids::{ExecuteSessionId, PromptHashHex};
use plasm_agent::execute_session::{ExecuteSession, ExecuteSessionStore, SessionReuseKey};
use plasm_core::{CgsContext, CGS};
use std::sync::Arc;
use std::time::Instant;

#[tokio::test]
#[ignore = "manual perf benchmark"]
async fn bench_session_get_contention() {
    let store = ExecuteSessionStore::default();
    let cgs = Arc::new(CGS::new());
    let reuse_key = SessionReuseKey {
        tenant_scope: String::new(),
        entry_id: "default".into(),
        catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
        entities: vec!["Pet".into()],
        context_intent: None,
        ranked_capabilities: None,
        principal: None,
        plugin_generation_id: None,
        logical_session_id: None,
    };
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        "default".into(),
        Arc::new(CgsContext::entry("default", cgs.clone())),
    );
    let session = ExecuteSession::new(
        "3c61dab1a208fb4c71a5079c0f513f894ce5f65700041943a3e0e2cef2cc6fc1".into(),
        "prompt".into(),
        cgs,
        ctxs,
        "default".into(),
        String::new(),
        String::new(),
        None,
        vec!["Pet".into()],
        None,
        None,
        None,
        "hash".into(),
        None,
        None,
    );
    let sid_str = "d8946f9c00a4474aa1ec0d1b3d4b76b8";
    store
        .insert(
            reuse_key,
            session.prompt_hash.clone(),
            sid_str.into(),
            session,
        )
        .await;
    let ph: PromptHashHex = "3c61dab1a208fb4c71a5079c0f513f894ce5f65700041943a3e0e2cef2cc6fc1"
        .parse()
        .expect("valid prompt hash");
    let sid: ExecuteSessionId = sid_str.parse().expect("valid sid");

    let workers = 64usize;
    let iterations = 500usize;
    let t0 = Instant::now();
    let mut joins = Vec::with_capacity(workers);
    for _ in 0..workers {
        let store = store.clone();
        let ph = ph.clone();
        let sid = sid.clone();
        joins.push(tokio::spawn(async move {
            let mut local = Vec::with_capacity(iterations);
            for _ in 0..iterations {
                let s = Instant::now();
                let ok = store.get(&ph, &sid).await.is_some();
                local.push((ok, s.elapsed().as_micros() as u64));
            }
            local
        }));
    }
    let mut samples: Vec<u64> = Vec::with_capacity(workers * iterations);
    for j in joins {
        for (ok, dur) in j.await.expect("join") {
            assert!(ok);
            samples.push(dur);
        }
    }
    samples.sort_unstable();
    let idx = |p: f64| -> usize { ((samples.len() as f64) * p).floor() as usize };
    let p50 = samples[idx(0.50).min(samples.len() - 1)];
    let p95 = samples[idx(0.95).min(samples.len() - 1)];
    let p99 = samples[idx(0.99).min(samples.len() - 1)];
    eprintln!(
        "bench_session_get_contention total_ms={} samples={} p50_us={} p95_us={} p99_us={}",
        t0.elapsed().as_millis(),
        samples.len(),
        p50,
        p95,
        p99
    );
}
