//! Durable workflow evidence and portfolio contracts. Execution receipts are host-owned.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::identity::{CompiledProgramId, ExecutionId};
pub use crate::execute_path_ids::{ExecuteSessionId, PromptHashHex};
pub use crate::session_identity::LogicalSessionId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryScope {
    pub tenant: String,
    pub project: String,
}

/// Only the execution host records this after observing successful completion.
/// This type is never accepted as review-tool input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub scope: LibraryScope,
    pub logical_session: LogicalSessionId,
    pub session_revision: u64,
    pub execution_id: ExecutionId,
    pub plan_id: CompiledProgramId,
    pub prompt_hash: PromptHashHex,
    pub execute_session: ExecuteSessionId,
    pub source: String,
    pub compiled_plan: serde_json::Value,
    pub teaching_plan: super::review_packet::TeachingPlan,
    pub observation: super::teaching::ProgramObservation,
    pub catalogue_requirements: super::catalogue_guidance::CatalogueRequirementEvidence,
    pub catalog_fingerprints: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReceiptDigest(String);
impl TryFrom<String> for ReceiptDigest {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err("invalid receipt digest");
        }
        Ok(Self(value))
    }
}
impl From<ReceiptDigest> for String {
    fn from(value: ReceiptDigest) -> Self {
        value.0
    }
}
impl ReceiptDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub(super) fn from_bytes(bytes: &[u8]) -> Self {
        Self(plasm_core::catalog_discovery::content_hash(bytes))
    }
}
impl ExecutionReceipt {
    pub fn evidence_digest(&self) -> Result<ReceiptDigest, serde_json::Error> {
        Ok(ReceiptDigest::from_bytes(&serde_json::to_vec(self)?))
    }
}

/// Program data shared by all execution facts for the same exact observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramEvidence {
    pub source: ProgramEvidenceSource,
    pub compiled_plan: serde_json::Value,
    pub teaching_plan: super::review_packet::TeachingPlan,
    pub catalogue_requirements: super::catalogue_guidance::CatalogueRequirementEvidence,
    pub catalog_fingerprints: BTreeMap<String, String>,
}

/// Normally the source is carried only by the compiler observation. Mismatched
/// historic data remains explicit for repair instead of silently losing bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProgramEvidenceSource {
    Available {
        observation: Box<super::teaching::ObservedTeachingSource>,
    },
    Unavailable {
        source: String,
        diagnostic: super::teaching::ObservationDiagnostic,
    },
    Conflicting {
        source: String,
        observation: Box<super::teaching::ObservedTeachingSource>,
    },
}

/// Lightweight run-specific fact; the program itself is stored once by digest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredExecutionFact {
    pub plan_id: CompiledProgramId,
    pub prompt_hash: PromptHashHex,
    pub execute_session: ExecuteSessionId,
}

