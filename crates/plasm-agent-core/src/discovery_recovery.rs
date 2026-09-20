//! Host-derived recovery after bounded capability-to-intent matching.
use crate::discovery_coverage::DiscoveryCoverage;
use crate::discovery_matcher::{CapabilityMatchReceipt, MatchChoice};
use crate::discovery_store::RetrievalReceipt;
use plasm_core::prerequisites::CapabilityRef;
use plasm_core::schema::CGS;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const RECOVERY_GUIDANCE: &str = "These slots remain unresolved; capability coverage is not task completion. Use available teaching for work whose inputs and intent constraints are satisfied. Previously taught capabilities remain available. Extend the same logical_session_ref for missing capabilities; inherited conditions and ordering still apply. No candidates means retrieval found none in this bounded packet. Rejected candidates were retrieved but judged not to match: inspect their documented constraints rather than repeatedly paraphrasing the same request. Uncertain judgments do not establish a match.";
const MAX_DESCRIPTION_BYTES: usize = 720;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CandidateJudgment {
    pub reference: CapabilityRef,
    pub choice: MatchChoice,
    pub direct_match_probability: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CatalogAppDescription {
    pub entry_id: String,
    pub description: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UnmatchedEffectSlot {
    pub slot_id: String,
    pub statement: String,
    pub candidates: Vec<CandidateJudgment>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryRecovery {
    pub unmatched_slots: Vec<UnmatchedEffectSlot>,
    pub available_catalogs: Vec<CatalogAppDescription>,
    pub guidance: String,
}

impl DiscoveryRecovery {
    pub fn from_unmatched(
        coverage: &DiscoveryCoverage,
        matching: &CapabilityMatchReceipt,
        retrieval: &RetrievalReceipt,
        catalogs: &BTreeMap<String, CGS>,
        allowed_catalog_ids: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        let unmatched_slots = coverage
            .unresolved()
            .map(|slot| UnmatchedEffectSlot {
                slot_id: slot.id.clone(),
                statement: slot.statement.clone(),
                candidates: matching
                    .matches
                    .iter()
                    .filter(|matched| matched.slot_id == slot.id)
                    .map(|matched| CandidateJudgment {
                        reference: retrieval
                            .candidates
                            .iter()
                            .find(|candidate| candidate.id == matched.capability_id)
                            .expect("validated match candidate")
                            .reference
                            .clone(),
                        choice: matched.choice.clone(),
                        direct_match_probability: matched.probabilities["direct_match"],
                    })
                    .collect(),
            })
            .collect();
        let allowed: BTreeSet<_> = allowed_catalog_ids
            .into_iter()
            .map(|id| id.as_ref().to_owned())
            .collect();
        let available_catalogs = catalogs
            .iter()
            .filter(|(id, _)| allowed.contains(id.as_str()))
            .map(|(id, cgs)| catalog_app_description(id, cgs))
            .collect();
        Self {
            unmatched_slots,
            available_catalogs,
            guidance: RECOVERY_GUIDANCE.into(),
        }
    }
    pub fn render_unmatched_markdown(&self) -> String {
        let mut lines = vec![
            "**Unresolved discovery slots** (matched capabilities can be taught independently)."
                .to_owned(),
        ];
        for slot in &self.unmatched_slots {
            lines.push(format!(
                "- No matching authorized retrieved capability for effect slot `{}`: {}",
                slot.slot_id, slot.statement
            ));
            let mut candidates: Vec<_> = slot.candidates.iter().collect();
            candidates.sort_by(|left, right| {
                right
                    .direct_match_probability
                    .total_cmp(&left.direct_match_probability)
                    .then(left.reference.cmp(&right.reference))
            });
            if candidates.is_empty() {
                lines.push("No authorized candidates retrieved in this packet.".into());
            } else {
                for candidate in candidates.iter().take(3) {
                    let judgment = match candidate.choice {
                        MatchChoice::DoesNotMatch => "rejected",
                        MatchChoice::Uncertain => "uncertain",
                        MatchChoice::DirectMatch => "matched",
                    };
                    lines.push(format!(
                        "  - `{}/{}` — {judgment} (match probability {:.2})",
                        candidate.reference.catalog,
                        candidate.reference.capability,
                        candidate.direct_match_probability
                    ));
                }
                if candidates.len() > 3 {
                    lines.push(format!(
                        "  - {} other judgments in the routing receipt.",
                        candidates.len() - 3
                    ));
                }
            }
        }
        lines.push(self.guidance.clone());
        lines.join("\n\n")
    }
}
fn catalog_app_description(entry_id: &str, cgs: &CGS) -> CatalogAppDescription {
    let mut description = format!(
        "Catalog {entry_id}: {} entities, {} capabilities",
        cgs.entities.len(),
        cgs.capabilities.len()
    );
    if description.len() > MAX_DESCRIPTION_BYTES {
        let mut end = MAX_DESCRIPTION_BYTES;
        while end > 0 && !description.is_char_boundary(end) {
            end -= 1;
        }
        description.truncate(end);
        description.push('…');
    }
    CatalogAppDescription {
        entry_id: entry_id.into(),
        description,
    }
}
