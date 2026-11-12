//! Portable retention and explicitly authorized adoption, independent of live jobs.
use super::model::LibraryScope;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ArchiveDigest(String);
impl TryFrom<String> for ArchiveDigest {
    type Error = ArchiveError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(ArchiveError::InvalidDigest);
        }
        Ok(Self(value))
    }
}
impl From<ArchiveDigest> for String {
    fn from(value: ArchiveDigest) -> Self {
        value.0
    }
}
impl ArchiveDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("archive digest is malformed")]
    InvalidDigest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveKind {
    PrivateCollection,
    ServingLibrary,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReference {
    pub source_scope: LibraryScope,
    pub review_id: Uuid,
    pub seal_digest: ArchiveDigest,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveCompatibilityIssue {
    SupportingEvidenceUnavailable,
    AuthorizationUnavailable,
    CatalogueContractsChanged,
    SessionPreparation { diagnostic: String },
    Teaching { diagnostic: String },
    Compilation { diagnostic: String },
    Infrastructure { diagnostic: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ImportDisposition {
    RetainedObservation,
    Duplicate,
    AwaitingCompatibility { reason: ArchiveCompatibilityIssue },
    EligibleCandidate,
    Quarantined { reason: ArchiveCompatibilityIssue },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedItem {
    pub identity: String,
    pub digest: ArchiveDigest,
    pub disposition: ImportDisposition,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveImportReport {
    pub archive_digest: ArchiveDigest,
    pub target_scope: LibraryScope,
    pub items: Vec<ImportedItem>,
}
