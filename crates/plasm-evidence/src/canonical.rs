use crate::digest::{IntentDigest, SegmentDigest};
use crate::jcs;
use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{comp_canonical, PlanCommitId, PlasmComp};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Segment hash preimage schema (`2` = RFC 8785 JCS over `{ schema_version, seq, prev, kind }`).
pub const EVIDENCE_SEGMENT_HASH_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Error)]
pub enum CanonicalError {
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Canonical semantic comp JSON for commit hashing (matches `plasm_core::comp_canonical`).
pub fn comp_semantic_canonical(comp: &PlasmComp) -> serde_json::Value {
    comp_canonical::plasm_comp_commit_canonical(comp)
}

/// SHA256 semantic comp commit id (same as dry-run `plan_commit_id`).
pub fn compute_comp_commit_id(comp: &PlasmComp) -> Result<PlanCommitId, CanonicalError> {
    let canonical = comp_semantic_canonical(comp);
    let canonical_str = serde_json::to_string(&canonical)?;
    let digest = Sha256::digest(canonical_str.as_bytes());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    Ok(PlanCommitId::from_canonical_bytes(bytes))
}

/// Intent digest: normalized intent + scope catalog/entry keys.
pub fn compute_intent_digest(
    intent: &str,
    catalog_cgs_hash: &str,
    entry_id: &str,
    federated_seed_hash: Option<&str>,
) -> Result<IntentDigest, CanonicalError> {
    let v = serde_json::json!({
        "schema_version": 1u32,
        "intent": intent.trim(),
        "catalog_cgs_hash": catalog_cgs_hash,
        "entry_id": entry_id,
        "federated_seed_hash": federated_seed_hash,
    });
    let bytes = serde_json::to_vec(&v)?;
    let digest = Sha256::digest(&bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(IntentDigest::from_bytes(out))
}

pub fn compute_parsed_expr_digest(parsed: &ParsedExpr) -> Result<SegmentDigest, CanonicalError> {
    let bytes = serde_json::to_vec(parsed)?;
    let digest = Sha256::digest(&bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(SegmentDigest::from_bytes(out))
}

/// Matches [`RunArtifactId::from_plan_bundle_inputs`](plasm_agent_core::run_artifacts::RunArtifactId) preimage.
pub fn compute_run_bundle_digest(
    catalog_cgs_hash: &str,
    domain_revision: u32,
    entry_id: &str,
    source_line: &str,
    parsed: &ParsedExpr,
    request_fingerprints: &[String],
) -> Result<SegmentDigest, CanonicalError> {
    let mut fps: Vec<String> = request_fingerprints.to_vec();
    fps.sort();
    let v = serde_json::json!({
        "schema_version": 1u32,
        "catalog_cgs_hash": catalog_cgs_hash,
        "domain_revision": domain_revision,
        "entry_id": entry_id,
        "source_line": source_line.trim(),
        "parsed": parsed,
        "request_fingerprints": fps,
    });
    let bytes = serde_json::to_vec(&v)?;
    let digest = Sha256::digest(&bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(SegmentDigest::from_bytes(out))
}

/// Canonical segment body for hashing (excludes audit-only `emitted_at_ms`).
pub fn segment_body_for_hash(
    seq: u64,
    prev: Option<SegmentDigest>,
    kind: &crate::segment::EvidenceKind,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": EVIDENCE_SEGMENT_HASH_SCHEMA_VERSION,
        "seq": seq,
        "prev": prev.map(|d| d.to_string()),
        "kind": kind,
    })
}

pub fn hash_segment_body(
    body: &serde_json::Value,
) -> Result<SegmentDigest, crate::verify::EvidenceError> {
    let bytes = jcs::canonical_bytes(body)?;
    let digest = Sha256::digest(&bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(SegmentDigest::from_bytes(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::plasm_monad::{
        PlasmBindGraph, PlasmComp, PlasmDataValue, PlasmReturn, PlasmStepPayload, PurePayload,
        StepId,
    };
    use std::collections::BTreeMap;

    #[test]
    fn comp_commit_stable_ignores_name() {
        let mut steps = BTreeMap::new();
        steps.insert(
            "x".into(),
            PlasmStepPayload::Pure(PurePayload {
                data: PlasmDataValue::Literal {
                    value: serde_json::json!("hi"),
                },
                effect_class: plasm_core::EffectClass::ArtifactRead,
                result_shape: plasm_core::ResultShape::Artifact,
            }),
        );
        let bind = PlasmBindGraph {
            topo: vec![StepId::new("x").unwrap()],
            deps: BTreeMap::new(),
            primary: BTreeMap::new(),
            holes: BTreeMap::new(),
        };
        let a = PlasmComp {
            version: 1,
            name: Some("a".into()),
            steps: steps.clone(),
            bind: bind.clone(),
            return_: PlasmReturn::Step {
                step: StepId::new("x").unwrap(),
            },
            metadata: BTreeMap::new(),
        };
        let mut b = a.clone();
        b.name = Some("b".into());
        assert_eq!(
            compute_comp_commit_id(&a).expect("commit"),
            compute_comp_commit_id(&b).expect("commit")
        );
    }
}
