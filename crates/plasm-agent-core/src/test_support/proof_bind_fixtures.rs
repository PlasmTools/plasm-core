//! Proof bind + durable execute-session fixtures for integration tests.

use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema;
use plasm_core::{CgsContext, Expr, InvokeExpr, TeachingExposureSession, Value, CGS};
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

use crate::execute_session::{ExecuteSession, SessionBindCredentialsSnapshot, SessionReuseKey};
use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
use crate::mcp_transport_store::ExecuteSessionRegistry;
use crate::run_artifacts::RunArtifactStore;
use crate::server_state::{CatalogBootstrap, PlasmHostState};

pub struct ProofBindFixture {
    pub cgs: Arc<CGS>,
    pub session: ExecuteSession,
    pub reuse_key: SessionReuseKey,
    pub registry: ExecuteSessionRegistry,
}

impl ProofBindFixture {
    pub fn open(session_id_label: &str) -> Self {
        let proof_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
        let mut cgs = load_schema(&proof_dir).expect("proof catalog");
        cgs.http_backend = "http://127.0.0.1:9".into();
        let cgs = Arc::new(cgs);
        let mut contexts = IndexMap::new();
        contexts.insert(
            "proof".into(),
            Arc::new(CgsContext::entry("proof", cgs.clone())),
        );
        let exposure = TeachingExposureSession::new(cgs.as_ref(), "proof", &["Document"]);
        let session = ExecuteSession::new(
            format!("ph_{session_id_label}"),
            "proof bind integration".into(),
            cgs.clone(),
            contexts,
            "proof".into(),
            String::new(),
            String::new(),
            Some("http://127.0.0.1:9".into()),
            vec!["Document".into()],
            Some(exposure),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let reuse_key = SessionReuseKey {
            tenant_scope: String::new(),
            entry_id: "proof".into(),
            catalog_cgs_hash: session.catalog_cgs_hash.clone(),
            entities: session.entities.clone(),
            context_intent: None,
            ranked_capabilities: None,
            principal: None,
            logical_session_id: None,
        };
        let (registry, _) = ExecuteSessionRegistry::with_test_json_store();
        Self {
            cgs,
            session,
            reuse_key,
            registry,
        }
    }

    pub fn token_only_bind_expr(&self) -> Expr {
        Expr::Invoke(InvokeExpr::new(
            "document_share_bind",
            "Document",
            "my-doc",
            Some(Value::Object(IndexMap::from([(
                "share_token".into(),
                Value::String("secret-tok".into()),
            )]))),
        ))
    }

    pub fn host_with_registry(&self) -> PlasmHostState {
        let reg = InMemoryCgsRegistry::from_pairs(vec![(
            "proof".into(),
            "Proof".into(),
            vec!["Document".into()],
            self.cgs.clone(),
        )]);
        let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
        let mut host = build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry: Arc::new(reg),
            catalog_bootstrap: CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        });
        host.oss.execute_session_registry = self.registry.clone();
        host
    }
}

pub async fn rehydrate_proof_session(
    host: &PlasmHostState,
    fixture: &ProofBindFixture,
    session_id: &str,
) -> ExecuteSession {
    let desc = fixture
        .registry
        .load(fixture.session.prompt_hash.as_str(), session_id)
        .await
        .expect("durable descriptor");
    crate::execute_session_rehydrate::rehydrate_execute_session(host, &desc)
        .await
        .expect("rehydrate")
}

pub async fn merge_durable_credentials_into_hot(
    registry: &ExecuteSessionRegistry,
    hot: &ExecuteSession,
    prompt_hash: &str,
    session_id: &str,
) {
    registry
        .merge_into_live_session(hot, prompt_hash, session_id)
        .await;
}

pub fn credential_snapshot(
    share: Option<&str>,
    base: Option<&str>,
) -> SessionBindCredentialsSnapshot {
    SessionBindCredentialsSnapshot {
        session_share_token: share.map(str::to_string),
        session_proof_base_token: base.map(str::to_string),
    }
}
