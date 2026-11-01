//! Native discovery consumes the same authorized library recall as HTTP/MCP.
use super::{
    model::{LibraryScope, OutcomeAssurance, StoredExample},
    teaching::TeachingOutput,
    WorkflowLearning,
};
use crate::discovery_store::DiscoveryAuthorization;
use plasm_core::prerequisites::CapabilityRef;
use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub enum ExampleExposure {
    AdmissionOnly,
    OptionalExamples,
}

#[derive(Debug, Serialize)]
pub struct ExampleSummary {
    pub example_ref: uuid::Uuid,
    pub purpose: String,
    pub applicability: Vec<String>,
    pub outputs: Vec<TeachingOutput>,
    pub assurance: OutcomeAssurance,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallDisposition {
    Completed,
    Unavailable,
    TimedOut,
}

pub struct ExampleGuidance {
    pub capabilities: Vec<CapabilityRef>,
    pub examples: Vec<ExampleSummary>,
    pub disposition: RecallDisposition,
}

impl ExampleGuidance {
    pub fn empty(disposition: RecallDisposition) -> Self {
        Self {
            capabilities: Vec::new(),
            examples: Vec::new(),
            disposition,
        }
    }

    fn from_authorized(
        examples: Vec<StoredExample>,
        associations: Vec<super::catalogue_guidance::CatalogueAssociation>,
        exposure: ExampleExposure,
    ) -> Self {
        let capabilities: std::collections::BTreeSet<_> = associations
            .iter()
            .flat_map(|association| {
                association
                    .requirements()
                    .required_capabilities()
                    .iter()
                    .cloned()
            })
            .collect();
        let mut summaries = Vec::new();
        for example in examples.into_iter().take(3) {
            let Some(program) = example.candidate.program() else {
                continue;
            };
            if matches!(exposure, ExampleExposure::OptionalExamples) {
                summaries.push(ExampleSummary {
                    example_ref: example.id,
                    purpose: example.candidate.intent.purpose.clone(),
                    applicability: example.candidate.intent.facets.clone(),
                    outputs: program.outputs.clone(),
                    assurance: example.candidate.quality.assurance,
                });
            }
        }
        Self {
            capabilities: capabilities.into_iter().collect(),
            examples: summaries,
            disposition: RecallDisposition::Completed,
        }
    }
}

/// Recall cannot bypass existing catalogue judgment, and optional infrastructure
/// failure cannot block normal discovery. No metadata is produced before scope
/// and capability authorization inside `WorkflowLearning::recall`.
pub async fn recall_guidance(
    learning: &WorkflowLearning,
    scope: &LibraryScope,
    intent: &str,
    allowed: &DiscoveryAuthorization,
    exposure: ExampleExposure,
    store: &crate::discovery_store::DiscoveryStore,
    generation: &str,
) -> ExampleGuidance {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let catalog_fingerprints = match store.load_discovery_generation(generation, allowed).await
        {
            Ok((catalogs, _)) => catalogs
                .into_iter()
                .map(|(id, cgs)| (id, cgs.effective_catalog_cgs_hash_hex()))
                .collect(),
            Err(error) => {
                tracing::warn!(%error,"catalogue guidance pins unavailable");
                return ExampleGuidance::empty(RecallDisposition::Unavailable);
            }
        };
        match learning
            .recall_with_guidance(scope, intent, allowed, &catalog_fingerprints)
            .await
        {
            Ok((examples, associations)) => {
                ExampleGuidance::from_authorized(examples, associations, exposure)
            }
            Err(error) => {
                tracing::warn!(%error,"native workflow recall unavailable");
                ExampleGuidance::empty(RecallDisposition::Unavailable)
            }
        }
    })
    .await
    .unwrap_or_else(|_| ExampleGuidance::empty(RecallDisposition::TimedOut))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_learning::{
        index::NormalizedIntent,
        model::{Candidate, CandidateState, Quality, VerifiedProgram},
    };

    fn example() -> StoredExample {
        let program = crate::workflow_learning::teaching::tests::checked_fixture();
        let catalogs = program.program().catalog_fingerprints.clone();
        StoredExample {
            id: uuid::Uuid::new_v4(),
            candidate: Candidate {
                family: "inspect".into(),
                variant: "matrix".into(),
                identity: "read".into(),
                intent: NormalizedIntent {
                    purpose: "inspect".into(),
                    facets: vec!["read only".into()],
                    aliases: vec![],
                },
                embedding: vec![],
                embedding_profile: "fixture".into(),
                coverage: vec!["read".into()],
                quality: Quality::unassessed(OutcomeAssurance::AgentAttested, 10),
                state: CandidateState::Verified {
                    evidence: VerifiedProgram::observed(program, OutcomeAssurance::AgentAttested),
                },
                catalog_fingerprints: catalogs,
                expected_family_version: 0,
            },
        }
    }

    #[test]
    fn admission_and_optional_examples_supply_the_same_judgment_evidence() {
        let examples = (0..4).map(|_| example()).collect::<Vec<_>>();
        let admission = ExampleGuidance::from_authorized(
            examples.clone(),
            Vec::new(),
            ExampleExposure::AdmissionOnly,
        );
        let optional = ExampleGuidance::from_authorized(
            examples,
            Vec::new(),
            ExampleExposure::OptionalExamples,
        );
        assert!(
            admission.capabilities.is_empty(),
            "examples alone do not manufacture catalogue associations"
        );
        assert_eq!(admission.capabilities, optional.capabilities);
        assert!(admission.examples.is_empty());
        assert_eq!(optional.examples.len(), 3);
        assert!(optional
            .examples
            .iter()
            .all(|example| example.purpose == "inspect"));
    }
}
