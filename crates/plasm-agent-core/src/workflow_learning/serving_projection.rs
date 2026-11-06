//! Minimal, claim-free evidence for selected archive products.
use super::{
    catalogue_guidance::CatalogueAssociation,
    filesystem_archive::SelectedReviewEvidence,
    model::{Candidate, ExecutionReceipt, SealedReview, StoredReviewSeal},
    repository::RepositoryError,
    teaching::CheckedTeachingExample,
};
use std::collections::BTreeSet;

/// Derive only evidence needed by the selected products. The original review
/// claims are intentionally not included in the portable projection.
pub(super) fn select_review_evidence(
    original: &SealedReview,
    candidates: &[Candidate],
    associations: &[CatalogueAssociation],
) -> Result<(SelectedReviewEvidence, Vec<ExecutionReceipt>), RepositoryError> {
    if candidates.is_empty() && associations.is_empty() {
        return Err(RepositoryError::InvalidEvidence("empty serving projection"));
    }
    let mut selected = BTreeSet::new();
    for candidate in candidates {
        if !candidate.verified_by(original) {
            return Err(RepositoryError::InvalidEvidence(
                "example is not supported by review",
            ));
        }
        let program = candidate
            .checked_program()
            .ok_or(RepositoryError::InvalidEvidence(
                "example program unavailable",
            ))?;
        for receipt in &original.executions {
            if CheckedTeachingExample::from_observation(&receipt.observation)
                .is_ok_and(|observed| &observed == program)
            {
                selected.insert(receipt.execution_id);
            }
        }
    }
    for association in associations {
        association.validate(original)?;
        selected.extend(association.evidence.keys().copied());
    }
    let executions: Vec<_> = original
        .executions
        .iter()
        .filter(|receipt| selected.contains(&receipt.execution_id))
        .cloned()
        .collect();
    if executions.is_empty() || executions.len() != selected.len() {
        return Err(RepositoryError::InvalidEvidence(
            "selected review evidence is incomplete",
        ));
    }
    // This is an ephemeral validation view, never a replacement for the
    // original review or a portable claim about what the agent observed.
    let projected = SealedReview {
        request: original.request.clone(),
        executions: executions.clone(),
        assurance: original.assurance,
    };
    if candidates
        .iter()
        .any(|candidate| !candidate.verified_by(&projected))
    {
        return Err(RepositoryError::InvalidEvidence(
            "selected example evidence is incomplete",
        ));
    }
    for association in associations {
        association.validate(&projected)?;
    }
    let references = StoredReviewSeal::from_review(&projected)?.executions;
    Ok((
        SelectedReviewEvidence {
            source_scope: original.request.scope.clone(),
            logical_session: original.request.logical_session,
            session_revision: original.request.session_revision,
            assurance: original.assurance,
            executions: references,
        },
        executions,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_learning::{
        index::NormalizedIntent,
        model::{
            AchievedSubtask, CandidateState, LibraryScope, OutcomeAssurance, Quality,
            ReviewRequest, VerifiedProgram,
        },
        review_packet::{TeachingPlan, TeachingPlanUnavailable},
        test_identities as ids,
    };

    #[test]
    fn projection_excludes_unselected_siblings_and_review_claims() {
        let chosen = super::super::teaching::tests::checked_fixture();
        let sibling = super::super::teaching::tests::checked_predicate_fixture();
        assert_ne!(chosen, sibling);
        let scope = LibraryScope {
            tenant: "tenant".into(),
            project: "project".into(),
        };
        let receipt = ExecutionReceipt {
            scope: scope.clone(),
            logical_session: ids::logical("session"),
            session_revision: 1,
            execution_id: ids::run("chosen"),
            plan_id: ids::plan("chosen"),
            prompt_hash: ids::prompt("prompt"),
            execute_session: ids::execute("session"),
            source: chosen.program().source.clone(),
            compiled_plan: serde_json::json!({}),
            teaching_plan: TeachingPlan::Unavailable {
                reason: TeachingPlanUnavailable::PlanningFailed,
            },
            observation: chosen.observation(),
            catalogue_requirements: crate::workflow_learning::catalogue_guidance::CatalogueRequirementEvidence::Unavailable { diagnostic: "fixture".into() },
            catalog_fingerprints: chosen.program().catalog_fingerprints.clone(),
        };
        let mut other = receipt.clone();
        other.execution_id = ids::run("sibling");
        other.plan_id = ids::plan("sibling");
        other.source = sibling.program().source.clone();
        other.observation = sibling.observation();
        other.catalog_fingerprints = sibling.program().catalog_fingerprints.clone();
        let original = SealedReview {
            request: ReviewRequest {
                scope,
                logical_session: receipt.logical_session,
                session_revision: 1,
                intent: "private sibling intent".into(),
                subtasks: vec![AchievedSubtask {
                    intent: "private sibling claim".into(),
                    observed_outcome: "private sibling outcome".into(),
                    execution_ids: vec![receipt.execution_id, other.execution_id],
                }],
            },
            executions: vec![receipt.clone(), other.clone()],
            assurance: OutcomeAssurance::AgentAttested,
        };
        let candidate = Candidate {
            intent: NormalizedIntent {
                purpose: "inspect".into(),
                facets: vec![],
                aliases: vec![],
            },
            embedding: vec![],
            embedding_profile: "fixture".into(),
            family: "family".into(),
            variant: "variant".into(),
            identity: "identity".into(),
            coverage: vec!["coverage".into()],
            quality: Quality::unassessed(OutcomeAssurance::AgentAttested, 1),
            state: CandidateState::Verified {
                evidence: VerifiedProgram::observed(
                    chosen.clone(),
                    OutcomeAssurance::AgentAttested,
                ),
            },
            catalog_fingerprints: chosen.program().catalog_fingerprints.clone(),
            expected_family_version: 0,
        };
        let (projection, selected) = select_review_evidence(&original, &[candidate], &[]).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].execution_id, receipt.execution_id);
        assert_eq!(projection.executions.len(), 1);
        let value = serde_json::to_string(&projection).unwrap();
        assert!(!value.contains("private sibling"));
        assert!(!value.contains(&other.execution_id.to_wire()));
    }
}
