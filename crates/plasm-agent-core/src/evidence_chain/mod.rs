//! Per execute-session hash-chained evidence emission (`PLASM_EVIDENCE_CHAIN=1`).
//!
//! The chain lives in an `Option` slot on [`ExecuteSession`](crate::execute_session::ExecuteSession),
//! allocated on first use while evidence is enabled — not a nested `OnceLock`.

mod config;
mod error;
mod plan;
mod records;
mod session;

#[cfg(test)]
mod tests;

pub use config::{
    evidence_chain_enabled, trusted_public_keys_from_env, ENV_PLASM_EVIDENCE_CHAIN,
    ENV_PLASM_EVIDENCE_SIGNING_KEY, ENV_PLASM_EVIDENCE_TRUSTED_PUBLIC_KEYS,
};
pub use error::EvidenceEmitError;
pub use plan::{
    active_chain, attach_evidence_meta, begin_plan_evidence, begin_plan_evidence_with_anchors,
    chain, evidence_anchors, evidence_scope_from_session, new_evidence_chain_slot,
    persist_evidence_sidecars, semantic_comp_commit_hex, semantic_comp_commit_hex_from_json,
    start_run_evidence_chain, verify_evidence_for_http_serve, EvidenceChainSlot,
};
pub use records::{RunSealRecord, StepExecutedRecord};
pub use session::EvidenceChainSession;
