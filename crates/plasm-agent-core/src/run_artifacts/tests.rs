use super::gc::object_store_path_is_run_snapshot_gc_eligible;
use super::*;
use crate::mcp_logical_ref::parse_logical_session_wire_ref;
use axum::body::Bytes;
use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{Expr, Value};
use plasm_runtime::{ExecutionSource, ExecutionStats};
use std::str::FromStr;
use std::sync::Mutex;

/// `init_from_env` reads process env; serialize tests that mutate it.
static PLASM_RUN_ARTIFACTS_ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Restores `PLASM_RUN_ARTIFACTS_{URL,DIR}` after test (even on panic).
struct RestorePlasmRunArtifactEnv {
    had_url: Option<String>,
    had_dir: Option<String>,
}

impl Drop for RestorePlasmRunArtifactEnv {
    fn drop(&mut self) {
        match &self.had_url {
            Some(s) => std::env::set_var("PLASM_RUN_ARTIFACTS_URL", s),
            None => std::env::remove_var("PLASM_RUN_ARTIFACTS_URL"),
        }
        match &self.had_dir {
            Some(s) => std::env::set_var("PLASM_RUN_ARTIFACTS_DIR", s),
            None => std::env::remove_var("PLASM_RUN_ARTIFACTS_DIR"),
        }
    }
}

fn sample_run_id() -> RunArtifactId {
    RunArtifactId::from_bytes([0xab; 32])
}

#[test]
fn parse_plasm_run_uri_round_trip() {
    let id = sample_run_id();
    let ph64 = "ab".repeat(32);
    let uri = plasm_run_resource_uri(&ph64, "sess01", &id);
    let (ph, sid, rid) = parse_plasm_execute_run_uri(&uri).expect("parse");
    assert_eq!(ph, "ab".repeat(32));
    assert_eq!(sid, "sess01");
    assert_eq!(rid, id);
}

#[test]
fn run_artifact_wire_rejects_uuid_shape() {
    assert!(RunArtifactWire::from_str("550e8400-e29b-41d4-a716-446655440000").is_err());
}

#[test]
fn run_artifact_wire_accepts_uppercase_hex() {
    let lower = sample_run_id().to_wire();
    let upper_hex: String = lower
        .strip_prefix(RUN_ARTIFACT_WIRE_PREFIX)
        .expect("prefix")
        .to_ascii_uppercase();
    let mixed = format!("{RUN_ARTIFACT_WIRE_PREFIX}{upper_hex}");
    let w = RunArtifactWire::from_str(&mixed).expect("parse uppercase hex");
    assert_eq!(w.0, sample_run_id());
}

#[test]
fn plan_bundle_digest_stable_and_fingerprint_sensitive() {
    use plasm_core::{Expr, Value};
    let p = ParsedExpr {
        expr: Expr::TeachingValue {
            value: Value::String("probe".into()),
        },
        projection: None,
    };
    let a = RunArtifactId::from_plan_bundle_inputs(
        "h",
        0,
        "e",
        " line ",
        &p,
        &["b".into(), "a".into()],
    )
    .expect("digest");
    let b =
        RunArtifactId::from_plan_bundle_inputs("h", 0, "e", "line", &p, &["a".into(), "b".into()])
            .expect("digest");
    assert_eq!(a, b, "fingerprints are sorted in the bundle");
    let c = RunArtifactId::from_plan_bundle_inputs("h", 0, "e", "line", &p, &["z".into()])
        .expect("digest");
    assert_ne!(a, c);
}

fn sample_parsed_preimage() -> ParsedExpr {
    ParsedExpr {
        expr: Expr::TeachingValue {
            value: Value::String("probe".into()),
        },
        projection: None,
    }
}

