//! Deterministic, valid domain identities for tests that do not compile a program.
use super::model::{
    CompiledProgramId, ExecuteSessionId, ExecutionId, LogicalSessionId, PromptHashHex,
};

fn hash(label: &str) -> String {
    plasm_core::catalog_discovery::content_hash(label.as_bytes())
}

pub(crate) fn logical(label: &str) -> LogicalSessionId {
    LogicalSessionId(uuid::Uuid::parse_str(&hash(label)[..32]).expect("fixture UUID"))
}

pub(crate) fn run(label: &str) -> ExecutionId {
    ExecutionId::from_wire(&format!("we{}", hash(label))).expect("fixture run digest")
}

pub(crate) fn plan(label: &str) -> CompiledProgramId {
    hash(label)
        .parse()
        .expect("fixture compiled program digest")
}

pub(crate) fn prompt(label: &str) -> PromptHashHex {
    PromptHashHex::from_prompt_sha256(label)
}

pub(crate) fn execute(label: &str) -> ExecuteSessionId {
    hash(label)[..32].parse().expect("fixture execute identity")
}
