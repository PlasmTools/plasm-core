//! Opaque host-minted async operation continuation handles (`s0_o1`, …).
//!
//! Parallel to [`PagingHandle`](crate::PagingHandle) (`s0_pg1`): MCP logical session slot + monotonic `_o` sequence.

use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

/// Session-scoped opaque handle for LLM `wait(...)` / `cancel(...)` continuations.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationHandle(String);

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum OperationHandleParseError {
    #[error("operation handle must be namespaced `s` + digits + `_o` + digits (got {0:?})")]
    InvalidFormat(String),
}

fn valid_namespaced_operation(s: &str) -> bool {
    let Some((slot, rest)) = s.split_once("_o") else {
        return false;
    };
    if !crate::paging_handle::is_valid_logical_session_ref_segment(slot) {
        return false;
    }
    if rest.is_empty() || rest.len() > 24 {
        return false;
    }
    rest.chars().all(|c| c.is_ascii_digit())
}

impl OperationHandle {
    /// Parses a client-supplied handle from `wait(<ident>)` / `cancel(<ident>)`.
    pub fn parse(s: impl AsRef<str>) -> Result<Self, OperationHandleParseError> {
        let s = s.as_ref().trim();
        if valid_namespaced_operation(s) {
            return Ok(Self(s.to_string()));
        }
        Err(OperationHandleParseError::InvalidFormat(s.to_string()))
    }

    /// Host mint: MCP logical session slot + monotonic sequence within the execute session.
    pub fn mint_namespaced(logical_session_ref: &str, n: u64) -> Self {
        Self(format!("{logical_session_ref}_o{n}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Logical session segment (`s0`, …) when namespaced.
    pub fn logical_session_ref(&self) -> Option<&str> {
        self.0.split_once("_o").map(|(slot, _)| slot)
    }
}

impl fmt::Display for OperationHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for OperationHandle {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_mint_namespaced_operation() {
        let h = OperationHandle::mint_namespaced("s0", 1);
        assert_eq!(h.as_str(), "s0_o1");
        assert_eq!(
            OperationHandle::parse("s0_o1").expect("parse"),
            h
        );
        assert_eq!(h.logical_session_ref(), Some("s0"));
    }

    #[test]
    fn rejects_plain_and_paging_handles() {
        assert!(OperationHandle::parse("pg1").is_err());
        assert!(OperationHandle::parse("s0_pg1").is_err());
    }
}