#[tokio::test]
async fn memory_insert_get_round_trip() {
    let store = RunArtifactStore::memory();
    let run_id = sample_run_id();
    let doc = RunArtifactDocument {
        run_id: run_id.to_wire(),
        prompt_hash: "p".repeat(64),
        session_id: "s1".into(),
        entry_id: "e".into(),
        resource_index: Some(1),
        principal: None,
        parsed_preimage: sample_parsed_preimage(),
        display_lines: vec![],
        request_fingerprints: vec![],
        entities: vec![],
        source: ExecutionSource::Live,
        stats: ExecutionStats {
            duration_ms: 0,
            network_requests: 0,
            cache_hits: 0,
            cache_misses: 0,
            ..Default::default()
        },
    };
    let n = store
        .insert(&"p".repeat(64), "s1", run_id, &doc)
        .await
        .expect("insert");
    assert!(n > 0);
    let bytes = store.get(&"p".repeat(64), "s1", run_id).await.expect("get");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(v["run_id"], run_id.to_wire());
}

#[tokio::test]
async fn memory_insert_get_payload_round_trip_binary() {
    let store = RunArtifactStore::memory();
    let run_id = sample_run_id();
    let payload = ArtifactPayload {
        metadata: ArtifactPayloadMetadata {
            content_type: "application/x-plasm-test".into(),
            content_encoding: Some("identity".into()),
            schema_version: 7,
            producer: "unit-test".into(),
        },
        bytes: Bytes::from_static(&[0, 1, 2, 3, 254, 255]),
    };
    store
        .insert_payload(&"p".repeat(64), "s1", run_id, Some(7), &payload)
        .await
        .expect("insert");
    let got = store
        .get_payload(&"p".repeat(64), "s1", run_id)
        .await
        .expect("payload");
    assert_eq!(got, payload);
    let by_idx = store
        .get_payload_result_by_resource_index(&"p".repeat(64), "s1", 7)
        .await
        .expect("by index")
        .expect("some");
    assert_eq!(by_idx, payload);
}

#[test]
fn parse_short_plasm_resource_uri() {
    assert_eq!(parse_plasm_short_resource_uri("plasm://r/42"), Some(42));
    assert_eq!(parse_plasm_short_resource_uri("plasm://r/0"), Some(0));
    assert!(parse_plasm_short_resource_uri("plasm://r/").is_none());
    assert!(parse_plasm_short_resource_uri("plasm://r/x").is_none());
    assert!(parse_plasm_short_resource_uri("plasm://execute/a/b/run/u").is_none());
}

#[test]
fn parse_logical_short_plasm_resource_uri_round_trip() {
    let wire = "l_AAAAAAAAQACAAAAAAAAAAQ";
    let u = parse_logical_session_wire_ref(wire)
        .expect("parse")
        .as_uuid();
    let u2 = plasm_short_resource_uri_logical(&u, 7);
    assert_eq!(u2, format!("plasm://session/{wire}/r/7"));
    assert_eq!(
        parse_plasm_session_short_resource_uri(&u2),
        Some((LogicalSessionUriSegment::WireRef(wire.into()), 7))
    );
    assert!(parse_plasm_session_short_resource_uri("plasm://session/s3/r/1").is_none());
    assert!(parse_plasm_session_short_resource_uri("plasm://session/not-uuid/r/1").is_none());
}

#[test]
fn parse_code_plan_handles_and_uris() {
    let wire = "l_AAAAAAAAQACAAAAAAAAAAQ";
    let id = Uuid::from_u128(1);
    assert_eq!(code_plan_handle(3), "p3");
    assert_eq!(parse_code_plan_handle("p3"), Some(3));
    assert!(parse_code_plan_handle("r3").is_none());
    let short = plasm_session_short_plan_uri(wire, 3);
    assert_eq!(
        parse_plasm_session_short_plan_uri(&short),
        Some((LogicalSessionUriSegment::WireRef(wire.into()), 3))
    );
    let canonical = plasm_code_plan_resource_uri(&"a".repeat(64), "sess", &id);
    assert_eq!(
        parse_plasm_execute_plan_uri(&canonical),
        Some(("a".repeat(64), "sess".into(), id))
    );
}

