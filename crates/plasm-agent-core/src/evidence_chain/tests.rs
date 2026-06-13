use super::config::{
    evidence_chain_enabled, ENV_PLASM_EVIDENCE_CHAIN, ENV_PLASM_EVIDENCE_SIGNING_KEY,
};
use super::error::EvidenceEmitError;
use super::plan::chain;
use super::records::{RunSealRecord, StepExecutedRecord};
use super::session::EvidenceChainSession;
use crate::execute_session::ExecuteSession;
use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{Expr, Value};
use plasm_evidence::EvidenceScope;
use std::sync::Mutex;

static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

fn env_test_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn session_slot_unallocated_when_disabled() {
    let _guard = env_test_guard();
    std::env::remove_var(ENV_PLASM_EVIDENCE_CHAIN);
    let chain = EvidenceChainSession::new();
    chain
        .record_step_executed(
            "x",
            0,
            None,
            "line",
            &ParsedExpr {
                expr: Expr::TeachingValue {
                    value: Value::String("x".into()),
                },
                projection: None,
            },
            &[],
        )
        .expect("noop");
    assert!(!evidence_chain_enabled());
}

#[test]
fn execute_session_chain_slot_lazy() {
    let _guard = env_test_guard();
    std::env::set_var(ENV_PLASM_EVIDENCE_CHAIN, "1");
    let es = minimal_execute_session();
    assert!(es.evidence_chain.lock().unwrap().is_none());
    assert!(chain(&es).is_some());
    assert!(es.evidence_chain.lock().unwrap().is_some());
}

#[test]
fn record_step_executed_accepts_synthetic_fingerprint() {
    let _guard = env_test_guard();
    std::env::set_var(ENV_PLASM_EVIDENCE_CHAIN, "1");
    let chain = EvidenceChainSession::new();
    chain
        .reset_scope(EvidenceScope::new_v1(
            "p".repeat(64),
            "sess",
            "c".repeat(64),
            0,
            "demo",
        ))
        .expect("scope");
    chain
        .record_step_executed(
            "compute_step",
            0,
            None,
            "plan.compute(compute_step)",
            &ParsedExpr {
                expr: Expr::TeachingValue {
                    value: Value::String("x".into()),
                },
                projection: None,
            },
            &["plan-compute:deadbeef".into()],
        )
        .expect("synthetic fingerprint accepted");
}

#[test]
fn batch_record_steps_single_lock() {
    let _guard = env_test_guard();
    std::env::set_var(ENV_PLASM_EVIDENCE_CHAIN, "1");
    let chain = EvidenceChainSession::new();
    chain
        .reset_scope(EvidenceScope::new_v1(
            "p".repeat(64),
            "sess",
            "c".repeat(64),
            0,
            "demo",
        ))
        .expect("scope");
    chain.record_comp_committed(&minimal_comp()).expect("comp");
    let parsed = ParsedExpr {
        expr: Expr::TeachingValue {
            value: Value::String("x".into()),
        },
        projection: None,
    };
    chain
        .record_steps_executed(&[
            StepExecutedRecord {
                step_id: "a".into(),
                step_index: 0,
                entry_id: None,
                source_line: "plan.compute(a)".into(),
                parsed: parsed.clone(),
                request_fingerprints: vec!["fp1".into()],
            },
            StepExecutedRecord {
                step_id: "x".into(),
                step_index: 1,
                entry_id: None,
                source_line: "LangItem".into(),
                parsed,
                request_fingerprints: vec![],
            },
        ])
        .expect("batch");
    let bundle = chain.finish_bundle().expect("finish").expect("some");
    assert_eq!(bundle.chain.segments.len(), 3);
}

#[test]
fn record_run_sealed_rejects_run_id_wire_mismatch() {
    let _guard = env_test_guard();
    std::env::set_var(ENV_PLASM_EVIDENCE_CHAIN, "1");
    let chain = EvidenceChainSession::new();
    chain
        .reset_scope(EvidenceScope::new_v1(
            "p".repeat(64),
            "sess",
            "c".repeat(64),
            0,
            "demo",
        ))
        .expect("scope");
    let parsed = ParsedExpr {
        expr: Expr::TeachingValue {
            value: Value::String("x".into()),
        },
        projection: None,
    };
    let err = chain
        .record_run_sealed(&RunSealRecord {
            expected_run_id_wire: format!("{}00", "pr".to_string() + &"ab".repeat(32)),
            step_id: None,
            resource_index: None,
            entry_id: "demo".into(),
            source_line: "LangItem".into(),
            parsed,
            request_fingerprints: vec![],
        })
        .expect_err("wire mismatch");
    assert!(matches!(err, EvidenceEmitError::RunBundleDigest(_)));
}

#[test]
fn finish_bundle_rejects_invalid_signing_key() {
    let _guard = env_test_guard();
    std::env::set_var(ENV_PLASM_EVIDENCE_CHAIN, "1");
    std::env::set_var(ENV_PLASM_EVIDENCE_SIGNING_KEY, "not-a-valid-seed");
    let chain = EvidenceChainSession::new();
    chain
        .reset_scope(EvidenceScope::new_v1(
            "p".repeat(64),
            "sess",
            "c".repeat(64),
            0,
            "demo",
        ))
        .expect("scope");
    chain.record_comp_committed(&minimal_comp()).expect("comp");
    let err = chain.finish_bundle().expect_err("invalid key");
    assert!(matches!(err, EvidenceEmitError::SigningKeyInvalid(_)));
    std::env::remove_var(ENV_PLASM_EVIDENCE_SIGNING_KEY);
}

fn minimal_execute_session() -> ExecuteSession {
    use plasm_core::{load_schema, CgsContext, TeachingExposureSession};
    use std::path::PathBuf;
    use std::sync::Arc;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(
        load_schema(&root.join("tests/fixtures/execute_tiny")).expect("load execute_tiny"),
    );
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "acme".into(),
        Arc::new(CgsContext::entry("acme", cgs.clone())),
    );
    let exp = TeachingExposureSession::new(cgs.as_ref(), "acme", &["Product", "Category"]);
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "acme".into(),
        String::new(),
        String::new(),
        None,
        vec!["Product".into(), "Category".into()],
        Some(exp),
        None,
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

fn minimal_comp() -> plasm_core::PlasmComp {
    use plasm_core::plasm_monad::*;
    use std::collections::BTreeMap;
    let mut steps = BTreeMap::new();
    steps.insert(
        "a".into(),
        PlasmStepPayload::Pure(PurePayload {
            data: plasm_core::PlasmDataValue::Literal {
                value: serde_json::json!("a"),
            },
            effect_class: plasm_core::EffectClass::ArtifactRead,
            result_shape: plasm_core::ResultShape::Artifact,
        }),
    );
    steps.insert(
        "x".into(),
        PlasmStepPayload::Pure(PurePayload {
            data: plasm_core::PlasmDataValue::Literal {
                value: serde_json::json!("hi"),
            },
            effect_class: plasm_core::EffectClass::ArtifactRead,
            result_shape: plasm_core::ResultShape::Artifact,
        }),
    );
    let bind = PlasmBindGraph {
        topo: vec![StepId::new("a").unwrap(), StepId::new("x").unwrap()],
        deps: BTreeMap::new(),
        primary: BTreeMap::new(),
        holes: BTreeMap::new(),
    };
    plasm_core::PlasmComp {
        version: 1,
        name: None,
        steps,
        bind,
        return_: PlasmReturn::Step {
            step: StepId::new("x").unwrap(),
        },
        metadata: BTreeMap::new(),
    }
}
