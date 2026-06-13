//! Opaque host-minted pagination continuation handles.
//!
//! - **HTTP execute** (no MCP logical session): plain `pg1`, `pg2`, …
//! - **MCP `plasm`**: namespaced `l_<token>_pg1`, … where `l_<token>` matches [`logical_session_ref`].

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

/// Session-scoped opaque handle for LLM `page(...)` continuations (not a CGS entity name).
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PagingHandle(String);

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum PagingHandleParseError {
    #[error(
        "paging handle must be plain `pg` + digits or namespaced `l_<token>_pg` + digits (got {0:?})"
    )]
    InvalidFormat(String),
}

pub const LOGICAL_SESSION_WIRE_PREFIX: &str = "l_";
pub const LOGICAL_SESSION_WIRE_TOKEN_LEN: usize = 22;

/// `logical_session_ref` segment: `l_` + 22 URL-safe base64 chars (MCP tool contract).
#[inline]
pub fn is_valid_logical_session_ref_segment(s: &str) -> bool {
    let Some(token) = s.strip_prefix(LOGICAL_SESSION_WIRE_PREFIX) else {
        return false;
    };
    token.len() == LOGICAL_SESSION_WIRE_TOKEN_LEN
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && URL_SAFE_NO_PAD
            .decode(token)
            .map(|b| b.len() == 16)
            .unwrap_or(false)
}

fn valid_plain_paging(s: &str) -> bool {
    if s.len() < 3 || !s.starts_with("pg") {
        return false;
    }
    let num = &s[2..];
    if num.is_empty() || num.len() > 24 {
        return false;
    }
    num.chars().all(|c| c.is_ascii_digit())
}

fn valid_namespaced_paging(s: &str) -> bool {
    let Some((slot, rest)) = s.rsplit_once("_pg") else {
        return false;
    };
    if !is_valid_logical_session_ref_segment(slot) {
        return false;
    }
    if rest.is_empty() || rest.len() > 24 {
        return false;
    }
    rest.chars().all(|c| c.is_ascii_digit())
}

impl PagingHandle {
    /// Parses a client-supplied handle from `page(<ident>)` syntax: plain `pgN` or namespaced `l_<token>_pgN`.
    pub fn parse(s: impl AsRef<str>) -> Result<Self, PagingHandleParseError> {
        let s = s.as_ref().trim();
        if s.contains("_pg") {
            if valid_namespaced_paging(s) {
                return Ok(Self(s.to_string()));
            }
            return Err(PagingHandleParseError::InvalidFormat(s.to_string()));
        }
        if valid_plain_paging(s) {
            return Ok(Self(s.to_string()));
        }
        Err(PagingHandleParseError::InvalidFormat(s.to_string()))
    }

    /// Host mint: monotonic `pgN` (HTTP execute without logical session).
    #[must_use]
    pub fn mint_monotonic(n: u64) -> Self {
        Self(format!("pg{n}"))
    }

    /// Host mint: MCP logical session ref + monotonic sequence within the execute session.
    /// `logical_session_ref` must satisfy [`is_valid_logical_session_ref_segment`].
    #[must_use]
    pub fn mint_namespaced(logical_session_ref: &str, n: u64) -> Self {
        Self(format!("{logical_session_ref}_pg{n}"))
    }

    /// `true` if this is a plain `pgN` handle (HTTP path).
    #[must_use]
    pub fn is_plain(&self) -> bool {
        valid_plain_paging(self.as_str())
    }

    /// `true` if this is `l_<token>_pg{m}` (MCP path).
    #[must_use]
    pub fn is_logical_namespaced(&self) -> bool {
        valid_namespaced_paging(self.as_str())
    }

    /// For namespaced handles, returns the `l_<token>` prefix.
    #[must_use]
    pub fn logical_session_ref(&self) -> Option<&str> {
        let s = self.as_str();
        if !self.is_logical_namespaced() {
            return None;
        }
        s.rsplit_once("_pg").map(|(slot, _)| slot)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PagingHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn sample_wire_ref() -> String {
        let token = URL_SAFE_NO_PAD.encode([1u8; 16]);
        format!("l_{token}")
    }

    #[test]
    fn parse_accepts_plain() {
        assert_eq!(PagingHandle::parse("pg1").unwrap().as_str(), "pg1");
        assert_eq!(PagingHandle::parse("  pg42 ").unwrap().as_str(), "pg42");
        assert!(PagingHandle::parse("pg1").unwrap().is_plain());
        assert!(!PagingHandle::parse("pg1").unwrap().is_logical_namespaced());
    }

    #[test]
    fn parse_accepts_namespaced() {
        let wire = sample_wire_ref();
        let handle = format!("{wire}_pg1");
        let h = PagingHandle::parse(&handle).unwrap();
        assert_eq!(h.as_str(), handle);
        assert!(h.is_logical_namespaced());
        assert!(!h.is_plain());
        assert_eq!(h.logical_session_ref(), Some(wire.as_str()));
    }

    #[test]
    fn parse_accepts_token_with_dash_underscore() {
        let bytes: [u8; 16] = [
            0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44,
            0x00, 0x00,
        ];
        let wire = format!(
            "{LOGICAL_SESSION_WIRE_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(bytes)
        );
        let handle = format!("{wire}_pg9");
        assert!(PagingHandle::parse(&handle).is_ok());
    }

    #[test]
    fn parse_rejects_legacy_slot_and_bad_namespaced() {
        assert!(PagingHandle::parse("s0_pg1").is_err());
        assert!(PagingHandle::parse("l_short_pg1").is_err());
        assert!(PagingHandle::parse("l_AAAAAAAAQACAAAAAAAAAAQ_pg").is_err());
    }

    #[test]
    fn parse_rejects_non_plain() {
        assert!(PagingHandle::parse("x1").is_err());
        assert!(PagingHandle::parse("p").is_err());
        assert!(PagingHandle::parse("pg").is_err());
        assert!(PagingHandle::parse("pgx1").is_err());
    }

    #[test]
    fn mint_namespaced_shape() {
        let wire = sample_wire_ref();
        let h = PagingHandle::mint_namespaced(&wire, 7);
        assert_eq!(h.as_str(), format!("{wire}_pg7"));
        assert!(h.is_logical_namespaced());
    }

    #[test]
    fn serde_round_trips_as_string() {
        let h = PagingHandle::mint_monotonic(7);
        let v = serde_json::to_string(&h).unwrap();
        assert_eq!(v, "\"pg7\"");
        let back: PagingHandle = serde_json::from_str(&v).unwrap();
        assert_eq!(back, h);
    }
}
