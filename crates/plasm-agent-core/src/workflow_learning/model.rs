//! Durable workflow evidence and portfolio contracts. Execution receipts are host-owned.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::identity::CompiledProgramId;
pub use crate::execute_path_ids::{ExecuteSessionId, PromptHashHex};
pub use crate::run_artifacts::RunArtifactId;
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
    pub run_id: RunArtifactId,
    pub plan_id: CompiledProgramId,
    pub prompt_hash: PromptHashHex,
    pub execute_session: ExecuteSessionId,
    pub run_artifact: serde_json::Value,
    pub source: String,
    pub compiled_plan: serde_json::Value,
    pub observation: super::teaching::ProgramObservation,
    pub catalogue_requirements: super::catalogue_guidance::CatalogueRequirementEvidence,
    #[serde(
        default,
        skip_serializing_if = "crate::program_symbol_context::SavedProgramContext::is_historical_missing"
    )]
    pub symbol_context: crate::program_symbol_context::SavedProgramContext,
    pub catalog_fingerprints: BTreeMap<String, String>,
}

impl ExecutionReceipt {
    /// Validate a saved whole program under its original symbols for host repair.
    /// This does not issue a new successful execution receipt or publication authority.
    pub async fn recompile_for_repair(
        &self,
        scope: &LibraryScope,
        session: &crate::execute_session::ExecuteSession,
    ) -> Result<
        crate::plasm_comp_bundle::PlasmCompBundle,
        crate::program_symbol_context::SymbolContextError,
    > {
        if &self.scope != scope {
            return Err(crate::program_symbol_context::SymbolContextError::Identity);
        }
        self.symbol_context.recompile(self, session).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AchievedSubtask {
    pub intent: String,
    pub observed_outcome: String,
    pub run_ids: Vec<RunArtifactId>,
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
        self.eligible(seal.assurance)
            && self.checked_program().is_some_and(|program| {
                self.quality.verified_for(program, seal)
                    && seal.executions.iter().any(|receipt| {
                        super::teaching::CheckedTeachingExample::from_observation(
                            &receipt.observation,
                        )
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