#[tokio::test]
async fn evidence_sidecar_memory_round_trip() {
    use plasm_evidence::{
        ChainBuilder, EvidenceAnchors, EvidenceBundle, EvidenceKind, EvidenceScope, IntentDigest,
    };
    let store = RunArtifactStore::memory();
    let ph = "p".repeat(64);
    let run_id = RunArtifactId::from_bytes([9u8; 32]);
    let mut b = ChainBuilder::new();
    b.push(
        EvidenceKind::IntentBound {
            intent_digest: IntentDigest::from_bytes([1u8; 32]),
            intent_len: 3,
        },
        None,
    )
    .expect("push");
    let bundle = EvidenceBundle {
        scope: EvidenceScope::new_v1(ph.clone(), "s1", "c".repeat(64), 0, "demo"),
        chain: b.finish(),
        anchors: EvidenceAnchors::default(),
        signature: None,
    };
    store
        .insert_evidence_bundle(&ph, "s1", run_id, &bundle)
        .await
        .expect("insert");
    let got = store
        .get_evidence_bundle(&ph, "s1", run_id)
        .await
        .expect("get")
        .expect("some");
    assert_eq!(got.chain.segments.len(), 1);
    plasm_evidence::DefaultChainVerifier::verify(&got).expect("verify");
}

#[tokio::test]
async fn evidence_sidecar_dedup_multi_run_id() {
    use plasm_evidence::{
        ChainBuilder, EvidenceAnchors, EvidenceBundle, EvidenceKind, EvidenceScope, IntentDigest,
    };
    let store = RunArtifactStore::memory();
    let ph = "p".repeat(64);
    let run_a = RunArtifactId::from_bytes([9u8; 32]);
    let run_b = RunArtifactId::from_bytes([8u8; 32]);
    let mut b = ChainBuilder::new();
    b.push(
        EvidenceKind::IntentBound {
            intent_digest: IntentDigest::from_bytes([1u8; 32]),
            intent_len: 3,
        },
        None,
    )
    .expect("push");
    let bundle = EvidenceBundle {
        scope: EvidenceScope::new_v1(ph.clone(), "s1", "c".repeat(64), 0, "demo"),
        chain: b.finish(),
        anchors: EvidenceAnchors::default(),
        signature: None,
    };
    store
        .insert_evidence_bundles(&ph, "s1", &[run_a, run_b], &bundle)
        .await
        .expect("insert");
    let got_a = store
        .get_evidence_bundle(&ph, "s1", run_a)
        .await
        .expect("get a")
        .expect("some a");
    let got_b = store
        .get_evidence_bundle(&ph, "s1", run_b)
        .await
        .expect("get b")
        .expect("some b");
    assert_eq!(got_a.chain.head, got_b.chain.head);
    plasm_evidence::DefaultChainVerifier::verify(&got_a).expect("verify chain");
}

#[test]
fn object_store_gc_skips_evidence_paths() {
    assert!(object_store_path_is_run_snapshot_gc_eligible(
        "execute/abc/s1/prdeadbeef.json"
    ));
    assert!(!object_store_path_is_run_snapshot_gc_eligible(
        "execute/abc/s1/evidence/heads/feedface.evidence.json"
    ));
    assert!(!object_store_path_is_run_snapshot_gc_eligible(
        "execute/abc/s1/evidence/run-heads/prabc.head"
    ));
    assert!(!object_store_path_is_run_snapshot_gc_eligible(
        "code-plans/p/s1/p1.json"
    ));
}

#[tokio::test]
async fn memory_code_plan_round_trip_by_index() {
    let store = RunArtifactStore::memory();
    let plan_id = Uuid::new_v4();
    let doc = CodePlanArchiveDocument {
        kind: "code_plan".into(),
        plan_id: plan_id.to_string(),
        prompt_hash: "p".repeat(64),
        session_id: "s1".into(),
        entry_id: "demo".into(),
        plan_index: 1,
        plan_handle: "p1".into(),
        name: "demo plan".into(),
        code: "JSON.stringify({version:1,nodes:[]})".into(),
        plan_hash: "h".repeat(64),
        comp: serde_json::json!({"version": 1, "steps": {}, "bind": {"topo": []}, "return": {"kind": "step", "step": "x"}}),
        catalog_cgs_hash: "c".repeat(64),
        domain_revision: 0,
        entities: vec!["Widget".into()],
        principal: None,
        created_at: "2026-01-01T00:00:00Z".into(),
    };
    store
        .insert_code_plan(&"p".repeat(64), "s1", plan_id, 1, &doc)
        .await
        .expect("insert");
    let payload = store
        .get_code_plan_payload_result_by_index(&"p".repeat(64), "s1", 1)
        .await
        .expect("decode")
        .expect("payload");
    let got: CodePlanArchiveDocument = serde_json::from_slice(payload.bytes.as_ref()).expect("doc");
    assert_eq!(got.plan_id, plan_id.to_string());
    assert_eq!(got.plan_handle, "p1");
}

