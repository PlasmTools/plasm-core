//! Host-derived recovery after bounded capability-to-intent matching.
use crate::discovery_matcher::CapabilityMatchReceipt;
use plasm_core::schema::CGS;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const RECOVERY_GUIDANCE: &str = "No matching authorized retrieved capability was found for every affirmative effect slot above. The host withheld the entire candidate teaching set: partial slot coverage is not readiness. This is a bounded packet result, not proof that the wider task is impossible. Widen discovery with plasm_context session_mode extend and the same logical_session_ref when a session exists, or session_mode new only when none exists.";
const MAX_DESCRIPTION_BYTES: usize = 720;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatalogAppDescription {
    pub entry_id: String,
    pub description: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UnmatchedEffectSlot {
    pub slot_id: String,
    pub statement: String,
    pub admitted_candidate_ids: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryRecovery {
    pub unmatched_slots: Vec<UnmatchedEffectSlot>,
    pub available_catalogs: Vec<CatalogAppDescription>,
    pub guidance: String,
}

impl DiscoveryRecovery {
    pub fn from_unmatched(
        matching: &CapabilityMatchReceipt,
        catalogs: &BTreeMap<String, CGS>,
        allowed_catalog_ids: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        let unmatched: BTreeSet<_> = matching.unmatched_slot_ids.iter().collect();
        let unmatched_slots = matching
            .slots
            .iter()
            .filter(|slot| unmatched.contains(&slot.id))
            .map(|slot| UnmatchedEffectSlot {
                slot_id: slot.id.clone(),
                statement: slot.statement.clone(),
                admitted_candidate_ids: matching
                    .matches
                    .iter()
                    .filter(|matched| matched.slot_id == slot.id)
                    .map(|matched| matched.capability_id.clone())
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
            "**Capability matching is insufficient** (no partial teaching was published)."
                .to_owned(),
        ];
        for slot in &self.unmatched_slots {
            lines.push(format!(
                "- No matching authorized retrieved capability for effect slot `{}`: {}",
                slot.slot_id, slot.statement
            ));
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
