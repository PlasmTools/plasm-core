use super::config::signing_seed_hex_from_env;
use super::error::EvidenceEmitError;
use super::records::{RunSealRecord, StepExecutedRecord};
use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{PlanCommitId, PlasmComp};
use plasm_evidence::{
    compute_comp_commit_id, compute_intent_digest, compute_parsed_expr_digest,
    run_id_wire_from_digest, ChainBuilder, DefaultChainVerifier, EvidenceAnchors, EvidenceBundle,
    EvidenceKind, EvidenceScope, SegmentDigest, CHAIN_BUILDER_DEFAULT_CAPACITY,
};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn lock_chain<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, EvidenceEmitError> {
    mutex
        .lock()
        .map_err(|_| EvidenceEmitError::ChainLockPoisoned)
}

#[derive(Default)]
struct Inner {
    scope: Option<EvidenceScope>,
    builder: ChainBuilder,
    anchors: EvidenceAnchors,
}

/// Mutable chain builder for one intent→comp→execute transcript.
///
/// Allocated when the parent [`ExecuteSession`](crate::execute_session::ExecuteSession) enables
/// evidence (`Option` slot) — inner state is always present on this struct.
#[derive(Clone)]
pub struct EvidenceChainSession {
    inner: Arc<Mutex<Inner>>,
}

impl Default for EvidenceChainSession {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }
}

impl EvidenceChainSession {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        super::config::evidence_chain_enabled()
    }

    pub fn reset_scope(&self, scope: EvidenceScope) -> Result<(), EvidenceEmitError> {
        let mut g = lock_chain(&self.inner)?;
        *g = Inner {
            scope: Some(scope),
            builder: ChainBuilder::with_capacity(CHAIN_BUILDER_DEFAULT_CAPACITY),
            anchors: EvidenceAnchors::default(),
        };
        Ok(())
    }

    pub fn set_anchors(&self, anchors: EvidenceAnchors) -> Result<(), EvidenceEmitError> {
        lock_chain(&self.inner)?.anchors = anchors;
        Ok(())
    }

    pub fn record_intent_bound(
        &self,
        intent: &str,
        catalog_cgs_hash: &str,
        entry_id: &str,
    ) -> Result<(), EvidenceEmitError> {
        let digest = compute_intent_digest(intent, catalog_cgs_hash, entry_id, None)?;
        let mut g = lock_chain(&self.inner)?;
        g.builder
            .push(
                EvidenceKind::IntentBound {
                    intent_digest: digest,
                    intent_len: intent.trim().len() as u32,
                },
                Some(now_ms()),
            )
            .map_err(EvidenceEmitError::Chain)?;
        Ok(())
    }

    pub fn record_comp_committed(&self, comp: &PlasmComp) -> Result<(), EvidenceEmitError> {
        let commit_id = compute_comp_commit_id(comp)?;
        let semantic = SegmentDigest::from_bytes(*commit_id.as_bytes());
        let topo: Vec<String> = comp
            .bind
            .topo
            .iter()
            .map(|s| s.as_str().to_string())
            .collect();
        let mut g = lock_chain(&self.inner)?;
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
            .map_err(EvidenceEmitError::Chain)?;
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
    pub fn record_steps_executed(
        &self,
        steps: &[StepExecutedRecord],
    ) -> Result<(), EvidenceEmitError> {
        if steps.is_empty() {
            return Ok(());
        }
        let at = now_ms();
        let mut g = lock_chain(&self.inner)?;
        g.builder.reserve(steps.len());
        for step in steps {
            let fps = Self::parse_fingerprints(&step.request_fingerprints);
            let parsed_digest = compute_parsed_expr_digest(&step.parsed)?;
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
                .map_err(EvidenceEmitError::Chain)?;
        }
        Ok(())
    }

    pub fn record_run_sealed(&self, seal: &RunSealRecord) -> Result<(), EvidenceEmitError> {
        let mut g = lock_chain(&self.inner)?;
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
        )?;
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
            .map_err(EvidenceEmitError::Chain)?;
        Ok(())
    }

    pub fn verify_comp_commit_matches(
        &self,
        expected: &PlanCommitId,
    ) -> Result<(), EvidenceEmitError> {
        let g = lock_chain(&self.inner)?;
        let Some(bundle) = Self::bundle_from_inner(&g) else {
            return Ok(());
        };
        DefaultChainVerifier::verify_comp_commit_id(&bundle, &expected.to_string())
            .map_err(EvidenceEmitError::Chain)
    }

    pub fn chain_head_hex(&self) -> Option<String> {
        lock_chain(&self.inner)
            .ok()
            .and_then(|g| g.builder.head().map(|h| h.to_hex()))
    }

    pub fn finish_bundle(&self) -> Result<Option<EvidenceBundle>, EvidenceEmitError> {
        let mut g = lock_chain(&self.inner)?;
        if g.scope.is_none() && g.builder.segments().is_empty() {
            return Ok(None);
        }
        let scope = g
            .scope
            .clone()
            .ok_or(EvidenceEmitError::ScopeNotInitialized)?;
        let chain = std::mem::replace(
            &mut g.builder,
            ChainBuilder::with_capacity(CHAIN_BUILDER_DEFAULT_CAPACITY),
        )
        .finish();
        let mut bundle = EvidenceBundle {
            scope,
            chain,
            anchors: g.anchors.clone(),
            signature: None,
        };
        if let Some(seed_hex) = signing_seed_hex_from_env()? {
            let key = plasm_evidence::sign::signing_key_from_seed_hex(&seed_hex)
                .map_err(EvidenceEmitError::SigningKeyInvalid)?;
            let sig = plasm_evidence::sign::sign_bundle(&bundle, &key)?;
            bundle.signature = Some(sig);
        }
        DefaultChainVerifier::verify_emit_invariants(&bundle)?;
        Ok(Some(bundle))
    }

    pub fn segment_count(&self) -> usize {
        lock_chain(&self.inner)
            .map(|g| g.builder.segments().len())
            .unwrap_or(0)
    }

    fn bundle_from_inner(g: &Inner) -> Option<EvidenceBundle> {
        let scope = g.scope.clone()?;
        Some(EvidenceBundle {
            scope,
            chain: g.builder.clone().finish(),
            anchors: g.anchors.clone(),
            signature: None,
        })
    }
}
