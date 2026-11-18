//! Diagnostics when the current bounded packet has no relevant capabilities.
use crate::discovery_matcher::{CapabilityMatchReceipt, MatchChoice};
use crate::discovery_store::RetrievalReceipt;
use plasm_core::prerequisites::CapabilityRef;
use serde::{Deserialize, Serialize};

pub const RECOVERY_GUIDANCE: &str = "No relevant capability was selected from this bounded packet. Previously taught capabilities remain available. Extend the same logical_session_ref with the current discovery need. This result does not establish that the capability is absent from the catalog or that the task is complete.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CandidateJudgment {
    pub reference: CapabilityRef,
    pub choice: MatchChoice,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryRecovery {
    pub candidates: Vec<CandidateJudgment>,
    pub guidance: String,
}

impl DiscoveryRecovery {
    pub fn from_unmatched(matching: &CapabilityMatchReceipt, retrieval: &RetrievalReceipt) -> Self {
        Self {
            candidates: matching
                .matches
                .iter()
                .map(|m| CandidateJudgment {
                    reference: retrieval
                        .candidates
                        .iter()
                        .find(|c| c.id == m.capability_id)
                        .expect("validated relevance candidate")
                        .reference
                        .clone(),
                    choice: m.choice.clone(),
                })
                .collect(),
            guidance: RECOVERY_GUIDANCE.into(),
        }
    }

    pub fn render_unmatched_markdown(&self) -> String {
        let mut candidates: Vec<_> = self.candidates.iter().collect();
        candidates.sort_by(|a, b| a.reference.cmp(&b.reference));
        let mut lines = vec![self.guidance.clone()];
        for c in candidates.into_iter().take(3) {
            lines.push(format!(
                "- `{}/{}`: {:?}",
                c.reference.catalog, c.reference.capability, c.choice
            ));
        }
        lines.join("\n\n")
    }
}
