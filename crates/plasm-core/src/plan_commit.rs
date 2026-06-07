//! Dry-run plan acceptance tokens for soft-gate live execute.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Agent-facing plan commit ref (`pc0`, …) returned by `plasm` dry-run.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlanCommitRef(String);

/// Content-addressed commit id (internal).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct PlanCommitId([u8; 32]);

impl PlanCommitRef {
    pub fn mint(sequence: u64) -> Self {
        Self(format!("pc{sequence}"))
    }

    pub fn parse(s: impl AsRef<str>) -> Option<Self> {
        let s = s.as_ref().trim();
        if s.len() >= 3 && s.starts_with("pc") && s[2..].chars().all(|c| c.is_ascii_digit()) {
            Some(Self(s.to_string()))
        } else {
            None
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PlanCommitRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PlanCommitId {
    pub fn from_canonical_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for PlanCommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}