impl ExecutionReceipt {
    pub fn program_evidence(&self) -> ProgramEvidence {
        ProgramEvidence {
            source: match &self.observation {
                super::teaching::ProgramObservation::Available(observation)
                    if observation.program().source == self.source =>
                {
                    ProgramEvidenceSource::Available {
                        observation: observation.clone(),
                    }
                }
                super::teaching::ProgramObservation::Available(observation) => {
                    ProgramEvidenceSource::Conflicting {
                        source: self.source.clone(),
                        observation: observation.clone(),
                    }
                }
                super::teaching::ProgramObservation::Unavailable(diagnostic) => {
                    ProgramEvidenceSource::Unavailable {
                        source: self.source.clone(),
                        diagnostic: diagnostic.clone(),
                    }
                }
            },
            compiled_plan: self.compiled_plan.clone(),
            teaching_plan: self.teaching_plan.clone(),
            catalogue_requirements: self.catalogue_requirements.clone(),
            catalog_fingerprints: self.catalog_fingerprints.clone(),
        }
    }
    pub fn stored_fact(&self) -> StoredExecutionFact {
        StoredExecutionFact {
            plan_id: self.plan_id.clone(),
            prompt_hash: self.prompt_hash.clone(),
            execute_session: self.execute_session.clone(),
        }
    }
    pub fn hydrate(
        scope: LibraryScope,
        logical_session: LogicalSessionId,
        session_revision: u64,
        execution_id: ExecutionId,
        fact: StoredExecutionFact,
        program: ProgramEvidence,
    ) -> Self {
        let (source, observation) = match program.source {
            ProgramEvidenceSource::Available { observation } => (
                observation.program().source.clone(),
                super::teaching::ProgramObservation::Available(observation),
            ),
            ProgramEvidenceSource::Unavailable { source, diagnostic } => (
                source,
                super::teaching::ProgramObservation::Unavailable(diagnostic),
            ),
            ProgramEvidenceSource::Conflicting {
                source,
                observation,
            } => (
                source,
                super::teaching::ProgramObservation::Available(observation),
            ),
        };
        Self {
            scope,
            logical_session,
            session_revision,
            execution_id,
            plan_id: fact.plan_id,
            prompt_hash: fact.prompt_hash,
            execute_session: fact.execute_session,
            source,
            compiled_plan: program.compiled_plan,
            teaching_plan: program.teaching_plan,
            observation,
            catalogue_requirements: program.catalogue_requirements,
            catalog_fingerprints: program.catalog_fingerprints,
        }
    }
}
impl ProgramEvidence {
    pub fn digest(&self) -> Result<ReceiptDigest, serde_json::Error> {
        Ok(ReceiptDigest::from_bytes(&serde_json::to_vec(self)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AchievedSubtask {
    pub intent: String,
    pub observed_outcome: String,
    pub execution_ids: Vec<ExecutionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub scope: LibraryScope,
    pub logical_session: LogicalSessionId,
    pub session_revision: u64,
    pub intent: String,
    pub subtasks: Vec<AchievedSubtask>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeAssurance {
    AgentAttested,
    PostconditionVerified,
}

/// Trusted host policy; never supplied by the reviewing agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationPolicy {
    Shadow,
    Reviewed,
    Automatic,
}

impl PublicationPolicy {
    pub fn permits_publication(self, approved: bool) -> bool {
        match self {
            Self::Shadow => false,
            Self::Reviewed => approved,
            Self::Automatic => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SealedReview {
    pub request: ReviewRequest,
    pub executions: Vec<ExecutionReceipt>,
    pub assurance: OutcomeAssurance,
}

/// Durable seal contains scoped facts, not copies of their program observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionReceiptRef {
    pub session_revision: u64,
    pub execution_id: ExecutionId,
    pub evidence_digest: ReceiptDigest,
    pub program_digest: ReceiptDigest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredReviewSeal {
    pub request: ReviewRequest,
    pub executions: Vec<ExecutionReceiptRef>,
    pub assurance: OutcomeAssurance,
}

impl StoredReviewSeal {
    pub fn from_review(review: &SealedReview) -> Result<Self, serde_json::Error> {
        let executions = review
            .executions
            .iter()
            .map(|receipt| {
                Ok(ExecutionReceiptRef {
                    session_revision: receipt.session_revision,
                    execution_id: receipt.execution_id,
                    evidence_digest: receipt.evidence_digest()?,
                    program_digest: receipt.program_evidence().digest()?,
                })
            })
            .collect::<Result<Vec<_>, serde_json::Error>>()?;
        Ok(Self {
            request: review.request.clone(),
            executions,
            assurance: review.assurance,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewHandle {
    pub id: Uuid,
    pub digest: String,
}

#[derive(Debug, Clone)]
pub struct ClaimedReview {
    pub id: Uuid,
    pub fence: i64,
    pub seal: SealedReview,
}

pub use super::quality::{
    AvoidableCallCount, CallCostAnalysis, CallCostAssessment, ExecutionEvidenceRef,
    ObservedExecutions, Quality, QualityError, ReadabilityAssessment, ReadabilityGrade,
    ReadabilityReview, RobustnessAssessment, RobustnessGap,
};

/// Typed reason retained for repair or semantic review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    UnsupportedProgram,
    DisclosureUncertainty,
    SensitiveOperand,
    Compilation,
    CatalogDrift,
    SemanticUncertainty,
    UnprovedRewrite,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalDiagnostic {
    pub kind: DiagnosticKind,
    pub message: String,
}

/// Only checked execution evidence can enter this state. No unproved rewrite
/// constructor exists; future transformations must supply a compiler witness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedProgram {
    program: super::teaching::CheckedTeachingExample,
    assurance: OutcomeAssurance,
}
impl VerifiedProgram {
    pub fn observed(
        program: super::teaching::CheckedTeachingExample,
        assurance: OutcomeAssurance,
    ) -> Self {
        Self { program, assurance }
    }
    pub fn program(&self) -> &super::teaching::CheckedTeachingExample {
        &self.program
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CandidateState {
    Verified {
        evidence: VerifiedProgram,
    },
    Uncertain {
        program: super::teaching::CheckedTeachingExample,
        diagnostic: ProposalDiagnostic,
    },
    Proposed {
        diagnostic: ProposalDiagnostic,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub intent: super::index::NormalizedIntent,
    pub embedding: Vec<f32>,
    pub embedding_profile: String,
    pub family: String,
    pub variant: String,
    pub identity: String,
    pub coverage: Vec<String>,
    pub quality: Quality,
    pub state: CandidateState,
    pub catalog_fingerprints: BTreeMap<String, String>,
    pub expected_family_version: i64,
}

impl Candidate {
    pub fn behavior_fingerprint(&self) -> Option<&str> {
        self.program().map(|p| p.behavior_fingerprint())
    }
    pub fn checked_program(&self) -> Option<&super::teaching::CheckedTeachingExample> {
        match &self.state {
            CandidateState::Verified { evidence } => Some(evidence.program()),
            CandidateState::Uncertain { program, .. } => Some(program),
            CandidateState::Proposed { .. } => None,
        }
    }
    pub fn program(&self) -> Option<&super::teaching::TeachingProgram> {
        self.checked_program().map(|p| p.program())
    }
    pub fn diagnostic(&self) -> Option<&ProposalDiagnostic> {
        match &self.state {
            CandidateState::Verified { .. } => None,
            CandidateState::Uncertain { diagnostic, .. }
            | CandidateState::Proposed { diagnostic } => Some(diagnostic),
        }
    }
    pub fn verified_by(&self, seal: &SealedReview) -> bool {
        self.observed_by(seal)
            && self
                .checked_program()
                .is_some_and(|program| self.quality.verified_for(program, seal))
    }
    /// Independent supporting reviews may observe the same program without
    /// containing the original receipts that establish its quality.
    pub(super) fn observed_by(&self, seal: &SealedReview) -> bool {
        self.eligible(seal.assurance)
            && self.checked_program().is_some_and(|program| {
                seal.executions.iter().any(|receipt| {
                    super::teaching::CheckedTeachingExample::from_observation(&receipt.observation)
                        .is_ok_and(|observed| &observed == program)
                })
            })
    }
    pub fn eligible(&self, assurance: OutcomeAssurance) -> bool {
        matches!(&self.state,CandidateState::Verified{evidence} if evidence.assurance<=assurance && self.quality.assurance<=evidence.assurance && evidence.program.integrity_valid())
            && !self.coverage.is_empty()
            && !self.family.is_empty()
            && !self.variant.is_empty()
            && !self.identity.is_empty()
            && !self.catalog_fingerprints.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredExample {
    pub id: Uuid,
    pub candidate: Candidate,
}
