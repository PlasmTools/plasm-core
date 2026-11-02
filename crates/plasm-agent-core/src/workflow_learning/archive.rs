//! Portable retention and explicitly authorized adoption, independent of live jobs.
use super::{
    catalogue_guidance::CatalogueAssociation,
    model::{Candidate, ExecutionReceipt, LibraryScope, SealedReview},
};
use crate::session_identity::LogicalSessionId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

pub const ARCHIVE_VERSION: u32 = 1;
pub const MAX_ARCHIVE_BYTES: usize = 512 * 1024 * 1024;
pub const MAX_ITEMS: usize = 10_000;
const MAX_ITEM_BYTES: usize = 16 * 1024 * 1024;

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
    #[error("archive JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("archive digest is malformed")]
    InvalidDigest,
    #[error("archive content digest differs")]
    Integrity,
    #[error("archive format is unsupported")]
    Version,
    #[error("archive exceeds bounded admission limits")]
    Limit,
    #[error("archive evidence reference is missing or conflicting")]
    Reference,
    #[error("archive scope differs from authorized source")]
    Scope,
    #[error("archive assessment or manifest is invalid")]
    InvalidMetadata,
    #[error("private evidence cannot be included in a serving-library export")]
    PrivateEvidence,
    #[error("archive database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("archive repository: {0}")]
    Repository(#[from] super::repository::RepositoryError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveKind {
    PrivateCollection,
    ServingLibrary,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskMembership {
    pub task_id: String,
    pub group_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionIdentity {
    pub protocol_digest: ArchiveDigest,
    pub tasks: Vec<TaskMembership>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TaskOutcome {
    Unavailable { reason: AssessmentUnavailable },
    Passed { passed: u32, total: u32 },
    Failed { passed: u32, total: u32 },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentUnavailable {
    NotAttempted,
    WorkerFault,
    MissingGrade,
    InvalidGrade,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAssessment {
    pub attempt_id: String,
    pub task_id: String,
    pub group_id: String,
    pub logical_sessions: Vec<LogicalSessionId>,
    pub protocol_digest: ArchiveDigest,
    pub assessor: String,
    pub outcome: TaskOutcome,
}
impl TaskAssessment {
    pub(crate) fn validate(&self) -> Result<(), ArchiveError> {
        let invalid_outcome = match self.outcome {
            TaskOutcome::Passed { passed, total } => total == 0 || passed != total,
            TaskOutcome::Failed { passed, total } => total == 0 || passed >= total,
            TaskOutcome::Unavailable { .. } => false,
        };
        if invalid_outcome
            || [
                &self.attempt_id,
                &self.task_id,
                &self.group_id,
                &self.assessor,
            ]
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 512)
            || self.logical_sessions.len() > MAX_ITEMS
        {
            return Err(ArchiveError::InvalidMetadata);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReference {
    pub source_scope: LibraryScope,
    pub review_id: Uuid,
    pub seal_digest: ArchiveDigest,
}
impl EvidenceReference {
    pub(crate) fn key(&self) -> (String, String, Uuid, ArchiveDigest) {
        (
            self.source_scope.tenant.clone(),
            self.source_scope.project.clone(),
            self.review_id,
            self.seal_digest.clone(),
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveRecord {
    /// Opaque retained ledger JSON never becomes a local execution on import.
    Observation {
        receipt: ExecutionReceipt,
    },
    HistoricalObservation {
        receipt: serde_json::Value,
    },
    Review {
        review_id: Uuid,
        seal_digest: ArchiveDigest,
        disposition: String,
        seal: SealedReview,
    },
    HistoricalReview {
        review_id: Uuid,
        seal_digest: ArchiveDigest,
        disposition: String,
        seal: serde_json::Value,
    },
    RetainedRevision {
        revision_id: Uuid,
        disposition: String,
        candidate: serde_json::Value,
    },
    Example {
        revision_id: Uuid,
        candidate: Candidate,
        support: Vec<EvidenceReference>,
    },
    Guidance {
        association_id: Uuid,
        association: CatalogueAssociation,
        support: Vec<EvidenceReference>,
    },
    Assessment {
        assessment: TaskAssessment,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveItem {
    pub identity: String,
    pub digest: ArchiveDigest,
    pub record: ArchiveRecord,
}
impl ArchiveItem {
    pub(crate) fn new(identity: String, record: ArchiveRecord) -> Result<Self, ArchiveError> {
        let digest = digest(&(&identity, &record))?;
        Ok(Self {
            identity,
            digest,
            record,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AssessmentCoverage {
    Complete,
    Partial { unassessed_tasks: Vec<String> },
    NotIncluded,
}
fn assessment_coverage(
    kind: ArchiveKind,
    collection: &CollectionIdentity,
    items: &[ArchiveItem],
) -> AssessmentCoverage {
    if kind == ArchiveKind::ServingLibrary {
        return AssessmentCoverage::NotIncluded;
    }
    let assessed: BTreeSet<_> = items
        .iter()
        .filter_map(|item| match &item.record {
            ArchiveRecord::Assessment { assessment } => Some(&assessment.task_id),
            _ => None,
        })
        .collect();
    let unassessed_tasks: Vec<_> = collection
        .tasks
        .iter()
        .filter(|task| !assessed.contains(&task.task_id))
        .map(|task| task.task_id.clone())
        .collect();
    if unassessed_tasks.is_empty() {
        AssessmentCoverage::Complete
    } else {
        AssessmentCoverage::Partial { unassessed_tasks }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TaskProvenance {
    Complete,
    Unmapped {
        logical_sessions: Vec<LogicalSessionId>,
        external_reviews: Vec<Uuid>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveManifest {
    pub version: u32,
    pub source_scope: LibraryScope,
    pub kind: ArchiveKind,
    pub collection: CollectionIdentity,
    pub assessment_coverage: AssessmentCoverage,
    pub task_provenance: TaskProvenance,
    /// References not embedded here must be resolved from retained source archives.
    pub external_evidence: Vec<EvidenceReference>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveDocument {
    pub manifest: ArchiveManifest,
    pub items: Vec<ArchiveItem>,
    pub digest: ArchiveDigest,
}
/// Construction is private: bytes, all item hashes and reference declarations passed validation.
pub struct ValidatedArchive(ArchiveDocument);
impl ValidatedArchive {
    pub fn document(&self) -> &ArchiveDocument {
        &self.0
    }
    pub fn encode(&self) -> Result<Vec<u8>, ArchiveError> {
        Ok(canonical(&self.0)?)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, ArchiveError> {
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err(ArchiveError::Limit);
        }
        let document: ArchiveDocument = serde_json::from_slice(bytes)?;
        Self::validate(document)
    }
    pub(crate) fn create(
        mut manifest: ArchiveManifest,
        mut items: Vec<ArchiveItem>,
    ) -> Result<Self, ArchiveError> {
        items.sort_by(|a, b| a.identity.cmp(&b.identity));
        manifest.assessment_coverage =
            assessment_coverage(manifest.kind, &manifest.collection, &items);
        let hash = digest(&(&manifest, &items))?;
        Self::validate(ArchiveDocument {
            manifest,
            items,
            digest: hash,
        })
    }
    fn validate(document: ArchiveDocument) -> Result<Self, ArchiveError> {
        if document.manifest.assessment_coverage
            != assessment_coverage(
                document.manifest.kind,
                &document.manifest.collection,
                &document.items,
            )
        {
            return Err(ArchiveError::InvalidMetadata);
        }
        if document.manifest.version != ARCHIVE_VERSION {
            return Err(ArchiveError::Version);
        }
        if document.items.len() > MAX_ITEMS || canonical(&document)?.len() > MAX_ARCHIVE_BYTES {
            return Err(ArchiveError::Limit);
        }
        if digest(&(&document.manifest, &document.items))? != document.digest {
            return Err(ArchiveError::Integrity);
        }
        let mut identities = BTreeSet::new();
        let mut embedded = BTreeSet::new();
        let mut requested = BTreeSet::new();
        let memberships: BTreeSet<_> = document
            .manifest
            .collection
            .tasks
            .iter()
            .map(|t| (&t.task_id, &t.group_id))
            .collect();
        if memberships.len() != document.manifest.collection.tasks.len()
            || memberships.len() > MAX_ITEMS
        {
            return Err(ArchiveError::InvalidMetadata);
        }
        for item in &document.items {
            if item.identity.is_empty()
                || item.identity.len() > 512
                || !identities.insert(&item.identity)
            {
                return Err(ArchiveError::Reference);
            }
            if canonical(&item.record)?.len() > MAX_ITEM_BYTES {
                return Err(ArchiveError::Limit);
            }
            if digest(&(&item.identity, &item.record))? != item.digest {
                return Err(ArchiveError::Integrity);
            }
            match &item.record {
                ArchiveRecord::Review {
                    review_id,
                    seal_digest,
                    seal,
                    ..
                } => {
                    if seal.request.scope != document.manifest.source_scope {
                        return Err(ArchiveError::Scope);
                    }
                    if ArchiveDigest(plasm_core::catalog_discovery::content_hash(
                        &serde_json::to_vec(&serde_json::to_value(seal)?)?,
                    )) != *seal_digest
                    {
                        return Err(ArchiveError::Integrity);
                    }
                    embedded.insert((
                        document.manifest.source_scope.tenant.clone(),
                        document.manifest.source_scope.project.clone(),
                        *review_id,
                        seal_digest.clone(),
                    ));
                }
                ArchiveRecord::Observation { receipt } => {
                    if receipt.scope != document.manifest.source_scope {
                        return Err(ArchiveError::Scope);
                    }
                }
                ArchiveRecord::HistoricalReview {
                    review_id,
                    seal_digest,
                    seal,
                    ..
                } => {
                    let scope: LibraryScope = serde_json::from_value(
                        seal.pointer("/request/scope")
                            .ok_or(ArchiveError::Scope)?
                            .clone(),
                    )?;
                    if scope != document.manifest.source_scope {
                        return Err(ArchiveError::Scope);
                    }
                    // Original repository digest hashes typed seal bytes, retained separately.
                    if ArchiveDigest(plasm_core::catalog_discovery::content_hash(
                        &serde_json::to_vec(seal)?,
                    )) != *seal_digest
                    {
                        return Err(ArchiveError::Integrity);
                    }
                    embedded.insert((
                        document.manifest.source_scope.tenant.clone(),
                        document.manifest.source_scope.project.clone(),
                        *review_id,
                        seal_digest.clone(),
                    ));
                }
                ArchiveRecord::HistoricalObservation { receipt } => {
                    let scope: LibraryScope = serde_json::from_value(
                        receipt.get("scope").ok_or(ArchiveError::Scope)?.clone(),
                    )?;
                    if scope != document.manifest.source_scope {
                        return Err(ArchiveError::Scope);
                    }
                }
                ArchiveRecord::Example { support, .. }
                | ArchiveRecord::Guidance { support, .. } => {
                    if support.is_empty() {
                        return Err(ArchiveError::Reference);
                    }
                    for reference in support {
                        requested.insert(reference.key());
                    }
                }
                ArchiveRecord::Assessment { assessment } => {
                    assessment.validate()?;
                    if assessment.protocol_digest != document.manifest.collection.protocol_digest
                        || !memberships.contains(&(&assessment.task_id, &assessment.group_id))
                    {
                        return Err(ArchiveError::InvalidMetadata);
                    }
                }
                ArchiveRecord::RetainedRevision { .. } => {}
            }
            if document.manifest.kind == ArchiveKind::ServingLibrary
                && !matches!(
                    item.record,
                    ArchiveRecord::Example { .. } | ArchiveRecord::Guidance { .. }
                )
            {
                return Err(ArchiveError::PrivateEvidence);
            }
        }
        let external: BTreeSet<_> = document
            .manifest
            .external_evidence
            .iter()
            .map(EvidenceReference::key)
            .collect();
        if external.len() != document.manifest.external_evidence.len()
            || requested
                .difference(&embedded)
                .cloned()
                .collect::<BTreeSet<_>>()
                != external
        {
            return Err(ArchiveError::Reference);
        }
        Ok(Self(document))
    }
}
pub(crate) fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    fn sort(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.into_iter()
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .map(|(k, v)| (k, sort(v)))
                    .collect(),
            ),
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(sort).collect())
            }
            other => other,
        }
    }
    serde_json::to_vec(&sort(serde_json::to_value(value)?))
}
pub(crate) fn digest<T: Serialize>(value: &T) -> Result<ArchiveDigest, ArchiveError> {
    Ok(ArchiveDigest(plasm_core::catalog_discovery::content_hash(
        &canonical(value)?,
    )))
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveExportRequest {
    pub kind: ArchiveKind,
    pub collection: CollectionIdentity,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveImportPolicy {
    pub expected_digest: ArchiveDigest,
    pub source_scope: LibraryScope,
    pub excluded_tasks: Vec<String>,
    pub excluded_groups: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveCompatibilityIssue {
    IncompatibleTeachingWitness {
        schema_version: u32,
        compiler_version: String,
        comp_wire_version: u32,
    },
    SupportingEvidenceUnavailable,
    TaskProvenanceUnavailable,
    AuthorizationUnavailable,
    CatalogueContractsChanged,
    SessionPreparation {
        diagnostic: String,
    },
    Teaching {
        diagnostic: String,
    },
    Compilation {
        diagnostic: String,
    },
    Infrastructure {
        diagnostic: String,
    },
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

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
