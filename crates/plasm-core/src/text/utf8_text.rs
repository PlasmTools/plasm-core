//! UTF-8 text carrier — marks strings that must round-trip through interpolation and wire.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Valid UTF-8 text for program literals, template output, and wire payloads.
///
/// Construct only via [`From`]/[`FromStr`], [`Self::from_string`], or [`Self::try_from_bytes`]
/// at explicit decode boundaries — never from per-byte Latin-1 interpretation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Utf8Text(String);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Utf8FromBytesError {
    #[error("invalid UTF-8 at byte {valid_up_to}")]
    InvalidUtf8 { valid_up_to: usize },
}

impl Utf8Text {
    #[must_use]
    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    pub fn try_from_bytes(bytes: &[u8]) -> Result<Self, Utf8FromBytesError> {
        match std::str::from_utf8(bytes) {
            Ok(s) => Ok(Self(s.to_string())),
            Err(e) => Err(Utf8FromBytesError::InvalidUtf8 {
                valid_up_to: e.valid_up_to(),
            }),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn push_str(&mut self, s: &str) {
        self.0.push_str(s);
    }
}

impl fmt::Display for Utf8Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Utf8Text {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for Utf8Text {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::str::FromStr for Utf8Text {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}
