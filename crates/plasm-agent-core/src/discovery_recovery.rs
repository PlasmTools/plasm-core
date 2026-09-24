//! Diagnostics when the current bounded packet has no relevant capabilities.
use crate::discovery_matcher::{CapabilityMatchReceipt, MatchChoice};
use crate::discovery_store::RetrievalReceipt;
use plasm_core::{prerequisites::CapabilityRef, schema::CGS};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub const RECOVERY_GUIDANCE:&str="No relevant capability was selected from this bounded packet. Previously taught capabilities remain available. Extend the same logical_session_ref with the current discovery need. This result does not establish that the capability is absent from the catalog or that the task is complete.";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CandidateJudgment {
    pub reference: CapabilityRef,
    pub choice: MatchChoice,
    pub relevance_probability: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CatalogAppDescription {
    pub entry_id: String,
    pub description: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryRecovery {
    pub candidates: Vec<CandidateJudgment>,
    pub available_catalogs: Vec<CatalogAppDescription>,
    pub guidance: String,
}
impl DiscoveryRecovery {
    pub fn from_unmatched(
        matching: &CapabilityMatchReceipt,
        retrieval: &RetrievalReceipt,
        catalogs: &BTreeMap<String, CGS>,
        allowed_catalog_ids: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        let allowed: BTreeSet<_> = allowed_catalog_ids
            .into_iter()
            .map(|id| id.as_ref().to_owned())
            .collect();
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
                    relevance_probability: m.probabilities["relevant"],
                })
                .collect(),
            available_catalogs: catalogs
                .iter()
                .filter(|(id, _)| allowed.contains(id.as_str()))
                .map(|(id, cgs)| catalog_app_description(id, cgs))
                .collect(),
            guidance: RECOVERY_GUIDANCE.into(),
        }
    }
    pub fn render_unmatched_markdown(&self) -> String {
        let mut candidates: Vec<_> = self.candidates.iter().collect();
        candidates.sort_by(|a, b| {
            b.relevance_probability
                .total_cmp(&a.relevance_probability)
                .then(a.reference.cmp(&b.reference))
        });
        let mut lines = vec![self.guidance.clone()];
        for c in candidates.into_iter().take(3) {
            lines.push(format!(
                "- `{}/{}`: {:?} (relevance {:.2})",
                c.reference.catalog, c.reference.capability, c.choice, c.relevance_probability
            ));
        }
        lines.join("\n\n")
    }
}
fn catalog_app_description(entry_id: &str, cgs: &CGS) -> CatalogAppDescription {
    CatalogAppDescription {
        entry_id: entry_id.into(),
        description: format!(
            "Catalog {entry_id}: {} entities, {} capabilities",
            cgs.entities.len(),
            cgs.capabilities.len()
        ),
    }
}
