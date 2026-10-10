use plasm_evidence::{sign::SigningKeyParseError, CanonicalError, EvidenceError};

#[derive(Debug, thiserror::Error)]
pub enum EvidenceEmitError {
    #[error("evidence scope not initialized — call begin_plan_evidence first")]
    ScopeNotInitialized,
    #[error("invalid PLASM_EVIDENCE_SIGNING_KEY: {0}")]
    SigningKeyInvalid(#[source] SigningKeyParseError),
    #[error("run_id wire {supplied} does not match recomputed digest {expected}")]
    RunIdDigestMismatch { supplied: String, expected: String },
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
    #[error("evidence persist failed: {0}")]
    Persist(#[from] crate::run_artifacts::RunArtifactError),
}

impl From<EvidenceEmitError> for plasm_runtime::ExecutionFailure {
    fn from(error: EvidenceEmitError) -> Self {
        use EvidenceEmitError as E;
        let code = match &error {
            E::ScopeNotInitialized => "evidence_scope_uninitialized",
            E::SigningKeyInvalid(_) => "evidence_signing_key_invalid",
            E::RunIdDigestMismatch { .. } => "evidence_run_bundle_invalid",
            E::MissingCompCommitted => "evidence_comp_missing",
            E::CompCommitMismatch { .. } => "evidence_comp_mismatch",
            E::StepTopoMismatch { .. } => "evidence_step_order_mismatch",
            E::ChainLockPoisoned => "evidence_chain_lock_poisoned",
            E::Canonical(_) => "evidence_canonicalization_failed",
            E::Chain(_) => "evidence_chain_invalid",
            E::Persist(_) => "evidence_persistence_failed",
        };
        Self::new(
            plasm_runtime::FailureCause::Runtime,
            code,
            error.to_string(),
        )
    }
}
