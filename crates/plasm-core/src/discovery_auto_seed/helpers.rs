use std::sync::Arc;

use indexmap::IndexMap;

use crate::discovery::{EntitySummary, RankedCandidate};
use crate::schema::CGS;

use super::types::{EntityCandidateBundle, EntityCapabilityEvidence};

pub(crate) type ArcCgs = Arc<CGS>;

pub(crate) fn capability_id(entry_id: &str, entity: &str, capability_name: &str) -> String {
    format!("{entry_id}:{entity}:{capability_name}")
}

pub(crate) fn candidate_id(entry_id: &str, entity: &str) -> String {
    format!("{entry_id}:{entity}")
}

pub(crate) fn entity_description_for(
    summaries: &[EntitySummary],
    entry_id: &str,
    entity: &str,
    cgs: Option<&CGS>,
) -> String {
    if let Some(s) = summaries
        .iter()
        .find(|s| s.entry_id == entry_id && s.name == entity)
    {
        return s.description.clone();
    }
    cgs.and_then(|g| g.get_entity(entity))
        .map(|e| e.description.clone())
        .unwrap_or_default()
}

pub(crate) fn capability_kind_label(
    catalogs: &IndexMap<String, ArcCgs>,
    entry_id: &str,
    capability_name: &str,
) -> String {
    let Some(cgs) = catalogs.get(entry_id) else {
        return String::new();
    };
    cgs.capabilities
        .get(capability_name)
        .map(|c| format!("{:?}", c.kind))
        .unwrap_or_default()
}

pub(crate) fn push_capability_evidence(
    entry: &mut EntityCandidateBundle,
    cand: &RankedCandidate,
    catalogs: &IndexMap<String, ArcCgs>,
    max_capabilities_per_entity: usize,
) {
    if entry.capabilities.len() >= max_capabilities_per_entity {
        return;
    }
    let cap_id = capability_id(&cand.entry_id, &cand.entity, &cand.capability_name);
    if entry.capabilities.iter().any(|c| c.capability_id == cap_id) {
        return;
    }
    entry.capabilities.push(EntityCapabilityEvidence {
        capability_id: cap_id,
        capability_name: cand.capability_name.clone(),
        kind: capability_kind_label(catalogs, &cand.entry_id, &cand.capability_name),
        description: cand.capability_description.clone(),
        reason_codes: cand.reason_codes.clone(),
        lexical_score: cand.score,
    });
}
