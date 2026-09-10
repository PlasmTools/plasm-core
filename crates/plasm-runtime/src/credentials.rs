//! Scoped credential references shared by host persistence and transport dispatch.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialScope {
    pub session: String,
    pub catalog_revision: String,
    pub origin: String,
    pub slot: String,
    pub resource: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CredentialReference(String);

impl TryFrom<String> for CredentialReference {
    type Error = crate::RuntimeError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<CredentialReference> for String {
    fn from(reference: CredentialReference) -> Self {
        reference.0
    }
}

impl CredentialReference {
    pub fn parse(value: &str) -> Result<Self, crate::RuntimeError> {
        if value.len() != 34
            || !value.starts_with("cr")
            || !value[2..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(credential_error("invalid credential reference"));
        }
        Ok(Self(value.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Reference and scope metadata only. Secret material remains with the host injection provider.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredCredential {
    pub reference: CredentialReference,
    pub scope: CredentialScope,
    pub source: plasm_compile::CredentialSource,
    pub expires_at_unix: u64,
}

impl std::fmt::Debug for StoredCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredCredential")
            .field("reference", &self.reference)
            .field("scope", &self.scope)
            .field("expires_at_unix", &self.expires_at_unix)
            .finish_non_exhaustive()
    }
}

pub fn credential_error(message: &str) -> crate::RuntimeError {
    crate::RuntimeError::ConfigurationError {
        message: message.into(),
    }
}

#[async_trait]
pub trait SessionCredentialStore: Send + Sync + std::fmt::Debug {
    /// Commit before acknowledging. Identical input retries return the same immutable record.
    async fn bind(
        &self,
        scope: CredentialScope,
        source: plasm_compile::CredentialSource,
        lifetime_seconds: u64,
    ) -> Result<CredentialReference, crate::RuntimeError>;
    /// Resolution must match the entire scope and reject expired references.
    async fn resolve(
        &self,
        reference: &CredentialReference,
        scope: &CredentialScope,
    ) -> Result<plasm_compile::CredentialSource, crate::RuntimeError>;
}
