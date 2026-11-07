//! Evidence-backed assessments of observed behavior, call cost, and readability.
//! Successful execution is evidence of observation, not edge-case robustness.
use super::model::{ExecutionId, ExecutionReceipt, OutcomeAssurance, SealedReview};
use super::teaching::CheckedTeachingExample;
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::BTreeSet, num::NonZeroU32};

#[derive(Debug, thiserror::Error)]
pub enum QualityError {
    #[error("quality assessment requires matching sealed execution evidence")]
    MissingEvidence,
    #[error(
        "receipts for one program observation disagree on source, checked program, or execution context"
    )]
    InconsistentProgramObservation,
    #[error("execution evidence exceeds supported count")]
    EvidenceCount,
    #[error(transparent)]
    Encoding(#[from] serde_json::Error),
}

/// One authored program observation can publish several result artifacts.
/// Artifact cardinality is not independent execution evidence.
pub(super) fn program_observations<'a>(
    receipts: impl IntoIterator<Item = &'a ExecutionReceipt>,
) -> Result<Vec<Vec<&'a ExecutionReceipt>>, QualityError> {
    let mut groups = std::collections::BTreeMap::new();
    for receipt in receipts {
        let key = (
            &receipt.scope.tenant,
            &receipt.scope.project,
            &receipt.logical_session,
            receipt.session_revision,
            &receipt.plan_id,
        );
        let group: &mut Vec<&ExecutionReceipt> = groups.entry(key).or_default();
        if let Some(first) = group.first() {
            if first.source != receipt.source
                || first.compiled_plan != receipt.compiled_plan
                || first.observation != receipt.observation
                || first.catalogue_requirements != receipt.catalogue_requirements
                || first.teaching_plan != receipt.teaching_plan
                || first.catalog_fingerprints != receipt.catalog_fingerprints
                || first.prompt_hash != receipt.prompt_hash
                || first.execute_session != receipt.execute_session
            {
                return Err(QualityError::InconsistentProgramObservation);
            }
        }
        group.push(receipt);
    }
    Ok(groups.into_values().collect())
}

