//! Hermit instance for `plasm_language_matrix` OpenAPI (compiled only by `plasm_language_matrix` integration test).

use tokio::sync::OnceCell;

static LANGUAGE_MATRIX_HERMIT: OnceCell<String> = OnceCell::const_new();

fn language_matrix_spec_path() -> std::path::PathBuf {
    let crate_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        crate_root.join("../../fixtures/real_openapi_specs/plasm_language_matrix.yaml"),
        crate_root.join("fixtures/real_openapi_specs/plasm_language_matrix.yaml"),
    ];
    for p in &candidates {
        if p.exists() {
            return p.clone();
        }
    }
    panic!(
        "Cannot find plasm_language_matrix.yaml (tried {:?})",
        candidates
    );
}

async fn spawn_hermit_host_root(spec_path: &std::path::Path) -> String {
    // Hermit's schema faker may emit sparse objects; `for_each` templates read row JSON (`_.id`).
    // Prefer declared OpenAPI `example` payloads so list GETs return stable primary keys (i1, i2, …).
    beavuck_hermit::resource_generator::set_use_examples(true);
    let spec = beavuck_hermit::spec_loader::load(spec_path);
    let routes = beavuck_hermit::spec_parser::extract_routes(&spec);
    let router = beavuck_hermit::router::build_with_bounds(routes, 1, 5);

    // Bind the listener on the *server* runtime. Creating it on the caller's runtime and then
    // dropping that runtime (e.g. views `block_on_views_live` harness) orphans the IO driver and
    // kills accept — every later test then sees connection-refused on the cached OnceCell URL.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .build()
            .expect("hermit server runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("hermit bind");
            let addr = listener.local_addr().expect("hermit local_addr");
            let base_url = format!("http://127.0.0.1:{}", addr.port());
            tx.send(base_url).expect("send hermit base url");
            axum::serve(listener, router).await.expect("hermit serve");
        });
    });

    let base_url = rx.recv().expect("hermit base url");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    base_url
}

/// Paths `/language/v1/...` from host root.
pub async fn language_matrix_hermit_base_url() -> &'static String {
    LANGUAGE_MATRIX_HERMIT
        .get_or_init(|| async {
            let spec_path = language_matrix_spec_path();
            spawn_hermit_host_root(spec_path.as_path()).await
        })
        .await
}
