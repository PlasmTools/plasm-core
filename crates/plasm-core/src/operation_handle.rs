//! Opaque host-minted async operation continuation handles.
//!
//! - **HTTP execute**: plain `o1`, `o2`, …
//! - **MCP `plasm_run`**: namespaced `l_<token>_o1`, … (parallel to [`PagingHandle`](crate::PagingHandle)).

use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

/// Session-scoped opaque handle for LLM `wait(...)` / `cancel(...)` continuations.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationHandle(String);

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum OperationHandleParseError {
    #[error(
        "operation handle must be plain `o` + digits or namespaced `l_<token>_o` + digits (got {0:?})"
    )]
    InvalidFormat(String),
}

fn valid_plain_operation(s: &str) -> bool {
    if s.len() < 2 || !s.starts_with('o') {
        return false;
    }
    let num = &s[1..];
    if num.is_empty() || num.len() > 24 {
        return false;
    }
    num.chars().all(|c| c.is_ascii_digit())
}

fn valid_namespaced_operation(s: &str) -> bool {
    let Some((slot, rest)) = s.rsplit_once("_o") else {
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
        if s.contains("_o") {
            if valid_namespaced_operation(s) {
                return Ok(Self(s.to_string()));
            }
            return Err(OperationHandleParseError::InvalidFormat(s.to_string()));
        }
        if valid_plain_operation(s) {
            return Ok(Self(s.to_string()));
        }
        Err(OperationHandleParseError::InvalidFormat(s.to_string()))
    }

    /// Host mint: monotonic `oN` (HTTP execute without logical session).
    #[must_use]
    pub fn mint_monotonic(n: u64) -> Self {
        Self(format!("o{n}"))
    }

    /// Host mint: MCP logical session ref + monotonic sequence within the execute session.
    pub fn mint_namespaced(logical_session_ref: &str, n: u64) -> Self {
        Self(format!("{logical_session_ref}_o{n}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `true` if this is a plain `oN` handle (HTTP path).
    #[must_use]
    pub fn is_plain(&self) -> bool {
        valid_plain_operation(self.as_str())
    }

    /// `true` if this is `l_<token>_o{m}` (MCP path).
    #[must_use]
    pub fn is_logical_namespaced(&self) -> bool {
        valid_namespaced_operation(self.as_str())
    }

    /// Logical session ref (`l_<token>`, …) when namespaced.
    pub fn logical_session_ref(&self) -> Option<&str> {
        if !self.is_logical_namespaced() {
            return None;
        }
        self.0.rsplit_once("_o").map(|(slot, _)| slot)
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
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;

    fn sample_wire_ref() -> String {
        let token = URL_SAFE_NO_PAD.encode([0u8; 16]);
        format!("l_{token}")
    }

    #[test]
    fn parse_and_mint_plain_operation() {
        let h = OperationHandle::mint_monotonic(1);
        assert_eq!(h.as_str(), "o1");
        assert!(h.is_plain());
        assert_eq!(OperationHandle::parse("o1").expect("parse"), h);
    }

    #[test]
    fn parse_and_mint_namespaced_operation() {
        let wire = sample_wire_ref();
        let h = OperationHandle::mint_namespaced(&wire, 1);
        assert_eq!(h.as_str(), format!("{wire}_o1"));
        assert_eq!(
            OperationHandle::parse(format!("{wire}_o1")).expect("parse"),
            h
        );
        assert_eq!(h.logical_session_ref(), Some(wire.as_str()));
    }

    #[test]
    fn rejects_legacy_slot_and_paging_handles() {
        assert!(OperationHandle::parse("pg1").is_err());
        assert!(OperationHandle::parse("s0_o1").is_err());
        assert!(OperationHandle::parse("s0_pg1").is_err());
    }
}
