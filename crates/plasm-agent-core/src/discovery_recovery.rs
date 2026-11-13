//! Diagnostics when the current bounded packet has no relevant capabilities.
use crate::discovery_matcher::{CapabilityMatchReceipt, MatchChoice};
use crate::discovery_store::RetrievalReceipt;
use plasm_core::{prerequisites::CapabilityRef, schema::CGS};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub const RECOVERY_GUIDANCE: &str = "No relevant capability was selected from this bounded packet. Previously taught capabilities remain available. Extend the same logical_session_ref with the current discovery need. This result does not establish that the capability is absent from the catalog or that the task is complete.";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CandidateJudgment {
    pub reference: CapabilityRef,
    pub choice: MatchChoice,
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
fn catalog_app_description(entry_id: &str, cgs: &CGS) -> CatalogAppDescription {
    let examples = cgs
        .entities
        .values()
        .filter(|entity| !entity.description.trim().is_empty())
        .take(4)
        .map(|entity| {
            let description = entity.description.trim();
            let mut chars = description.chars();
            let excerpt: String = chars.by_ref().take(200).collect();
            let suffix = if chars.next().is_some() { "…" } else { "" };
            format!("{}: {excerpt}{suffix}", entity.name)
        })
        .collect::<Vec<_>>()
        .join("; ");
    CatalogAppDescription {
        entry_id: entry_id.into(),
        description: if examples.is_empty() {
            "No entity descriptions supplied.".into()
        } else {
            examples
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_recovery_only_describes_authorized_catalogs() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/discovery/partial-routing.json"
        ))
        .unwrap();
        let routing: crate::discovery_service::RoutingReceipt =
            serde_json::from_value(fixture["routing"].clone()).unwrap();
        let cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/plasm_prompt_matrix"),
        )
        .unwrap();
        let catalogs = BTreeMap::from([("allowed".into(), cgs.clone()), ("denied".into(), cgs)]);
        let matching = CapabilityMatchReceipt {
            matches: Vec::new(),
            budget_retained: Vec::new(),
        };
        let recovery = DiscoveryRecovery::from_unmatched(
            &matching,
            &routing.retrieval,
            &catalogs,
            ["allowed"],
        );
        assert_eq!(recovery.available_catalogs.len(), 1);
        assert_eq!(recovery.available_catalogs[0].entry_id, "allowed");
        assert!(!recovery.available_catalogs[0]
            .description
            .contains("entities, "));
    }
}
