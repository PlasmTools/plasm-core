//! Live E2E: PokéAPI catalog + `plasm-plugin-stub` + HTTP execute (compile path uses the dylib).
//!
//! ```bash
//! cargo build -p plasm-plugin-stub
//! cargo test -p plasm-agent --test pokeapi_compile_plugin_e2e -- --ignored --nocapture
//! ```

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use plasm_agent::http::{build_plasm_host_state, discovery_execute_router, PlasmHostBootstrap};
use plasm_agent::server_state::CatalogBootstrap;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema;
use plasm_plugin_host::PluginManager;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

fn stub_dylib_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let target_debug = manifest.join("../../target/debug");
    let name = if cfg!(target_os = "macos") {
        "libplasm_plugin_stub.dylib"
    } else if cfg!(target_os = "windows") {
        "plasm_plugin_stub.dll"
    } else {
        "libplasm_plugin_stub.so"
    };
    target_debug.join(name)
}

#[tokio::test]
#[ignore = "requires outbound HTTPS to pokeapi.co and target/debug plasm-plugin-stub cdylib"]
async fn pokeapi_execute_session_compiles_via_plugin_and_queries_live_api() {
    let stub = stub_dylib_path();
    assert!(
        stub.exists(),
        "build the stub first: cargo build -p plasm-plugin-stub (expected {})",
        stub.display()
    );

    let poke_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
    let cgs =
        Arc::new(load_schema(&poke_dir).unwrap_or_else(|e| panic!("load pokeapi schema: {e}")));
    let reg = Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
        "pokeapi".into(),
        "pokeapi".into(),
        vec![],
        cgs,
    )]));
    let pm = Arc::new(PluginManager::load(&stub).expect("plugin load"));

    let config = ExecutionConfig {
        base_url: Some("https://pokeapi.co".into()),
        ..ExecutionConfig::default()
    };
    let engine = ExecutionEngine::new(config).expect("engine");
    let st = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: reg,
        catalog_bootstrap: CatalogBootstrap::Fixed,
        plugin_manager: Some(pm),
        incoming_auth: None,
        run_artifacts: std::sync::Arc::new(plasm_agent::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    });

    let app = discovery_execute_router(st);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(80)).await;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(45))
        .build()
        .unwrap();

    let base = format!("http://{}", addr);
    let r = client
        .post(format!("{base}/execute"))
        .json(&serde_json::json!({
            "entry_id": "pokeapi",
            "entities": ["Pokemon"]
        }))
        .send()
        .await
        .expect("post /execute");
    assert_eq!(r.status(), reqwest::StatusCode::SEE_OTHER);
    let loc = r
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("Location");
    let path = loc.to_str().unwrap();

    let session = client
        .get(format!("{base}{path}"))
        .send()
        .await
        .expect("get session");
    assert!(
        session.status().is_success(),
        "session {}",
        session.status()
    );
    let v: serde_json::Value = session.json().await.unwrap();
    let ph = v["prompt_hash"].as_str().unwrap();
    let sid = v["session"].as_str().unwrap();

    let run = client
        .post(format!("{base}/execute/{ph}/{sid}"))
        .header("Accept", "application/json")
        .header("Content-Type", "text/plain")
        .body("Pokemon query --limit 1")
        .send()
        .await
        .expect("run expression");
    assert!(run.status().is_success(), "run {}", run.status());
    let rows: serde_json::Value = run.json().await.unwrap();
    let first = rows
        .as_array()
        .and_then(|a| a.first())
        .expect("at least one row");
    assert_eq!(first["name"], "bulbasaur");

    serve.abort();
}
