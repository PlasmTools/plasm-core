//! Executable targets are distinct from observation artifact identifiers.
use crate::{PagingHandle, PlanCommitRef};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutableRunTarget {
    Commit(PlanCommitRef),
    Page(PagingHandle),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("expected a reviewed commit reference or paging handle, got `{token}`")]
pub struct RunReferenceParseError {
    pub token: String,
}

impl ExecutableRunTarget {
    pub fn parse(raw: &str) -> Result<Self, RunReferenceParseError> {
        if let Some(reference) = PlanCommitRef::parse(raw) {
            return Ok(Self::Commit(reference));
        }
        if let Ok(handle) = PagingHandle::parse(raw) {
            return Ok(Self::Page(handle));
        }
        Err(RunReferenceParseError {
            token: raw.to_owned(),
        })
    }
    pub fn as_str(&self) -> &str {
        match self {
            Self::Commit(reference) => reference.as_str(),
            Self::Page(handle) => handle.as_str(),
        }
    }
    pub fn is_canonical(raw: &str) -> bool {
        Self::parse(raw).is_ok_and(|reference| reference.as_str() == raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn executable_targets_are_typed_and_exclude_artifacts() {
        for sequence in [0, 1, u64::MAX] {
            let commit = PlanCommitRef::mint(sequence);
            assert_eq!(
                ExecutableRunTarget::parse(commit.as_str()),
                Ok(ExecutableRunTarget::Commit(commit))
            );
            let page = PagingHandle::mint_monotonic(sequence);
            assert_eq!(
                ExecutableRunTarget::parse(page.as_str()),
                Ok(ExecutableRunTarget::Page(page))
            );
        }
        for raw in [
            "",
            "for",
            "????",
            "pr0000000000000000000000000000000000000000000000000000000000000000",
        ] {
            assert!(ExecutableRunTarget::parse(raw).is_err());
        }
        assert!(!ExecutableRunTarget::is_canonical(" pc1 "));
    }
}
