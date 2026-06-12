//! Hermit-backed evidence chain: plan dry/live with `PLASM_EVIDENCE_CHAIN=1`.

#[path = "common/hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

#[path = "common/language_matrix.rs"]
mod language_matrix;

use plasm_agent::plasm_compile::compile_plasm_expression;
use plasm_agent::plasm_plan_run::{evaluate_plasm_comp_dry, run_plasm_comp};
use plasm_agent::evidence_chain::{begin_plan_evidence, evidence_chain_enabled};
use plasm_core::PromptPipelineConfig;
use plasm_agent::plasm_plan_run::parse_parsed_expr_for_session;
use plasm_evidence::{DefaultChainVerifier, VerifyOptions};
use plasm_runtime::{ExecutionConfig, ExecutionEngine};

#[test]
fn evidence_chain_plan_run_round_trip() {
    std::env::set_var("PLASM_EVIDENCE_CHAIN", "1");
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            assert!(evidence_chain_enabled());
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(evidence_chain_plan_run_round_trip_async());
        })
        .expect("spawn evidence e2e thread")
        .join()
        .expect("join");
}

async fn evidence_chain_plan_run_round_trip_async() {
    let base = hermit_lang_matrix::language_matrix_hermit_base_url().await.clone();
    let cgs = language_matrix::load_language_matrix_cgs();
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base.clone()),
        ..Default::default()
    })
    .expect("engine");
    let st = language_matrix::matrix_host_state(engine, cgs.clone());
    let es = language_matrix::matrix_execute_session(cgs);

    let program = "LangItem";
    let bundle = compile_plasm_expression(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "evidence_e2e",
        program,
    )
    .expect("compile");

    begin_plan_evidence(&es, "evidence_sess").expect("begin evidence");
    let dry = evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
    assert!(!dry.node_results.is_empty());

    let live = run_plasm_comp(
        &es,
        &st,
        es.prompt_hash.as_str(),
        "evidence_sess",
        &bundle,
        true,
        None,
        None,
    )
    .await
    .expect("live run");

    let head = live
        .run_plasm_meta
        .as_ref()
        .and_then(|m| m.get("evidence_chain_head"))
        .and_then(|v| v.as_str())
        .expect("evidence_chain_head in _meta.plasm");
    assert_eq!(head.len(), 64);

    let run_id = live
        .code_plan_run_artifacts
        .first()
        .map(|a| a.run_id.as_str())
        .expect("run artifact ref");
    let rid = plasm_agent::run_artifacts::RunArtifactId::from_wire(run_id).expect("run_id wire");

    let bundle_json = st
        .run_artifacts
        .get_evidence_bundle(es.prompt_hash.as_str(), "evidence_sess", rid)
        .await
        .expect("get evidence")
        .expect("evidence sidecar stored");

    DefaultChainVerifier::verify_bundle_for_serve(&bundle_json, &VerifyOptions::default())
        .expect("verify serve");
    DefaultChainVerifier::verify_step_executed_topo(&bundle_json).expect("verify topo");
    DefaultChainVerifier::verify_comp_commit_id(
        &bundle_json,
        &plasm_agent::operation::compute_plan_commit_id_from_dry(&dry).to_string(),
    )
    .expect("comp commit matches dry");

    let artifact_bytes = st
        .run_artifacts
        .get(es.prompt_hash.as_str(), "evidence_sess", rid)
        .await
        .expect("artifact bytes");
    let artifact_doc: plasm_evidence::RunArtifactForSeal =
        serde_json::from_slice(&artifact_bytes).expect("artifact json");
    let line = artifact_doc
        .expressions
        .first()
        .expect("artifact expression");
    let parsed = parse_parsed_expr_for_session(&es, line.trim()).expect("parse session line");
    let source_line = artifact_doc.source_line();
    let inputs = plasm_evidence::run_seal_inputs_from_artifact(
        &bundle_json.scope,
        &artifact_doc,
        &source_line,
        &parsed,
    );
    DefaultChainVerifier::verify_run_seal_with_inputs(&bundle_json, run_id, &inputs)
        .expect("run seal digest");
}

#[test]
fn evidence_chain_signed_bundle_round_trip() {
    std::env::set_var("PLASM_EVIDENCE_CHAIN", "1");
    std::env::set_var(
        "PLASM_EVIDENCE_SIGNING_KEY",
        "0101010101010101010101010101010101010101010101010101010101010101",
    );
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(evidence_chain_signed_round_trip_async());
        })
        .expect("spawn signed evidence e2e thread")
        .join()
        .expect("join");
    std::env::remove_var("PLASM_EVIDENCE_SIGNING_KEY");
}

async fn evidence_chain_signed_round_trip_async() {
    let base = hermit_lang_matrix::language_matrix_hermit_base_url().await.clone();
    let cgs = language_matrix::load_language_matrix_cgs();
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base.clone()),
        ..Default::default()
    })
    .expect("engine");
    let st = language_matrix::matrix_host_state(engine, cgs.clone());
    let es = language_matrix::matrix_execute_session(cgs);

    let bundle = compile_plasm_expression(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "evidence_signed_e2e",
        "LangItem",
    )
    .expect("compile");

    begin_plan_evidence(&es, "evidence_signed_sess").expect("begin evidence");
    let _dry = evaluate_plasm_comp_dry(&es, &bundle).expect("dry");

    let live = run_plasm_comp(
        &es,
        &st,
        es.prompt_hash.as_str(),
        "evidence_signed_sess",
        &bundle,
        true,
        None,
        None,
    )
    .await
    .expect("live run");

    let run_id = live
        .code_plan_run_artifacts
        .first()
        .map(|a| a.run_id.as_str())
        .expect("run artifact ref");
    let rid = plasm_agent::run_artifacts::RunArtifactId::from_wire(run_id).expect("run_id wire");

    let bundle_json = st
        .run_artifacts
        .get_evidence_bundle(es.prompt_hash.as_str(), "evidence_signed_sess", rid)
        .await
        .expect("get evidence")
        .expect("evidence sidecar stored");

    let sig = bundle_json
        .signature
        .as_ref()
        .expect("signed bundle");
    let pk = sig.public_key_hex.clone();
    let opts = VerifyOptions::from_trusted_public_keys(vec![pk.clone()]);
    DefaultChainVerifier::verify_bundle_for_serve(&bundle_json, &opts).expect("verify signed serve");
    assert!(
        plasm_evidence::sign::verify_bundle_signature_trusted(&bundle_json, sig, &[pk.clone()])
            .is_ok()
    );
    assert!(
        plasm_evidence::sign::verify_bundle_signature_trusted(
            &bundle_json,
            sig,
            &["00".repeat(32)]
        )
        .is_err()
    );
}
