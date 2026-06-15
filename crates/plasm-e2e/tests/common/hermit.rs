//! In-process Hermit mock servers for OpenAPI fixtures.

use std::path::{Path, PathBuf};
use tokio::sync::OnceCell;

static PETSTORE_HERMIT: OnceCell<String> = OnceCell::const_new();
static PETSTORE_HERMIT_HYDRATE: OnceCell<String> = OnceCell::const_new();
static POKEAPI_HERMIT: OnceCell<String> = OnceCell::const_new();
#[allow(dead_code)]
static POKEAPI_HERMIT_GRAPH_SPILL: OnceCell<String> = OnceCell::const_new();

fn fixture_dir_candidates(rel_under_fixtures: &str) -> Vec<PathBuf> {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    vec![
        crate_root.join("../../fixtures").join(rel_under_fixtures),
        crate_root.join("fixtures").join(rel_under_fixtures),
    ]
}

fn find_first_existing_path(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.exists()).cloned()
}

/// Petstore OpenAPI JSON (`/api/v3` prefix in returned base URL).
pub fn petstore_spec_path() -> Option<PathBuf> {
    find_first_existing_path(&fixture_dir_candidates(
        "real_openapi_specs/petstore_api.json",
    ))
}

/// PokéAPI YAML (paths from host root).
pub fn pokeapi_spec_path() -> PathBuf {
    find_first_existing_path(&fixture_dir_candidates("real_openapi_specs/pokeapi.yaml"))
        .expect("Cannot find pokeapi.yaml")
}

async fn spawn_hermit_with_base(
    spec_path: &Path,
    base_suffix: &str,
    min_items: usize,
    max_items: usize,
) -> String {
    let spec = beavuck_hermit::spec_loader::load(spec_path);
    let routes = beavuck_hermit::spec_parser::extract_routes(&spec);
    let router = beavuck_hermit::router::build_with_bounds(routes, min_items, max_items);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = if base_suffix.is_empty() {
        format!("http://127.0.0.1:{}", addr.port())
    } else {
        format!(
            "http://127.0.0.1:{}/{}",
            addr.port(),
            base_suffix.trim_start_matches('/')
        )
    };

    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    base_url
}

/// Default shared Petstore Hermit (`…/api/v3`).
pub async fn petstore_hermit_base_url() -> &'static String {
    PETSTORE_HERMIT
        .get_or_init(|| async {
            let spec_path = petstore_spec_path().expect("petstore_api.json");
            spawn_hermit_with_base(spec_path.as_path(), "api/v3", 1, 5).await
        })
        .await
}

/// Separate Petstore instance for hydrate-heavy tests.
pub async fn petstore_hermit_hydrate_base_url() -> &'static String {
    PETSTORE_HERMIT_HYDRATE
        .get_or_init(|| async {
            let spec_path = petstore_spec_path().expect("petstore_api.json");
            spawn_hermit_with_base(spec_path.as_path(), "api/v3", 1, 5).await
        })
        .await
}

/// PokéAPI Hermit (host root; paths include `/api/v2/...` in spec — pokeapi.yaml uses `/api/v2` or root?).
pub async fn pokeapi_hermit_base_url() -> &'static String {
    POKEAPI_HERMIT
        .get_or_init(|| async {
            let spec_path = pokeapi_spec_path();
            spawn_hermit_with_base(spec_path.as_path(), "", 1, 5).await
        })
        .await
}

/// PokéAPI Hermit tuned for graph-spill e2e: enough list rows to exceed `PLASM_GRAPH_HOT_MAX_ENTITIES=1`.
#[allow(dead_code)]
pub async fn pokeapi_hermit_graph_spill_base_url() -> &'static String {
    POKEAPI_HERMIT_GRAPH_SPILL
        .get_or_init(|| async {
            let spec_path = pokeapi_spec_path();
            spawn_hermit_with_base(spec_path.as_path(), "", 4, 8).await
        })
        .await
}
