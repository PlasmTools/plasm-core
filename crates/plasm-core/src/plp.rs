//! Stable **PLP-*** diagnostic prefixes for Plasm language surface properties.
//!
//! See `docs/plasm-language-surface-invariants.md` in the monorepo.

use std::fmt::Display;

/// Stable PLP property ids referenced in diagnostics, tests, and guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlpId {
    ReferentialTransparency,
    Heredoc,
    Staging,
    Continuation,
    IngressParity,
    AgentPayload,
}

impl PlpId {
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReferentialTransparency => "PLP-1",
            Self::Heredoc => "PLP-2",
            Self::Staging => "PLP-3",
            Self::Continuation => "PLP-4",
            Self::IngressParity => "PLP-5",
            Self::AgentPayload => "PLP-6",
        }
    }
}

/// Canonical surface diagnostic: `PLP-n: {message}`.
#[inline]
pub fn surface_err(id: PlpId, msg: impl Display) -> String {
    format!("{}: {msg}", id.as_str())
}

/// Program-scoped PLP-4 continuation reject.
#[inline]
pub fn plp4_program(id: &str, msg: impl Display) -> String {
    surface_err(
        PlpId::Continuation,
        format!("Plasm program `{id}`: {msg}"),
    )
}

#[inline]
pub fn plp1_referential_transparency(msg: impl Display) -> String {
    surface_err(PlpId::ReferentialTransparency, msg)
}

#[inline]
pub fn plp2_heredoc(msg: impl Display) -> String {
    surface_err(PlpId::Heredoc, msg)
}

#[inline]
pub fn plp3_staging(msg: impl Display) -> String {
    surface_err(PlpId::Staging, msg)
}

#[inline]
pub fn plp4_continuation(msg: impl Display) -> String {
    surface_err(PlpId::Continuation, msg)
}

#[inline]
pub fn plp5_ingress_parity(msg: impl Display) -> String {
    surface_err(PlpId::IngressParity, msg)
}

#[inline]
pub fn plp6_agent_payload(msg: impl Display) -> String {
    surface_err(PlpId::AgentPayload, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plp_prefixes_are_stable() {
        assert_eq!(PlpId::Heredoc.as_str(), "PLP-2");
        assert!(plp2_heredoc("x").starts_with("PLP-2:"));
        assert!(plp4_continuation("x").starts_with("PLP-4:"));
        assert!(plp4_program("n0", "bad tail").contains("Plasm program `n0`"));
    }
}
