//! Run snapshot preimage helpers for `run_sealed` verification.

use crate::digest::SegmentDigest;
use crate::scope::EvidenceScope;
use crate::verify::{EvidenceError, RunSealInputs};
use plasm_core::expr_parser::ParsedExpr;
use serde::Deserialize;

/// ASCII prefix for run artifact wire ids (`pr` + 64 hex).
pub const RUN_ARTIFACT_WIRE_PREFIX: &str = "pr";

/// Content-addressed wire id (`pr` + lowercase hex digest).
pub fn run_id_wire_from_digest(digest: &SegmentDigest) -> String {
    format!(
        "{RUN_ARTIFACT_WIRE_PREFIX}{}",
        hex::encode(digest.as_bytes())
    )
}

/// `run_sealed.run_bundle_digest` bytes from a content-addressed `pr…` run id.
pub fn run_bundle_digest_from_run_id_wire(run_id: &str) -> Result<SegmentDigest, EvidenceError> {
    let rest = run_id
        .trim()
        .strip_prefix(RUN_ARTIFACT_WIRE_PREFIX)
        .ok_or_else(|| EvidenceError::RunSealMismatch {
            run_id: run_id.to_string(),
        })?;
    if rest.len() != 64 || !rest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(EvidenceError::RunSealMismatch {
            run_id: run_id.to_string(),
        });
    }
    let decoded = hex::decode(rest).map_err(|_| EvidenceError::RunSealMismatch {
        run_id: run_id.to_string(),
    })?;
    if decoded.len() != 32 {
        return Err(EvidenceError::RunSealMismatch {
            run_id: run_id.to_string(),
        });
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&decoded);
    Ok(SegmentDigest::from_bytes(out))
}

/// Minimal run artifact fields required to recompute `run_bundle_digest`.
#[derive(Debug, Clone, Deserialize)]
pub struct RunArtifactForSeal {
    pub entry_id: String,
    pub parsed_preimage: ParsedExpr,
    #[serde(default, alias = "expressions")]
    pub display_lines: Vec<String>,
    pub request_fingerprints: Vec<String>,
}

impl RunArtifactForSeal {
    pub fn source_line(&self) -> String {
        self.display_lines.join("\n")
    }
}

pub fn run_seal_inputs_from_artifact<'a>(
    scope: &'a EvidenceScope,
    artifact: &'a RunArtifactForSeal,
    source_line: &'a str,
    parsed: &'a ParsedExpr,
) -> RunSealInputs<'a> {
    RunSealInputs {
        catalog_cgs_hash: scope.catalog_cgs_hash.as_str(),
        domain_revision: scope.domain_revision,
        entry_id: artifact.entry_id.as_str(),
        source_line,
        parsed,
        request_fingerprints: &artifact.request_fingerprints,
    }
}
