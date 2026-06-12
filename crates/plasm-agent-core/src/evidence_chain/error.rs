use plasm_evidence::{CanonicalError, EvidenceError};

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
    #[error("evidence chain lock poisoned")]
    ChainLockPoisoned,
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error(transparent)]
    Chain(#[from] EvidenceError),
    #[error("evidence persist: {0}")]
    Persist(String),
}
