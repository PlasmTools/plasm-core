use crate::bundle::EvidenceBundle;
use crate::canonical::{compute_run_bundle_digest, hash_segment_body, segment_body_for_hash};
use crate::digest::ChainHead;
use crate::segment::EvidenceKind;
use plasm_core::expr_parser::ParsedExpr;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EvidenceError {
    #[error("evidence chain is empty")]
    EmptyChain,
    #[error("broken prev link at segment seq {seq}")]
    BrokenPrevLink { seq: u64 },
    #[error("chain head mismatch: expected {expected}, got {got}")]
    HeadMismatch { expected: String, got: String },
    #[error("run seal digest mismatch for run_id {run_id}")]
    RunSealMismatch { run_id: String },
    #[error("missing comp_committed segment")]
    MissingCompCommitted,
    #[error("comp commit mismatch: expected {expected}, got {got}")]
    CompCommitMismatch { expected: String, got: String },
    #[error("step_topo mismatch at index {index}")]
    StepTopoMismatch { index: usize },
    #[error("signature verification failed")]
    SignatureInvalid,
    #[error("serde: {0}")]
    Serde(String),
}

pub struct RunSealInputs<'a> {
    pub catalog_cgs_hash: &'a str,
    pub domain_revision: u32,
    pub entry_id: &'a str,
    pub source_line: &'a str,
    pub parsed: &'a ParsedExpr,
    pub request_fingerprints: &'a [String],
}

/// Optional verification policy (signature trust anchors).
#[derive(Clone, Debug, Default)]
pub struct VerifyOptions {
    pub trusted_public_keys: Vec<String>,
}

impl VerifyOptions {
    pub fn from_trusted_public_keys(keys: impl IntoIterator<Item = String>) -> Self {
        Self {
            trusted_public_keys: keys.into_iter().collect(),
        }
    }
}

pub struct DefaultChainVerifier;

impl DefaultChainVerifier {
    pub fn verify(bundle: &EvidenceBundle) -> Result<ChainHead, EvidenceError> {
        Self::verify_with(bundle, &VerifyOptions::default())
    }

    pub fn verify_with(
        bundle: &EvidenceBundle,
        #[cfg_attr(not(feature = "signatures"), allow(unused_variables))] opts: &VerifyOptions,
    ) -> Result<ChainHead, EvidenceError> {
        let head = bundle.chain.verify_integrity()?;
        if let Some(declared) = bundle.chain.head {
            if declared != head {
                return Err(EvidenceError::HeadMismatch {
                    expected: declared.to_hex(),
                    got: head.to_hex(),
                });
            }
        }
        #[cfg(feature = "signatures")]
        if let Some(sig) = &bundle.signature {
            crate::sign::verify_bundle_signature_trusted(bundle, sig, &opts.trusted_public_keys)?;
        }
        Ok(head)
    }

    pub fn comp_committed_plan_id(bundle: &EvidenceBundle) -> Result<&str, EvidenceError> {
        for seg in &bundle.chain.segments {
            if let EvidenceKind::CompCommitted {
                plan_commit_id_hex, ..
            } = &seg.kind
            {
                return Ok(plan_commit_id_hex.as_str());
            }
        }
        Err(EvidenceError::MissingCompCommitted)
    }

    pub fn verify_comp_commit_id(
        bundle: &EvidenceBundle,
        expected_hex: &str,
    ) -> Result<(), EvidenceError> {
        let got = Self::comp_committed_plan_id(bundle)?;
        if got != expected_hex {
            return Err(EvidenceError::CompCommitMismatch {
                expected: expected_hex.to_string(),
                got: got.to_string(),
            });
        }
        Ok(())
    }

    /// Chain integrity, optional signature trust policy, and step-topo ordering (serve/emit gate).
    pub fn verify_bundle_for_serve(
        bundle: &EvidenceBundle,
        opts: &VerifyOptions,
    ) -> Result<ChainHead, EvidenceError> {
        let head = Self::verify_with(bundle, opts)?;
        Self::verify_step_executed_topo(bundle)?;
        Ok(head)
    }

