//! Per execute-session hash-chained evidence emission (`PLASM_EVIDENCE_CHAIN=1`).
//!
//! **Inner allocation:** `ensure_inner()` allocates when the chain is enabled and records
//! segments. `allocated_inner()` reads already-allocated state so `finish_bundle` and
//! `chain_head_hex` succeed even if `PLASM_EVIDENCE_CHAIN` is toggled off mid-session.

use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{PlanCommitId, PlasmComp};
use plasm_evidence::{
    compute_comp_commit_id, compute_intent_digest, compute_parsed_expr_digest, ChainBuilder,
    DefaultChainVerifier, EvidenceAnchors, EvidenceBundle, EvidenceKind, EvidenceScope,
    run_id_wire_from_digest, CanonicalError, SegmentDigest, CHAIN_BUILDER_DEFAULT_CAPACITY,
};
use std::env;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub const ENV_PLASM_EVIDENCE_CHAIN: &str = "PLASM_EVIDENCE_CHAIN";
pub const ENV_PLASM_EVIDENCE_SIGNING_KEY: &str = "PLASM_EVIDENCE_SIGNING_KEY";
pub const ENV_PLASM_EVIDENCE_TRUSTED_PUBLIC_KEYS: &str = "PLASM_EVIDENCE_TRUSTED_PUBLIC_KEYS";

static TRUSTED_PUBLIC_KEYS_CACHE: OnceLock<Vec<String>> = OnceLock::new();
static SIGNING_SEED_HEX_CACHE: OnceLock<String> = OnceLock::new();

