//! Abstract live witness: payer vs debtor are opposite row roles (`| where`), not dest tasks.

use std::future::IntoFuture;
use std::sync::Arc;

use axum::{http::Uri, Json, Router};
use indexmap::IndexMap;
use plasm_agent::execute_session::ExecuteSession;
use plasm_agent::http::{build_plasm_host_state, PlasmHostBootstrap};
use plasm_agent::plasm_compile::compile_plasm_program;
use plasm_agent::plasm_plan_run::run_plasm_comp;
use plasm_agent::run_artifacts::RunArtifactStore;
use plasm_agent::server_state::CatalogBootstrap;
use plasm_core::discovery::CgsRegistry;
use plasm_core::{CgsContext, PromptPipelineConfig, TeachingExposureSession};
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

const ENTRY: &str = "direction";

fn load_direction_cgs() -> Arc<plasm_core::CGS> {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_direction_matrix");
    Arc::new(
        plasm_core::loader::load_schema_dir(&dir)
            .unwrap_or_else(|e| panic!("load plasm_direction_matrix from {}: {e}", dir.display())),
    )
}

fn direction_session(cgs: Arc<plasm_core::CGS>) -> ExecuteSession {
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        ENTRY.into(),
        Arc::new(CgsContext::entry(ENTRY, cgs.clone())),
    );
    let wave: &[&str] = &["LedgerEntry"];
    let exp = TeachingExposureSession::new(cgs.as_ref(), ENTRY, wave);
    ExecuteSession::new(
        "direction_ph".into(),
        String::new(),
        cgs.clone(),
        ctxs,
        ENTRY.into(),
        String::new(),
        String::new(),
        None,
        wave.iter().map(|s| (*s).to_string()).collect(),
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    )
}

fn direction_host(
    engine: ExecutionEngine,
    cgs: Arc<plasm_core::CGS>,
) -> plasm_agent::server_state::PlasmHostState {
    let registry = Arc::new(CgsRegistry::from_pairs(vec![(
        ENTRY.into(),
        "Direction matrix".into(),
        vec!["matrix".into()],
        cgs,
    )]));
    build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry,
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}

async fn ledger_payload(uri: Uri) -> Json<serde_json::Value> {
    assert!(
        uri.path().contains("ledger"),
        "expected ledger path, got {}",
        uri.path()
    );
    Json(serde_json::json!([
        {"id": "t1", "payer": "alice", "debtor": "bob", "amount": 10},
        {"id": "t2", "payer": "bob", "debtor": "alice", "amount": 7}
    ]))
}

#[test]
fn direction_payer_versus_debtor_where_programs_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(direction_payer_versus_debtor_where_programs_live_impl());
        })
        .expect("spawn")
        .join()
        .expect("join");
}

async fn direction_payer_versus_debtor_where_programs_live_impl() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server =
        tokio::spawn(axum::serve(listener, Router::new().fallback(ledger_payload)).into_future());

    let mut cgs = (*load_direction_cgs()).clone();
    cgs.http_backend = base.clone();
    let cgs = Arc::new(cgs);
    let es = direction_session(cgs.clone());
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base),
        hydrate: false,
        ..Default::default()
    })
    .expect("engine");
    let st = direction_host(engine, cgs);

    let paid = run_direction_program(
        &es,
        &st,
        "sent = LedgerEntry | where payer = \"alice\"\nsent",
    )
    .await;
    let owed = run_direction_program(
        &es,
        &st,
        "recv = LedgerEntry | where debtor = \"alice\"\nrecv",
    )
    .await;
    assert_eq!(
        paid,
        vec!["t1".to_string()],
        "payer=alice is sent, not received"
    );
    assert_eq!(
        owed,
        vec!["t2".to_string()],
        "debtor=alice is received, not sent"
    );
    server.abort();
}

async fn run_direction_program(
    es: &ExecuteSession,
    st: &plasm_agent::server_state::PlasmHostState,
    program: &str,
) -> Vec<String> {
    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        es,
        "direction_polarity",
        program,
    )
    .unwrap_or_else(|e| panic!("compile {program}: {e}"));
    let live = run_plasm_comp(
        es,
        st,
        es.prompt_hash.as_str(),
        "direction",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("live {program}: {e}"));
    live.return_steps
        .iter()
        .flat_map(|s| s.result.entities.iter())
        .map(|e| e.reference.primary_slot_str().to_string())
        .collect()
}