    /// Chain integrity, optional signature trust policy, step-topo ordering, and optional run seal.
    pub fn verify_bundle_for_serve_with_run_seal(
        bundle: &EvidenceBundle,
        opts: &VerifyOptions,
        run_id: &str,
        run_seal_inputs: Option<&RunSealInputs<'_>>,
    ) -> Result<ChainHead, EvidenceError> {
        let head = Self::verify_bundle_for_serve(bundle, opts)?;
        if let Some(inputs) = run_seal_inputs {
            Self::verify_run_seal_with_inputs(bundle, run_id, inputs)?;
        }
        Ok(head)
    }

    /// Emit-time gate: trust incremental `push` hashing; O(last segment) head check + topo only.
    pub fn verify_emit_invariants(bundle: &EvidenceBundle) -> Result<ChainHead, EvidenceError> {
        let head = Self::verify_emit_head(bundle)?;
        Self::verify_step_executed_topo(bundle)?;
        Ok(head)
    }

    fn verify_emit_head(bundle: &EvidenceBundle) -> Result<ChainHead, EvidenceError> {
        let Some(last) = bundle.chain.segments.last() else {
            return Err(EvidenceError::EmptyChain);
        };
        let body = segment_body_for_hash(last.seq, last.prev, &last.kind);
        let digest = hash_segment_body(&body)?;
        let computed = ChainHead::from_segment(digest);
        match bundle.chain.head {
            Some(declared) if declared != computed => Err(EvidenceError::HeadMismatch {
                expected: declared.to_hex(),
                got: computed.to_hex(),
            }),
            None => Err(EvidenceError::HeadMismatch {
                expected: computed.to_hex(),
                got: "null".into(),
            }),
            Some(declared) => Ok(declared),
        }
    }

    /// `step_executed` segments must appear in `comp_committed.step_topo` order.
    pub fn verify_step_executed_topo(bundle: &EvidenceBundle) -> Result<(), EvidenceError> {
        let topo = bundle
            .chain
            .segments
            .iter()
            .find(|s| matches!(s.kind, EvidenceKind::CompCommitted { .. }))
            .and_then(|s| match &s.kind {
                EvidenceKind::CompCommitted { step_topo, .. } => Some(step_topo.as_slice()),
                _ => None,
            })
            .ok_or(EvidenceError::MissingCompCommitted)?;
        let executed: Vec<&str> = bundle
            .chain
            .segments
            .iter()
            .filter_map(|s| match &s.kind {
                EvidenceKind::StepExecuted { step_id, .. } => Some(step_id.as_str()),
                _ => None,
            })
            .collect();
        let mut topo_idx = 0usize;
        for step_id in executed {
            while topo_idx < topo.len() && !topo[topo_idx].eq(step_id) {
                topo_idx += 1;
            }
            if topo_idx >= topo.len() {
                return Err(EvidenceError::StepTopoMismatch { index: topo_idx });
            }
            topo_idx += 1;
        }
        Ok(())
    }