pub fn evidence_chain_enabled() -> bool {
    env::var(ENV_PLASM_EVIDENCE_CHAIN)
        .ok()
        .map(|v| {
            let t = v.trim();
            t == "1" || t.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

#[derive(Debug, thiserror::Error)]
pub enum EvidenceEmitError {
    #[error("evidence scope not initialized — call begin_plan_evidence first")]
    ScopeNotInitialized,
    #[error("invalid PLASM_EVIDENCE_SIGNING_KEY: {0}")]
    SigningKeyInvalid(String),
    #[error("run bundle digest: {0}")]
    RunBundleDigest(String),
    #[error("evidence chain missing comp_committed segment")]
    MissingCompCommitted,
    #[error("evidence comp commit mismatch: expected {expected}, chain has {got}")]
    CompCommitMismatch { expected: String, got: String },
    #[error("evidence step_topo mismatch at index {index}: expected {expected}, got {got}")]
    StepTopoMismatch {
        index: usize,
        expected: String,
        got: String,
    },
    #[error("evidence verify: {0}")]
    Verify(String),
    #[error("evidence persist: {0}")]
    Persist(String),
}

/// One materialized return-step run snapshot for `run_sealed` recording.
#[derive(Debug, Clone)]
pub struct RunSealRecord {
    pub expected_run_id_wire: String,
    pub step_id: Option<String>,
    pub resource_index: Option<u64>,
    pub entry_id: String,
    pub source_line: String,
    pub parsed: ParsedExpr,
    pub request_fingerprints: Vec<String>,
}

fn map_canonical_err(e: CanonicalError) -> EvidenceEmitError {
    EvidenceEmitError::Verify(e.to_string())
}

fn lock_chain<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, EvidenceEmitError> {
    mutex
        .lock()
        .map_err(|_| EvidenceEmitError::Verify("evidence chain lock poisoned".into()))
}

/// One materialized plan step for batched `step_executed` recording.
#[derive(Debug, Clone)]
pub struct StepExecutedRecord {
    pub step_id: String,
    pub step_index: u32,
    pub entry_id: Option<String>,
    pub source_line: String,
    pub parsed: ParsedExpr,
    pub request_fingerprints: Vec<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
struct Inner {
    scope: Option<EvidenceScope>,
    builder: ChainBuilder,
    anchors: EvidenceAnchors,
}

/// Mutable chain builder for one intent→comp→execute transcript.
///
/// Inner state is allocated lazily on first use while `PLASM_EVIDENCE_CHAIN=1`.
#[derive(Default)]
pub struct EvidenceChainSession {
    inner: OnceLock<Arc<Mutex<Inner>>>,
}

impl Clone for EvidenceChainSession {
    fn clone(&self) -> Self {
        let cloned = Self::default();
        if let Some(arc) = self.inner.get() {
            let _ = cloned.inner.set(arc.clone());
        }
        cloned
    }
}

impl EvidenceChainSession {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_inner(&self) -> Option<Arc<Mutex<Inner>>> {
        if !evidence_chain_enabled() {
            return None;
        }
        Some(
            self.inner
                .get_or_init(|| Arc::new(Mutex::new(Inner::default())))
                .clone(),
        )
    }

    fn allocated_inner(&self) -> Option<Arc<Mutex<Inner>>> {
        self.inner.get().cloned()
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        evidence_chain_enabled()
    }

    pub fn reset_scope(&self, scope: EvidenceScope) -> Result<(), EvidenceEmitError> {
        let Some(inner) = self.ensure_inner() else {
            return Ok(());
        };
        let mut g = lock_chain(&inner)?;
        *g = Inner {
            scope: Some(scope),
            builder: ChainBuilder::with_capacity(CHAIN_BUILDER_DEFAULT_CAPACITY),
            anchors: EvidenceAnchors::default(),
        };
        Ok(())
    }

    pub fn set_anchors(&self, anchors: EvidenceAnchors) -> Result<(), EvidenceEmitError> {
        let Some(inner) = self.ensure_inner() else {
            return Ok(());
        };
        lock_chain(&inner)?.anchors = anchors;
        Ok(())
    }

    pub fn record_intent_bound(
        &self,
        intent: &str,
        catalog_cgs_hash: &str,
        entry_id: &str,
    ) -> Result<(), EvidenceEmitError> {
        let Some(inner) = self.ensure_inner() else {
            return Ok(());
        };
        let digest = compute_intent_digest(intent, catalog_cgs_hash, entry_id, None)
            .map_err(map_canonical_err)?;
        let mut g = lock_chain(&inner)?;
        g.builder
            .push(
                EvidenceKind::IntentBound {
                    intent_digest: digest,
                    intent_len: intent.trim().len() as u32,
                },
                Some(now_ms()),
            )
            .map_err(|e| EvidenceEmitError::Verify(e.to_string()))?;
        Ok(())
    }

    pub fn record_comp_committed(&self, comp: &PlasmComp) -> Result<(), EvidenceEmitError> {
        let Some(inner) = self.ensure_inner() else {
            return Ok(());
        };
        let commit_id = compute_comp_commit_id(comp).map_err(map_canonical_err)?;
        let semantic = SegmentDigest::from_bytes(*commit_id.as_bytes());
        let topo: Vec<String> = comp
            .bind
            .topo
            .iter()
            .map(|s| s.as_str().to_string())
            .collect();
        let mut g = lock_chain(&inner)?;
        // intent + comp + steps + run_sealed margin
        g.builder.reserve(topo.len().saturating_add(4));
        g.builder
            .push(
                EvidenceKind::CompCommitted {
                    plan_commit_id_hex: commit_id.to_string(),
                    comp_semantic_sha256: semantic,
                    step_topo: topo,
                },
                Some(now_ms()),
            )
            .map_err(|e| EvidenceEmitError::Verify(e.to_string()))?;
        Ok(())
    }

    fn parse_fingerprints(fps: &[String]) -> Vec<String> {
        fps.iter().map(|f| f.trim().to_string()).collect()
    }

    pub fn record_step_executed(
        &self,
        step_id: &str,
        step_index: u32,
        entry_id: Option<&str>,
        source_line: &str,
        parsed: &ParsedExpr,
        request_fingerprints: &[String],
    ) -> Result<(), EvidenceEmitError> {
        self.record_steps_executed(&[StepExecutedRecord {
            step_id: step_id.to_string(),
            step_index,
            entry_id: entry_id.map(str::to_string),
            source_line: source_line.to_string(),
            parsed: parsed.clone(),
            request_fingerprints: request_fingerprints.to_vec(),
        }])
    }

    /// Record many `step_executed` segments under one mutex hold.
    pub fn record_steps_executed(&self, steps: &[StepExecutedRecord]) -> Result<(), EvidenceEmitError> {
        if steps.is_empty() {
            return Ok(());
        }
        let Some(inner) = self.ensure_inner() else {
            return Ok(());
        };
        let at = now_ms();
        let mut g = lock_chain(&inner)?;
        g.builder.reserve(steps.len());
        for step in steps {
            let fps = Self::parse_fingerprints(&step.request_fingerprints);
            let parsed_digest = compute_parsed_expr_digest(&step.parsed).map_err(map_canonical_err)?;
            g.builder
                .push(
                    EvidenceKind::StepExecuted {
                        step_id: step.step_id.clone(),
                        step_index: step.step_index,
                        entry_id: step.entry_id.clone(),
                        request_fingerprints: fps,
                        source_line: step.source_line.clone(),
                        parsed_expr_digest: parsed_digest,
                    },
                    Some(at),
                )
                .map_err(|e| EvidenceEmitError::Verify(e.to_string()))?;
        }
        Ok(())
    }

    pub fn record_run_sealed(&self, seal: &RunSealRecord) -> Result<(), EvidenceEmitError> {
        let Some(inner) = self.ensure_inner() else {
            return Ok(());
        };
        let mut g = lock_chain(&inner)?;
        let scope = g
            .scope
            .as_ref()
            .ok_or(EvidenceEmitError::ScopeNotInitialized)?;
        let digest = plasm_evidence::compute_run_bundle_digest(
            scope.catalog_cgs_hash.as_str(),
            scope.domain_revision,
            seal.entry_id.as_str(),
            seal.source_line.as_str(),
            &seal.parsed,
            &seal.request_fingerprints,
        )
        .map_err(map_canonical_err)?;
        let expected_wire = run_id_wire_from_digest(&digest);
        if seal.expected_run_id_wire.trim() != expected_wire {
            return Err(EvidenceEmitError::RunBundleDigest(format!(
                "run_id wire {:?} does not match recomputed digest (expected {expected_wire})",
                seal.expected_run_id_wire
            )));
        }
        g.builder
            .push(
                EvidenceKind::RunSealed {
                    run_id: expected_wire,
                    step_id: seal.step_id.clone(),
                    resource_index: seal.resource_index,
                    run_bundle_digest: digest,
                },
                Some(now_ms()),
            )
            .map_err(|e| EvidenceEmitError::Verify(e.to_string()))?;
        Ok(())
    }

    pub fn verify_comp_commit_matches(&self, expected: &PlanCommitId) -> Result<(), EvidenceEmitError> {
        let Some(inner) = self.allocated_inner() else {
            return Ok(());
        };
        let g = lock_chain(&inner)?;
        let Some(bundle) = Self::bundle_from_inner(&g) else {
            return Ok(());
        };
        DefaultChainVerifier::verify_comp_commit_id(&bundle, &expected.to_string())
            .map_err(|e| EvidenceEmitError::Verify(e.to_string()))
    }

    pub fn chain_head_hex(&self) -> Option<String> {
        let inner = self.allocated_inner()?;
        inner
            .lock()
            .ok()
            .and_then(|g| g.builder.head().map(|h| h.to_hex()))
    }

    pub fn finish_bundle(&self) -> Result<Option<EvidenceBundle>, EvidenceEmitError> {
        let Some(inner) = self.allocated_inner() else {
            return Ok(None);
        };
        let mut g = lock_chain(&inner)?;
        let scope = g.scope.clone().ok_or(EvidenceEmitError::ScopeNotInitialized)?;
        let chain = std::mem::replace(
            &mut g.builder,
            ChainBuilder::with_capacity(CHAIN_BUILDER_DEFAULT_CAPACITY),
        )
        .finish_trusted();
        let mut bundle = EvidenceBundle {
            scope,
            chain,
            anchors: g.anchors.clone(),
            signature: None,
        };
        if let Some(seed_hex) = signing_seed_hex_from_env()? {
            let key = plasm_evidence::sign::signing_key_from_seed_hex(&seed_hex)
                .map_err(EvidenceEmitError::SigningKeyInvalid)?;
            let sig = plasm_evidence::sign::sign_bundle(&bundle, &key)
                .map_err(|e| EvidenceEmitError::Verify(e.to_string()))?;
            bundle.signature = Some(sig);
        }
        DefaultChainVerifier::verify_emit_invariants(&bundle)
            .map_err(|e| EvidenceEmitError::Verify(e.to_string()))?;
        Ok(Some(bundle))
    }

    pub fn segment_count(&self) -> usize {
        self.allocated_inner()
            .and_then(|inner| inner.lock().ok().map(|g| g.builder.segments().len()))
            .unwrap_or(0)
    }

    fn bundle_from_inner(g: &Inner) -> Option<EvidenceBundle> {
        let scope = g.scope.clone()?;
        Some(EvidenceBundle {
            scope,
            chain: g.builder.clone().finish_trusted(),
            anchors: g.anchors.clone(),
            signature: None,
        })
    }
}

fn signing_seed_hex_from_env() -> Result<Option<String>, EvidenceEmitError> {
    if cfg!(test) {
        return signing_seed_hex_from_env_uncached();
    }
    if let Some(cached) = SIGNING_SEED_HEX_CACHE.get() {
        return Ok(Some(cached.clone()));
    }
    let seed = match signing_seed_hex_from_env_uncached()? {
        Some(s) => s,
        None => return Ok(None),
    };
    let _ = SIGNING_SEED_HEX_CACHE.set(seed.clone());
    Ok(Some(seed))
}

fn signing_seed_hex_from_env_uncached() -> Result<Option<String>, EvidenceEmitError> {
    let raw = match env::var(ENV_PLASM_EVIDENCE_SIGNING_KEY) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let seed = raw.trim();
    if seed.is_empty() {
        return Ok(None);
    }
    let bytes = hex::decode(seed).map_err(|e| {
        EvidenceEmitError::SigningKeyInvalid(format!("invalid seed hex: {e}"))
    })?;
    if bytes.len() != 32 {
        return Err(EvidenceEmitError::SigningKeyInvalid(
            "PLASM_EVIDENCE_SIGNING_KEY must be 32-byte hex".into(),
        ));
    }
    Ok(Some(seed.to_string()))
}

pub fn evidence_scope_from_session(
    sess: &crate::execute_session::ExecuteSession,
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

pub fn trusted_public_keys_from_env() -> Vec<String> {
    TRUSTED_PUBLIC_KEYS_CACHE
        .get_or_init(|| {
            env::var(ENV_PLASM_EVIDENCE_TRUSTED_PUBLIC_KEYS)
                .ok()
                .map(|raw| plasm_evidence::sign::parse_trusted_public_keys_csv(&raw))
                .unwrap_or_default()
        })
        .clone()
}

/// Serve-time verification: full chain + topo (+ optional run_seal when artifact + parsed expr available).
pub fn verify_evidence_for_http_serve(
    bundle: &EvidenceBundle,
    opts: &plasm_evidence::VerifyOptions,
    run_id_wire: &str,
    artifact: Option<&plasm_evidence::RunArtifactForSeal>,
    parsed: Option<&ParsedExpr>,
) -> Result<plasm_evidence::ChainHead, plasm_evidence::EvidenceError> {
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

/// Reset chain and record intent anchor at the start of a plan dry/live cycle.
pub fn begin_plan_evidence(
    sess: &crate::execute_session::ExecuteSession,
    execute_session_id: &str,
) -> Result<(), EvidenceEmitError> {
    if !evidence_chain_enabled() {
        return Ok(());
    }
    let scope = evidence_scope_from_session(sess, execute_session_id);
    sess.evidence_chain.reset_scope(scope)?;
    if let Some(intent) = sess.context_intent.as_deref() {
        sess.evidence_chain.record_intent_bound(
            intent,
            sess.catalog_cgs_hash.as_str(),
            sess.entry_id.as_str(),
        )?;
    }
    Ok(())
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

pub fn begin_plan_evidence_with_anchors(
    sess: &crate::execute_session::ExecuteSession,
    execute_session_id: &str,
    anchors: EvidenceAnchors,
) -> Result<(), EvidenceEmitError> {
    begin_plan_evidence(sess, execute_session_id)?;
    sess.evidence_chain.set_anchors(anchors)
}

/// Semantic comp commit hex for archive `plan_hash` (matches `plan_commit_id`).
pub fn semantic_comp_commit_hex(comp: &PlasmComp) -> String {
    compute_comp_commit_id(comp)
        .expect("comp semantic canonical always serializes")
        .to_string()
}

pub fn semantic_comp_commit_hex_from_json(comp: &serde_json::Value) -> String {
    if let Ok(c) = serde_json::from_value::<PlasmComp>(comp.clone()) {
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

impl EvidenceChainSession {
    #[cfg(test)]
    pub(crate) fn is_inner_allocated(&self) -> bool {
        self.inner.get().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::expr_parser::ParsedExpr;
    use plasm_core::{Expr, Value};
    use std::sync::Mutex;

    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn env_test_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn session_inner_unallocated_when_disabled() {
        let _guard = env_test_guard();
        std::env::remove_var(ENV_PLASM_EVIDENCE_CHAIN);
        let chain = EvidenceChainSession::new();
        assert!(!chain.is_inner_allocated());
        chain
            .record_step_executed("x", 0, None, "line", &ParsedExpr {
                expr: Expr::TeachingValue {
                    value: Value::String("x".into()),
                },
                projection: None,
            }, &[])
            .expect("noop");
        assert!(!chain.is_inner_allocated());
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
        chain
            .record_comp_committed(&minimal_comp())
            .expect("comp");
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
        chain
            .record_comp_committed(&minimal_comp())
            .expect("comp");
        let err = chain.finish_bundle().expect_err("invalid key");
        assert!(matches!(err, EvidenceEmitError::SigningKeyInvalid(_)));
        std::env::remove_var(ENV_PLASM_EVIDENCE_SIGNING_KEY);
    }

    fn minimal_comp() -> PlasmComp {
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
        PlasmComp {
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
}
