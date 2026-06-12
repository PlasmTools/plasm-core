//! Hash-chained causal evidence from intent through comp commit to live HTTP effects.

mod bundle;
mod canonical;
mod chain;
mod digest;
mod jcs;
mod run_seal;
mod scope;
mod segment;
mod verify;

#[cfg(feature = "signatures")]
pub mod sign;

pub use bundle::{EvidenceAnchors, EvidenceBundle, EvidenceSignature};
pub use chain::{ChainBuilder, EvidenceChain, CHAIN_BUILDER_DEFAULT_CAPACITY};
pub use digest::{ChainHead, FingerprintHex, IntentDigest, SegmentDigest};
pub use run_seal::{
    run_bundle_digest_from_run_id_wire, run_id_wire_from_digest, run_seal_inputs_from_artifact,
    RunArtifactForSeal, RUN_ARTIFACT_WIRE_PREFIX,
};
pub use scope::EvidenceScope;
pub use segment::{EvidenceKind, EvidenceSegment};
pub use verify::{DefaultChainVerifier, EvidenceError, RunSealInputs, VerifyOptions};

pub use canonical::{
    comp_semantic_canonical, compute_comp_commit_id, compute_intent_digest,
    compute_parsed_expr_digest, compute_run_bundle_digest, CanonicalError,
    EVIDENCE_SEGMENT_HASH_SCHEMA_VERSION,
};