    pub fn verify_run_seal_with_inputs(
        bundle: &EvidenceBundle,
        run_id: &str,
        inputs: &RunSealInputs<'_>,
    ) -> Result<(), EvidenceError> {
        let seal = bundle
            .chain
            .segments
            .iter()
            .find(|s| {
                matches!(
                    &s.kind,
                    EvidenceKind::RunSealed { run_id: rid, .. } if rid == run_id
                )
            })
            .ok_or_else(|| EvidenceError::RunSealMismatch {
                run_id: run_id.to_string(),
            })?;
        let EvidenceKind::RunSealed {
            run_bundle_digest, ..
        } = &seal.kind
        else {
            return Err(EvidenceError::RunSealMismatch {
                run_id: run_id.to_string(),
            });
        };
        let computed = compute_run_bundle_digest(
            inputs.catalog_cgs_hash,
            inputs.domain_revision,
            inputs.entry_id,
            inputs.source_line,
            inputs.parsed,
            inputs.request_fingerprints,
        )
        .map_err(|e| EvidenceError::Serde(e.to_string()))?;
        if computed != *run_bundle_digest {
            return Err(EvidenceError::RunSealMismatch {
                run_id: run_id.to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::EvidenceAnchors;
    use crate::chain::ChainBuilder;
    use crate::digest::{IntentDigest, SegmentDigest};
    use crate::scope::EvidenceScope;
    use crate::segment::EvidenceKind;

    fn scope() -> EvidenceScope {
        EvidenceScope::new_v1("p".repeat(64), "s1", "c".repeat(64), 0, "demo")
    }

    #[test]
    fn tamper_prev_breaks_verify() {
        let mut b = ChainBuilder::new();
        b.push(
            EvidenceKind::IntentBound {
                intent_digest: IntentDigest::from_bytes([1u8; 32]),
                intent_len: 3,
            },
            None,
        )
        .expect("push");
        let mut chain = b.finish();
        chain.segments[0].prev = Some(SegmentDigest::from_bytes([9u8; 32]));
        let bundle = EvidenceBundle {
            scope: scope(),
            chain,
            anchors: EvidenceAnchors::default(),
            signature: None,
        };
        assert!(matches!(
            DefaultChainVerifier::verify(&bundle),
            Err(EvidenceError::BrokenPrevLink { seq: 0 })
        ));
    }

    #[test]
    fn step_executed_topo_order_required() {
        let mut b = ChainBuilder::new();
        b.push(
            EvidenceKind::CompCommitted {
                plan_commit_id_hex: "ab".repeat(64),
                comp_semantic_sha256: SegmentDigest::from_bytes([2u8; 32]),
                step_topo: vec!["a".into(), "b".into()],
            },
            None,
        )
        .expect("push");
        b.push(
            EvidenceKind::StepExecuted {
                step_id: "b".into(),
                step_index: 0,
                entry_id: None,
                request_fingerprints: vec!["plan-compute:deadbeef".into()],
                source_line: "plan.compute(b)".into(),
                parsed_expr_digest: SegmentDigest::from_bytes([3u8; 32]),
            },
            None,
        )
        .expect("push");
        b.push(
            EvidenceKind::StepExecuted {
                step_id: "a".into(),
                step_index: 1,
                entry_id: None,
                request_fingerprints: vec![],
                source_line: "plan.compute(a)".into(),
                parsed_expr_digest: SegmentDigest::from_bytes([4u8; 32]),
            },
            None,
        )
        .expect("push");
        let bundle = EvidenceBundle {
            scope: scope(),
            chain: b.finish(),
            anchors: EvidenceAnchors::default(),
            signature: None,
        };
        assert!(matches!(
            DefaultChainVerifier::verify_step_executed_topo(&bundle),
            Err(EvidenceError::StepTopoMismatch { .. })
        ));
    }

    #[test]
    fn verify_emit_invariants_accepts_valid_chain() {
        let mut b = ChainBuilder::new();
        b.push(
            EvidenceKind::CompCommitted {
                plan_commit_id_hex: "ab".repeat(64),
                comp_semantic_sha256: SegmentDigest::from_bytes([2u8; 32]),
                step_topo: vec!["a".into()],
            },
            None,
        )
        .expect("push");
        b.push(
            EvidenceKind::StepExecuted {
                step_id: "a".into(),
                step_index: 0,
                entry_id: None,
                request_fingerprints: vec![],
                source_line: "plan.compute(a)".into(),
                parsed_expr_digest: SegmentDigest::from_bytes([3u8; 32]),
            },
            None,
        )
        .expect("push");
        let bundle = EvidenceBundle {
            scope: scope(),
            chain: b.finish(),
            anchors: EvidenceAnchors::default(),
            signature: None,
        };
        DefaultChainVerifier::verify_emit_invariants(&bundle).expect("emit invariants");
    }
}