/// Immutable reference to the exact host receipt, not an LLM assertion.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExecutionEvidenceRef {
    execution_id: ExecutionId,
    receipt_digest: String,
}
impl ExecutionEvidenceRef {
    fn from_receipt(receipt: &ExecutionReceipt) -> Result<Self, QualityError> {
        Ok(Self {
            execution_id: receipt.execution_id,
            receipt_digest: receipt.evidence_digest()?.as_str().to_owned(),
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ExecutionEvidence {
    receipts: BTreeSet<ExecutionEvidenceRef>,
}
impl ExecutionEvidence {
    fn selected(seal: &SealedReview, execution_ids: &[ExecutionId]) -> Result<Self, QualityError> {
        let requested: BTreeSet<_> = execution_ids.iter().collect();
        if requested.is_empty() {
            return Err(QualityError::MissingEvidence);
        }
        let receipts = seal
            .executions
            .iter()
            .filter(|receipt| requested.contains(&receipt.execution_id))
            .map(ExecutionEvidenceRef::from_receipt)
            .collect::<Result<BTreeSet<_>, _>>()?;
        if receipts.len() != requested.len() {
            return Err(QualityError::MissingEvidence);
        }
        Ok(Self { receipts })
    }
    #[cfg(test)]
    fn for_program(
        seal: &SealedReview,
        program: &CheckedTeachingExample,
    ) -> Result<Self, QualityError> {
        let ids = seal
            .executions
            .iter()
            .filter(|receipt| {
                CheckedTeachingExample::from_observation(&receipt.observation)
                    .is_ok_and(|observed| &observed == program)
            })
            .map(|receipt| receipt.execution_id)
            .collect::<Vec<_>>();
        Self::selected(seal, &ids)
    }
    fn verified_by(&self, seal: &SealedReview) -> bool {
        !self.receipts.is_empty()
            && self.receipts.iter().all(|reference| {
                seal.executions.iter().any(|receipt| {
                    ExecutionEvidenceRef::from_receipt(receipt)
                        .is_ok_and(|actual| actual == *reference)
                })
            })
    }
    fn observation_count(&self, seal: &SealedReview) -> Result<usize, QualityError> {
        if !self.verified_by(seal) {
            return Err(QualityError::MissingEvidence);
        }
        let selected = seal.executions.iter().filter(|receipt| {
            self.receipts
                .iter()
                .any(|reference| reference.execution_id == receipt.execution_id)
        });
        Ok(program_observations(selected)?.len())
    }
    fn checked_programs(&self, seal: &SealedReview) -> Option<Vec<CheckedTeachingExample>> {
        if !self.verified_by(seal) {
            return None;
        }
        self.receipts
            .iter()
            .map(|reference| {
                let receipt = seal
                    .executions
                    .iter()
                    .find(|receipt| receipt.execution_id == reference.execution_id)?;
                CheckedTeachingExample::from_observation(&receipt.observation).ok()
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedExecutions {
    count: NonZeroU32,
    evidence: ExecutionEvidence,
}
impl ObservedExecutions {
    pub fn count(&self) -> NonZeroU32 {
        self.count
    }
    fn verified_by(&self, seal: &SealedReview) -> bool {
        self.evidence.verified_by(seal)
            && usize::try_from(self.count.get()).ok() == self.evidence.observation_count(seal).ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RobustnessGap {
    NoMatchingExecutions,
    MissingTeachingEvidence,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "assessment", content = "evidence", rename_all = "snake_case")]
pub enum RobustnessAssessment {
    Unassessed,
    ObservedSuccess(ObservedExecutions),
    InsufficientEvidence(RobustnessGap),
}
impl RobustnessAssessment {
    fn verified_by(&self, seal: &SealedReview) -> bool {
        match self {
            Self::Unassessed | Self::InsufficientEvidence(_) => true,
            Self::ObservedSuccess(observed) => observed.verified_by(seal),
        }
    }
    fn compare(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Self::ObservedSuccess(a), Self::ObservedSuccess(b)) if a == b => Some(Ordering::Equal),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AvoidableCallCount(u32);
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallCostAnalysis {
    avoidable_calls: AvoidableCallCount,
    behavior: String,
    evidence: ExecutionEvidence,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "assessment", content = "evidence", rename_all = "snake_case")]
pub enum CallCostAssessment {
    Unanalysed,
    Analysed(CallCostAnalysis),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadabilityGrade {
    NeedsExplanation,
    Clear,
}
/// No trusted readability-review producer exists yet. Private fields prevent an
/// execution observation from being recast as a readability judgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadabilityReview {
    grade: ReadabilityGrade,
    evidence: ExecutionEvidence,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "assessment", content = "evidence", rename_all = "snake_case")]
pub enum ReadabilityAssessment {
    Unreviewed,
    Reviewed(ReadabilityReview),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quality {
    pub assurance: OutcomeAssurance,
    pub robustness: RobustnessAssessment,
    pub call_cost: CallCostAssessment,
    pub teaching_tokens: u32,
    pub readability: ReadabilityAssessment,
}
impl Quality {
    pub fn unassessed(assurance: OutcomeAssurance, teaching_tokens: u32) -> Self {
        Self {
            assurance,
            robustness: RobustnessAssessment::Unassessed,
            call_cost: CallCostAssessment::Unanalysed,
            teaching_tokens,
            readability: ReadabilityAssessment::Unreviewed,
        }
    }
    pub fn observed(
        seal: &SealedReview,
        execution_ids: &[ExecutionId],
        teaching_tokens: u32,
    ) -> Result<Self, QualityError> {
        let evidence = ExecutionEvidence::selected(seal, execution_ids)?;
        let count = NonZeroU32::new(
            u32::try_from(evidence.observation_count(seal)?)
                .map_err(|_| QualityError::EvidenceCount)?,
        )
        .ok_or(QualityError::MissingEvidence)?;
        Ok(Self {
            robustness: RobustnessAssessment::ObservedSuccess(ObservedExecutions {
                count,
                evidence,
            }),
            ..Self::unassessed(seal.assurance, teaching_tokens)
        })
    }
    pub fn verified_by(&self, seal: &SealedReview) -> bool {
        self.assurance <= seal.assurance
            && self.robustness.verified_by(seal)
            && matches!(self.call_cost, CallCostAssessment::Unanalysed)
            // Until a trusted review producer exists, persisted grades are not proof.
            && matches!(self.readability, ReadabilityAssessment::Unreviewed)
    }
    /// Publication requires every assessment receipt to describe this candidate,
    /// not merely some other successful program in the same sealed session.
    pub fn verified_for(&self, program: &CheckedTeachingExample, seal: &SealedReview) -> bool {
        if !self.verified_by(seal) {
            return false;
        }
        let evidence = match &self.robustness {
            RobustnessAssessment::ObservedSuccess(observed) => &observed.evidence,
            RobustnessAssessment::Unassessed | RobustnessAssessment::InsufficientEvidence(_) => {
                return true;
            }
        };
        evidence
            .checked_programs(seal)
            .is_some_and(|programs| programs.into_iter().all(|observed| &observed == program))
    }
    pub fn strictly_better_than(&self, other: &Self) -> bool {
        match self.assurance.cmp(&other.assurance) {
            Ordering::Greater => return true,
            Ordering::Less => return false,
            Ordering::Equal => {}
        }
        match self.robustness.compare(&other.robustness) {
            Some(Ordering::Greater) => return true,
            Some(Ordering::Equal) => {}
            _ => return false,
        }
        match (&self.call_cost, &other.call_cost) {
            (CallCostAssessment::Analysed(a), CallCostAssessment::Analysed(b))
                if a.behavior == b.behavior =>
            {
                match a.avoidable_calls.cmp(&b.avoidable_calls) {
                    Ordering::Less => return true,
                    Ordering::Greater => return false,
                    Ordering::Equal => {}
                }
            }
            _ => return false,
        }
        match self.teaching_tokens.cmp(&other.teaching_tokens) {
            Ordering::Less => return true,
            Ordering::Greater => return false,
            Ordering::Equal => {}
        }
        match (&self.readability, &other.readability) {
            (ReadabilityAssessment::Reviewed(a), ReadabilityAssessment::Reviewed(b)) => {
                a.grade > b.grade
            }
            _ => false,
        }
    }

    /// Only an identical canonical program under identical catalogue contracts
    /// permits teaching cost to break a tie without an independent cost analysis.
    pub(super) fn strictly_better_for_same_behavior(
        &self,
        other: &Self,
        program: &super::teaching::TeachingProgram,
        previous: &super::teaching::TeachingProgram,
    ) -> bool {
        if program.behavior_fingerprint() != previous.behavior_fingerprint()
            || program.catalog_fingerprints != previous.catalog_fingerprints
            || program.semantic_contracts != previous.semantic_contracts
            || program.required_capabilities != previous.required_capabilities
            || program.outputs != previous.outputs
            || self.assurance != other.assurance
        {
            return false;
        }
        let equally_observed = matches!(
            (&self.robustness, &other.robustness),
            (RobustnessAssessment::ObservedSuccess(a), RobustnessAssessment::ObservedSuccess(b)) if a.count == b.count
        );
        equally_observed
            && matches!(self.call_cost, CallCostAssessment::Unanalysed)
            && matches!(other.call_cost, CallCostAssessment::Unanalysed)
            && plasm_core::o200k_token_count(&program.source)
                < plasm_core::o200k_token_count(&previous.source)
    }

    /// Parameter bindings remain attached to their respective observed programs;
    /// the proof preserves the typed filter, executable structure and contracts.
    pub(super) fn strictly_better_for_parameterized_behavior(
        &self,
        other: &Self,
        program: &CheckedTeachingExample,
        previous: &CheckedTeachingExample,
    ) -> bool {
        let Some(proof) = super::parameterization::prove(previous, program) else {
            return false;
        };
        if self.assurance != other.assurance {
            return false;
        }
        let equally_observed = matches!(
            (&self.robustness, &other.robustness),
            (RobustnessAssessment::ObservedSuccess(a), RobustnessAssessment::ObservedSuccess(b)) if a.count == b.count
        );
        equally_observed
            && matches!(self.call_cost, CallCostAssessment::Unanalysed)
            && matches!(other.call_cost, CallCostAssessment::Unanalysed)
            && proof.replacement_teaching_tokens < proof.original_teaching_tokens
    }

    pub(super) fn equivalent_parameterized_observation(
        &self,
        other: &Self,
        program: &CheckedTeachingExample,
        previous: &CheckedTeachingExample,
    ) -> bool {
        let Some(proof) = super::parameterization::prove(previous, program) else {
            return false;
        };
        self.assurance == other.assurance
            && matches!((&self.robustness, &other.robustness),
                (RobustnessAssessment::ObservedSuccess(a), RobustnessAssessment::ObservedSuccess(b)) if a.count == b.count)
            && matches!(self.call_cost, CallCostAssessment::Unanalysed)
            && matches!(other.call_cost, CallCostAssessment::Unanalysed)
            && proof.same_presentation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_learning::model::{LibraryScope, ReviewRequest};
    use crate::workflow_learning::test_identities as ids;
    fn seal() -> (CheckedTeachingExample, SealedReview) {
        let program = crate::workflow_learning::teaching::tests::checked_fixture();
        let scope = LibraryScope {
            tenant: "fixture".into(),
            project: "quality".into(),
        };
        let receipt = ExecutionReceipt {
            scope: scope.clone(),
            logical_session: ids::logical("logical"),
            session_revision: 1,
            execution_id: ids::run("run"),
            plan_id: ids::plan("plan"),
            prompt_hash: ids::prompt("prompt"),
            execute_session: ids::execute("session"),
            source: program.program().source.clone(),
            teaching_plan: super::super::review_packet::TeachingPlan::Unavailable {
                reason: super::super::review_packet::TeachingPlanUnavailable::PlanningFailed,
            },

            compiled_plan: serde_json::json!({}),
            observation: program.observation(),
            catalogue_requirements: crate::workflow_learning::catalogue_guidance::CatalogueRequirementEvidence::Unavailable { diagnostic: "fixture".into() },
            catalog_fingerprints: program.program().catalog_fingerprints.clone(),
        };
        (
            program,
            SealedReview {
                request: ReviewRequest {
                    scope,
                    logical_session: ids::logical("logical"),
                    session_revision: 1,
                    intent: "inspect".into(),
                    subtasks: vec![],
                },
                executions: vec![receipt],
                assurance: OutcomeAssurance::AgentAttested,
            },
        )
    }
    #[test]
    fn output_artifacts_count_as_one_program_observation() {
        let (program, mut seal) = seal();
        let mut second = seal.executions[0].clone();
        second.execution_id = ids::run("other-output");
        seal.executions.push(second);
        let ids = vec![ids::run("run"), ids::run("other-output")];
        let quality = Quality::observed(&seal, &ids, 1).unwrap();
        let RobustnessAssessment::ObservedSuccess(observed) = &quality.robustness else {
            panic!("observation");
        };
        assert_eq!(observed.count().get(), 1);
        assert!(quality.verified_for(&program, &seal));
        seal.executions[1].plan_id = ids::plan("another-program-observation");
        let quality = Quality::observed(&seal, &ids, 1).unwrap();
        let RobustnessAssessment::ObservedSuccess(observed) = &quality.robustness else {
            panic!("observation");
        };
        assert_eq!(observed.count().get(), 2);
        seal.executions[1].plan_id = ids::plan("plan");
        seal.executions[1].source.push_str("# incompatible source");
        assert!(Quality::observed(&seal, &ids, 1).is_err());
    }

    #[test]
    fn differing_teaching_plans_cannot_share_an_observation() {
        let (_, mut seal) = seal();
        let mut other = seal.executions[0].clone();
        other.execution_id = ids::run("second-output");
        other.teaching_plan = super::super::review_packet::TeachingPlan::Available {
            text: "different plan".into(),
        };
        seal.executions.push(other);
        assert!(
            Quality::observed(&seal, &[ids::run("run"), ids::run("second-output")], 1).is_err()
        );
    }

    #[test]
    fn success_observation_has_nonzero_receipts_but_is_not_a_robustness_proof() {
        let (_, mut seal) = seal();
        let observed = Quality::observed(&seal, &[ids::run("run")], 10).unwrap();
        let RobustnessAssessment::ObservedSuccess(ref observation) = observed.robustness else {
            panic!("observed evidence")
        };
        assert_eq!(observation.count().get(), 1);
        assert!(observed.verified_by(&seal));
        let mut cheaper = observed.clone();
        cheaper.teaching_tokens = 0;
        assert!(!cheaper.strictly_better_than(&observed));
        assert!(Quality::observed(&seal, &[], 10).is_err());
        assert!(Quality::observed(&seal, &[ids::run("missing")], 10).is_err());
        seal.executions[0].source.push_str("# changed program");
        assert!(!observed.verified_by(&seal));
    }

    #[test]
    fn same_behavior_can_prefer_shorter_teaching_without_claiming_robustness() {
        let (program, original) = seal();
        let mut later = original.clone();
        later.executions[0].execution_id = ids::run("independent-observation");
        let old = Quality::observed(&original, &[ids::run("run")], 100).unwrap();
        let better = Quality::observed(&later, &[ids::run("independent-observation")], 50).unwrap();
        let mut longer = program.program().clone();
        longer.source = format!(
            "# A verbose explanation to be removed from the example\n{}",
            longer.source
        );
        assert!(better.strictly_better_for_same_behavior(&old, program.program(), &longer));
        assert!(!old.strictly_better_for_same_behavior(&better, &longer, program.program()));
        assert!(!better.strictly_better_than(&old));
        let mut changed = program.program().clone();
        changed.structural_fingerprint.push('x');
        assert!(!better.strictly_better_for_same_behavior(&old, &changed, program.program()));
        let mut changed_contract = program.program().clone();
        changed_contract
            .catalog_fingerprints
            .insert("other".into(), "changed".into());
        assert!(!better.strictly_better_for_same_behavior(
            &old,
            &changed_contract,
            program.program()
        ));
    }
    #[test]
    fn unsupported_persisted_cost_and_readability_reports_are_not_proof() {
        let (program, seal) = seal();
        let evidence = ExecutionEvidence::for_program(&seal, &program).unwrap();
        let mut quality = Quality::observed(&seal, &[ids::run("run")], 1).unwrap();
        quality.call_cost = CallCostAssessment::Analysed(CallCostAnalysis {
            avoidable_calls: AvoidableCallCount(0),
            behavior: program.program().behavior_fingerprint().into(),
            evidence: evidence.clone(),
        });
        let restored: Quality =
            serde_json::from_value(serde_json::to_value(&quality).unwrap()).unwrap();
        assert!(!restored.verified_by(&seal));
        quality.call_cost = CallCostAssessment::Unanalysed;
        quality.readability = ReadabilityAssessment::Reviewed(ReadabilityReview {
            grade: ReadabilityGrade::Clear,
            evidence,
        });
        assert!(!quality.verified_by(&seal));
    }
    #[test]
    fn assessments_cannot_borrow_another_programs_success_from_the_same_seal() {
        let (first, mut seal) = seal();
        let second = crate::workflow_learning::teaching::tests::checked_predicate_fixture();
        assert_ne!(
            first.program().behavior_fingerprint(),
            second.program().behavior_fingerprint()
        );
        let mut second_receipt = seal.executions[0].clone();
        second_receipt.execution_id = ids::run("second-run");
        second_receipt.plan_id = ids::plan("second-plan");
        second_receipt.source = second.program().source.clone();
        second_receipt.observation = second.observation();
        seal.executions.push(second_receipt);
        let observation = Quality::observed(&seal, &[ids::run("second-run")], 1).unwrap();
        assert!(observation.verified_for(&second, &seal));
        assert!(!observation.verified_for(&first, &seal));
        let mixed =
            Quality::observed(&seal, &[ids::run("run"), ids::run("second-run")], 1).unwrap();
        assert!(!mixed.verified_for(&first, &seal));
        assert!(!mixed.verified_for(&second, &seal));
    }
}
