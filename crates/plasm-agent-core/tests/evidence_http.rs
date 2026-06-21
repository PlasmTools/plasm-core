//! HTTP evidence sidecar serve + verification.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Extension;
use axum::Router;
use plasm_agent_core::evidence_chain::verify_evidence_for_http_serve;
use plasm_agent_core::http::{build_plasm_host_state, PlasmHostBootstrap};
use plasm_agent_core::http_execute::execute_routes;
use plasm_agent_core::incoming_auth::IncomingPrincipal;
use plasm_agent_core::run_artifacts::{RunArtifactId, RunArtifactStore};
use plasm_agent_core::server_state::CatalogBootstrap;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_evidence::{
    ChainBuilder, DefaultChainVerifier, EvidenceAnchors, EvidenceBundle, EvidenceKind,
    EvidenceScope, IntentDigest, VerifyOptions,
};
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt;

fn test_app(store: Arc<RunArtifactStore>) -> Router<()> {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = Arc::new(load_schema_dir(&dir).expect("matrix"));
    let reg = InMemoryCgsRegistry::from_pairs(vec![(
        "langmatrix".into(),
        "Lang Matrix".into(),
        vec!["matrix".into()],
        cgs,
    )]);
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    let st = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(reg),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: store,
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    });
    execute_routes()
        .layer(Extension(st))
        .layer(Extension(IncomingPrincipal(None)))
}

fn sample_bundle(ph: &str, sid: &str) -> EvidenceBundle {
    use plasm_evidence::SegmentDigest;
    let mut b = ChainBuilder::new();
    b.push(
        EvidenceKind::IntentBound {
            intent_digest: IntentDigest::from_bytes([1u8; 32]),
            intent_len: 4,
        },
        None,
    )
    .expect("push");
    b.push(
        EvidenceKind::CompCommitted {
            plan_commit_id_hex: "ab".repeat(64),
            comp_semantic_sha256: SegmentDigest::from_bytes([2u8; 32]),
            step_topo: vec![],
        },
        None,
    )
    .expect("push");
    EvidenceBundle {
        scope: EvidenceScope::new_v1(ph.to_string(), sid, "c".repeat(64), 0, "demo"),
        chain: b.finish(),
        anchors: EvidenceAnchors::default(),
        signature: None,
    }
}

#[tokio::test]
async fn get_evidence_returns_stored_bundle() {
    let store = Arc::new(RunArtifactStore::memory());
    let app = test_app(store.clone());
    let ph = "a".repeat(64);
    let sid = "b".repeat(32);
    let run_id = RunArtifactId::from_wire(&format!("pr{}", "cd".repeat(32))).expect("run id");
    let bundle = sample_bundle(&ph, &sid);
    store
        .insert_evidence_bundle(&ph, &sid, run_id, &bundle)
        .await
        .expect("insert");
    let uri = format!(
        "/execute/{ph}/{sid}/artifacts/{}/evidence",
        run_id.to_wire()
    );
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[test]
fn verify_evidence_for_http_serve_rejects_tampered_chain() {
    let ph = "a".repeat(64);
    let sid = "b".repeat(32);
    let mut bundle = sample_bundle(&ph, &sid);
    DefaultChainVerifier::verify(&bundle).expect("valid");
    bundle.chain.segments[0].prev = Some(plasm_evidence::SegmentDigest::from_bytes([9u8; 32]));
    let err =
        verify_evidence_for_http_serve(&bundle, &VerifyOptions::default(), "pr00", None, None)
            .expect_err("tampered");
    assert!(err.to_string().contains("prev") || err.to_string().contains("head"));
}