#[tokio::test]
async fn fs_backend_resource_index_round_trip() {
    let tmp = tempfile::tempdir().expect("tmp");
    let store = RunArtifactStore::from_fs_root_for_test(tmp.path().to_path_buf());
    let ph = "p".repeat(64);
    let run_id = sample_run_id();
    let doc = RunArtifactDocument {
        run_id: run_id.to_wire(),
        prompt_hash: ph.clone(),
        session_id: "s1".into(),
        entry_id: "e".into(),
        resource_index: Some(3),
        principal: None,
        parsed_preimage: sample_parsed_preimage(),
        display_lines: vec![],
        request_fingerprints: vec![],
        entities: vec![],
        source: ExecutionSource::Live,
        stats: ExecutionStats {
            duration_ms: 0,
            network_requests: 0,
            cache_hits: 0,
            cache_misses: 0,
            ..Default::default()
        },
    };
    store.insert(&ph, "s1", run_id, &doc).await.expect("insert");
    let by_idx = store
        .get_payload_result_by_resource_index(&ph, "s1", 3)
        .await
        .expect("by index")
        .expect("some");
    let v: serde_json::Value = serde_json::from_slice(&by_idx.bytes).expect("json");
    assert_eq!(v["run_id"], run_id.to_wire());
}

/// `PLASM_RUN_ARTIFACTS_URL` must win over `PLASM_RUN_ARTIFACTS_DIR` (hosted/SaaS invariant).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn init_from_env_url_precedes_dir() {
    let _lock = PLASM_RUN_ARTIFACTS_ENV_TEST_LOCK
        .lock()
        .expect("env test lock");
    let _restore = RestorePlasmRunArtifactEnv {
        had_url: std::env::var("PLASM_RUN_ARTIFACTS_URL").ok(),
        had_dir: std::env::var("PLASM_RUN_ARTIFACTS_DIR").ok(),
    };
    std::env::remove_var("PLASM_RUN_ARTIFACTS_URL");
    std::env::remove_var("PLASM_RUN_ARTIFACTS_DIR");

    let object_root = tempfile::tempdir().expect("url root");
    let decoy_fs_root = tempfile::tempdir().expect("decoy dir — must not be used for blobs");

    let file_url = url::Url::from_directory_path(object_root.path())
        .expect("file: URL for run artifact prefix")
        .to_string();
    std::env::set_var("PLASM_RUN_ARTIFACTS_URL", &file_url);
    std::env::set_var(
        "PLASM_RUN_ARTIFACTS_DIR",
        decoy_fs_root.path().to_string_lossy().as_ref(),
    );

    let store: Arc<RunArtifactStore> = init_from_env().expect("init_from_env");

    let ph = "c".repeat(64);
    let run_id = sample_run_id();
    let doc = RunArtifactDocument {
        run_id: run_id.to_wire(),
        prompt_hash: ph.clone(),
        session_id: "sess".into(),
        entry_id: "e".into(),
        resource_index: None,
        principal: None,
        parsed_preimage: sample_parsed_preimage(),
        display_lines: vec![],
        request_fingerprints: vec![],
        entities: vec![],
        source: ExecutionSource::Live,
        stats: ExecutionStats {
            duration_ms: 0,
            network_requests: 0,
            cache_hits: 0,
            cache_misses: 0,
            ..Default::default()
        },
    };
    store
        .insert(&ph, "sess", run_id, &doc)
        .await
        .expect("insert with object store backend");

    assert!(
        !decoy_fs_root.path().join("execute").exists(),
        "If PLASM_RUN_ARTIFACTS_DIR were selected, execute/ would appear under the decoy path"
    );
    assert!(
        object_root.path().join("execute").exists(),
        "Object-store backend (file: URL) should place blobs under the URL path + execute/"
    );
}
