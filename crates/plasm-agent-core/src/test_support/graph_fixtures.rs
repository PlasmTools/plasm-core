//! Shared graph / spill fixtures for unit and Shuttle tests.

use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;
use plasm_core::{CgsContext, Ref, TypedFieldValue, Value, CGS};
use plasm_runtime::{EntityCompleteness, ExecutionConfig, ExecutionEngine, ExecutionMode};

use crate::execute_session::{ExecuteSession, SessionCore};
use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
use crate::run_artifacts::RunArtifactStore;
use crate::server_state::{CatalogBootstrap, PlasmHostState};
use crate::session_graph_persistence::SessionGraphPersistence;
use plasm_core::discovery::InMemoryCgsRegistry;

pub fn load_pokeapi_mini_cgs() -> Arc<CGS> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Arc::new(
        plasm_core::loader::load_schema_dir(&root.join("../../fixtures/schemas/pokeapi_mini"))
            .expect("pokeapi_mini"),
    )
}

pub fn berry_entity(name: &str) -> plasm_runtime::CachedEntity {
    let reference = Ref::new("Berry", name);
    let mut fields = IndexMap::new();
    fields.insert(
        "name".to_string(),
        TypedFieldValue::from(Value::String(name.to_string())),
    );
    plasm_runtime::CachedEntity {
        reference,
        fields,
        relations: IndexMap::new(),
        last_updated: 1,
        version: 1,
        completeness: EntityCompleteness::Complete,
    }
}

pub fn test_execute_session(cgs: Arc<CGS>, prompt_hash: &str) -> ExecuteSession {
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        "default".into(),
        Arc::new(CgsContext::entry("default", cgs.clone())),
    );
    ExecuteSession::new(
        prompt_hash.into(),
        String::new(),
        cgs.clone(),
        ctxs,
        "default".into(),
        String::new(),
        String::new(),
        None,
        vec!["Berry".into()],
        None,
        None,
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

#[derive(Clone)]
pub struct SpillHostFixture {
    pub st: Arc<PlasmHostState>,
    pub persistence: Arc<SessionGraphPersistence>,
    _dir: Arc<tempfile::TempDir>,
}

impl SpillHostFixture {
    pub fn new() -> Self {
        let dir = Arc::new(tempfile::tempdir().expect("tempdir"));
        let store_root = dir.path().join("graph-cache");
        std::fs::create_dir_all(&store_root).expect("mkdir");
        let url = url::Url::from_directory_path(&store_root).expect("file url");
        let (store, prefix) =
            object_store::parse_url_opts(&url, std::env::vars()).expect("object store");
        let persistence = Arc::new(SessionGraphPersistence::new(Arc::from(store), prefix));
        let st = Arc::new(build_plasm_host_state(PlasmHostBootstrap {
            engine: ExecutionEngine::new(ExecutionConfig::default()).expect("engine"),
            mode: ExecutionMode::Live,
            registry: Arc::new(InMemoryCgsRegistry::from_pairs(vec![])),
            catalog_bootstrap: CatalogBootstrap::Fixed,
            plugin_manager: None,
            incoming_auth: None,
            run_artifacts: Arc::new(RunArtifactStore::memory()),
            session_graph_persistence: Some(Arc::clone(&persistence)),
            oss_local_filesystem_defaults: false,
        }));
        Self {
            st,
            persistence,
            _dir: dir,
        }
    }
}

pub async fn spill_one_page(
    fx: &SpillHostFixture,
    prompt_hash: &str,
    session_id: &str,
    page: Vec<plasm_runtime::CachedEntity>,
) {
    let core = SessionCore::new();
    let seq = core.alloc_delta_seq().await.0;
    fx.persistence
        .append_graph_page(prompt_hash, session_id, seq, 0, "Berry", &page, None)
        .await
        .expect("append spill page");
}
