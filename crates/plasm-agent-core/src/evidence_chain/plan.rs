use super::config::evidence_chain_enabled;
use super::error::EvidenceEmitError;
use super::session::EvidenceChainSession;
use crate::execute_session::ExecuteSession;
use plasm_evidence::{EvidenceAnchors, EvidenceBundle, EvidenceScope};
use std::sync::{Arc, Mutex as StdMutex};

/// Lazy evidence chain slot on [`ExecuteSession`] (`None` until first use while enabled).
pub type EvidenceChainSlot = Arc<StdMutex<Option<Arc<EvidenceChainSession>>>>;

pub fn new_evidence_chain_slot() -> EvidenceChainSlot {
    Arc::new(StdMutex::new(None))
}

/// Resolve the session's evidence chain when `PLASM_EVIDENCE_CHAIN=1`, allocating on first use.
pub fn chain(sess: &ExecuteSession) -> Option<Arc<EvidenceChainSession>> {
    if !evidence_chain_enabled() {
        return None;
    }
    let mut guard = sess
        .evidence_chain
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(Arc::new(EvidenceChainSession::new()));
    }
    guard.clone()
}

pub fn evidence_scope_from_session(
    sess: &ExecuteSession,
    execute_session_id: &str,
) -> EvidenceScope {
    let mut scope = EvidenceScope::new_v1(
        sess.prompt_hash.clone(),
        execute_session_id,
        sess.catalog_cgs_hash.clone(),
        sess.domain_revision,
        sess.entry_id.clone(),
    );
    if !sess.tenant_scope.is_empty() {
        scope.tenant_id = sess.tenant_scope.clone();
    }
    scope
}

pub fn evidence_anchors(
    plan_commit_ref: Option<&plasm_core::PlanCommitRef>,
    trace_id: Option<uuid::Uuid>,
    plan_call_index: Option<u64>,
) -> EvidenceAnchors {
    EvidenceAnchors {
        plan_commit_ref: plan_commit_ref.map(|p| p.as_str().to_string()),
        trace_id,
        plan_call_index,
    }
}

/// Reset chain and record intent anchor at the start of a plan dry/live cycle.
pub fn begin_plan_evidence(
    sess: &ExecuteSession,
    execute_session_id: &str,
) -> Result<(), EvidenceEmitError> {
    if !evidence_chain_enabled() {
        return Ok(());
    }
    let Some(chain) = chain(sess) else {
        return Ok(());
    };
    let scope = evidence_scope_from_session(sess, execute_session_id);
    chain.reset_scope(scope)?;
    if let Some(intent) = sess.context_intent.as_deref() {
        chain.record_intent_bound(
            intent,
            sess.catalog_cgs_hash.as_str(),
            sess.entry_id.as_str(),
        )?;
    }
    Ok(())
}

pub fn begin_plan_evidence_with_anchors(
    sess: &ExecuteSession,
    execute_session_id: &str,
    anchors: EvidenceAnchors,
) -> Result<(), EvidenceEmitError> {
    begin_plan_evidence(sess, execute_session_id)?;
    if let Some(chain) = chain(sess) {
        chain.set_anchors(anchors)?;
    }
    Ok(())
}

/// Semantic comp commit hex for archive `plan_hash` (matches `plan_commit_id`).
pub fn semantic_comp_commit_hex(comp: &plasm_core::PlasmComp) -> String {
    plasm_evidence::compute_comp_commit_id(comp)
        .expect("comp semantic canonical always serializes")
        .to_string()
}

pub fn semantic_comp_commit_hex_from_json(comp: &serde_json::Value) -> String {
    if let Ok(c) = serde_json::from_value::<plasm_core::PlasmComp>(comp.clone()) {
        return semantic_comp_commit_hex(&c);
    }
    let subset = crate::operation::plan_commit_canonical_comp_json(comp);
    let canonical_str = serde_json::to_string(&subset).unwrap_or_default();
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(canonical_str.as_bytes()))
}

pub async fn persist_evidence_sidecars(
    store: &crate::run_artifacts::RunArtifactStore,
    prompt_hash: &str,
    session_id: &str,
    run_ids: &[crate::run_artifacts::RunArtifactId],
    bundle: &EvidenceBundle,
) -> Result<(), EvidenceEmitError> {
    store
        .insert_evidence_bundles(prompt_hash, session_id, run_ids, bundle)
        .await
        .map(|_| ())
        .map_err(|e| EvidenceEmitError::Persist(e.to_string()))
}

pub fn attach_evidence_meta(
    mut run_plasm_meta: Option<serde_json::Map<String, serde_json::Value>>,
    prompt_hash: &str,
    session_id: &str,
    chain: &EvidenceChainSession,
    evidence_run_ids: &[crate::run_artifacts::RunArtifactId],
    chain_head_hex: Option<String>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if !chain.is_enabled() {
        return run_plasm_meta;
    }
    let head = chain_head_hex.or_else(|| chain.chain_head_hex())?;
    let mut plasm = run_plasm_meta.take().unwrap_or_default();
    plasm.insert("evidence_chain_head".into(), serde_json::json!(head));
    if let Some(first) = evidence_run_ids.first() {
        plasm.insert(
            "evidence_uri".into(),
            serde_json::json!(crate::run_artifacts::RunArtifactStore::evidence_http_path(
                prompt_hash,
                session_id,
                first,
            )),
        );
    }
    Some(plasm)
}

/// Serve-time verification: full chain + topo (+ optional run_seal when artifact + parsed expr available).
pub fn verify_evidence_for_http_serve(
    bundle: &EvidenceBundle,
    opts: &plasm_evidence::VerifyOptions,
    run_id_wire: &str,
    artifact: Option<&plasm_evidence::RunArtifactForSeal>,
    parsed: Option<&plasm_core::expr_parser::ParsedExpr>,
) -> Result<plasm_evidence::ChainHead, plasm_evidence::EvidenceError> {
    use plasm_evidence::DefaultChainVerifier;
    match (artifact, parsed) {
        (Some(artifact), Some(parsed)) => {
            let source_line = artifact.source_line();
            let inputs = plasm_evidence::run_seal_inputs_from_artifact(
                &bundle.scope,
                artifact,
                &source_line,
                parsed,
            );
            DefaultChainVerifier::verify_bundle_for_serve_with_run_seal(
                bundle,
                opts,
                run_id_wire,
                Some(&inputs),
            )
        }
        _ => DefaultChainVerifier::verify_bundle_for_serve(bundle, opts),
    }
}
