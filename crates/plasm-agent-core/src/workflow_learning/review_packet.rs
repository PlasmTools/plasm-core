//! Semantic teaching input. Deterministic execution evidence stays in the receipt.
use super::{model::ExecutionReceipt, teaching::ProgramObservation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAX_PACKET_BYTES: usize = 64 * 1024;

/// Textual compiler plan captured with the program, independent of result blobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TeachingPlan {
    Available { text: String },
    Unavailable { reason: TeachingPlanUnavailable },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeachingPlanUnavailable {
    PlanningFailed,
}

impl TeachingPlan {
    pub(crate) async fn capture(
        session: &crate::execute_session::ExecuteSession,
        bundle: &crate::plasm_comp_bundle::PlasmCompBundle,
    ) -> Self {
        match crate::plasm_plan_run::evaluate_plasm_comp_dry_snapshot(session, bundle).await {
            Ok(dry) => Self::Available {
                text: crate::plan_dry_display::render_teaching_plan(
                    &crate::plasm_plan_run::plan_dry_compact_view(&dry, Some(session)),
                ),
            },
            Err(_) => Self::Unavailable {
                reason: TeachingPlanUnavailable::PlanningFailed,
            },
        }
    }

    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::Available { text } => Some(text),
            Self::Unavailable { .. } => None,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReviewProgram<'a> {
    Available {
        source: &'a str,
        plan: ReviewPlan<'a>,
        symbols: Vec<String>,
        capabilities: Vec<String>,
    },
    Unavailable {
        reason: &'static str,
    },
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReviewPlan<'a> {
    Available { text: &'a str },
    Unavailable { reason: &'static str },
}

#[derive(Serialize)]
pub struct ReviewPacket<'a> {
    pub version: u8,
    pub execution_id: &'a super::model::ExecutionId,
    pub program: ReviewProgram<'a>,
}

impl<'a> ReviewPacket<'a> {
    pub fn from_receipt(receipt: &'a ExecutionReceipt) -> Self {
        Self::with_plan(receipt, receipt.teaching_plan.text())
    }

    pub fn with_plan(receipt: &'a ExecutionReceipt, plan: Option<&'a str>) -> Self {
        let program = match (
            &receipt.observation,
            receipt.catalogue_requirements.available(),
        ) {
            (ProgramObservation::Available(observed), _)
                if observed.program().source != receipt.source =>
            {
                ReviewProgram::Unavailable {
                    reason: "source_mismatch",
                }
            }
            (ProgramObservation::Available(observed), _) if observed.model_source().is_none() => {
                ReviewProgram::Unavailable {
                    reason: "sensitive_source",
                }
            }
            (ProgramObservation::Available(_), Some(requirements))
                if requirements.source_sha256()
                    != plasm_core::catalog_discovery::content_hash(receipt.source.as_bytes()) =>
            {
                ReviewProgram::Unavailable {
                    reason: "catalogue_source_mismatch",
                }
            }
            (ProgramObservation::Available(observed), Some(requirements)) => {
                let distinct: BTreeSet<_> = observed
                    .program()
                    .occurrences
                    .iter()
                    .map(|occurrence| (occurrence.expected.as_str(), &occurrence.reference))
                    .collect();
                ReviewProgram::Available {
                    source: observed.program().source.as_str(),
                    plan: match plan {
                        Some(text) => ReviewPlan::Available { text },
                        None => ReviewPlan::Unavailable {
                            reason: "plan_not_available",
                        },
                    },
                    symbols: distinct
                        .into_iter()
                        .map(|(symbol, reference)| format!("{symbol}: {reference}"))
                        .collect(),
                    capabilities: requirements
                        .capability_contracts()
                        .iter()
                        .map(|contract| contract.teaching_text())
                        .collect(),
                }
            }
            (ProgramObservation::Available(_), None) => ReviewProgram::Unavailable {
                reason: "missing_catalogue_evidence",
            },
            (ProgramObservation::Unavailable(_), _) => ReviewProgram::Unavailable {
                reason: "missing_compiler_evidence",
            },
        };
        Self {
            version: 4,
            execution_id: &receipt.execution_id,
            program,
        }
    }

    pub fn encode(&self) -> Result<Option<serde_json::Value>, serde_json::Error> {
        let value = serde_json::to_value(self)?;
        Ok((serde_json::to_vec(&value)?.len() <= MAX_PACKET_BYTES).then_some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_learning::{
        catalogue_guidance::CatalogueRequirementEvidence, model::LibraryScope,
        test_identities as ids,
    };
    #[test]
    fn semantic_packet_excludes_all_execution_and_scoring_evidence() {
        let example = crate::workflow_learning::teaching::tests::checked_fixture();
        let requirements = serde_json::from_value(serde_json::json!({"capability_contracts":[],"dependencies":[],"required_capabilities":[],"catalog_fingerprints":{},"source_sha256":plasm_core::catalog_discovery::content_hash(example.program().source.as_bytes()),"compiled_sha256":"fixture"})).unwrap();
        let receipt = ExecutionReceipt {
            scope: LibraryScope {
                tenant: "fixture".into(),
                project: "fixture".into(),
            },
            logical_session: ids::logical("session"),
            session_revision: 1,
            execution_id: ids::run("run"),
            plan_id: ids::plan("plan"),
            prompt_hash: ids::prompt("prompt"),
            execute_session: ids::execute("execution"),
            source: example.program().source.clone(),
            compiled_plan: serde_json::json!({"plan_marker":"MUST_NOT_REACH_MODEL"}),
            teaching_plan: TeachingPlan::Unavailable {
                reason: TeachingPlanUnavailable::PlanningFailed,
            },
            observation: example.observation(),
            catalogue_requirements: CatalogueRequirementEvidence::Available(requirements),
            catalog_fingerprints: Default::default(),
        };
        let value = ReviewPacket::from_receipt(&receipt)
            .encode()
            .unwrap()
            .unwrap();
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../fixtures/workflow-review-packet.json"
        ))
        .unwrap();
        assert_eq!(value, contract);
        assert_eq!(value["program"]["state"], "available");
        assert_eq!(value["program"]["source"], receipt.source);
        assert_eq!(value["program"]["plan"]["state"], "unavailable");
        let with_plan = ReviewPacket::with_plan(&receipt, Some("read: query Product\n"))
            .encode()
            .unwrap()
            .unwrap();
        assert_eq!(
            with_plan["program"]["plan"]["text"],
            "read: query Product\n"
        );
        assert!(!value.to_string().contains("MUST_NOT_REACH_MODEL"));
        assert!(value.get("result").is_none());
        assert!(value.get("session_revision").is_none());
        assert!(value.get("plan_id").is_none());
        assert!(!value.to_string().contains("catalog_fingerprints"));
        assert!(!value.to_string().contains("compiled_plan"));
        assert!(!value.to_string().contains("source_sha256"));
        let mut mismatch = receipt.clone();
        mismatch.source.push(' ');
        assert_eq!(
            ReviewPacket::from_receipt(&mismatch)
                .encode()
                .unwrap()
                .unwrap()["program"]["reason"],
            "source_mismatch"
        );
        mismatch.source = receipt.source.clone();
        let mut changed = serde_json::to_value(&receipt.catalogue_requirements).unwrap();
        changed["evidence"]["source_sha256"] = "wrong".into();
        mismatch.catalogue_requirements = serde_json::from_value(changed).unwrap();
        assert_eq!(
            ReviewPacket::from_receipt(&mismatch)
                .encode()
                .unwrap()
                .unwrap()["program"]["reason"],
            "catalogue_source_mismatch"
        );
    }
}
