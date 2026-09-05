//! Hard identity-pair closure: session_primary (password) ⇒ federated_primary (principal).
//!
//! Single canonical admit path for both the closed witness shortlist ([`crate::discovery_seed_witness::corpus`])
//! and teaching satellites. Password without principal is forbidden.

use super::corpus::{RequirementWitness, WitnessKind};
use std::collections::HashSet;

/// DirectCapability key `(entry_id, entity, capability_id)`.
pub(crate) fn direct_cap_key(w: &RequirementWitness) -> Option<(String, String, String)> {
    match &w.kind {
        WitnessKind::DirectCapability {
            entry_id,
            entity,
            capability_id,
            ..
        } => Some((entry_id.clone(), entity.clone(), capability_id.clone())),
        _ => None,
    }
}

fn relation_hop_key(w: &RequirementWitness) -> Option<(String, String, String)> {
    match &w.kind {
        WitnessKind::RelationHop {
            entry_id,
            from_entity,
            wire,
            target_entity,
        } => Some((
            entry_id.clone(),
            from_entity.clone(),
            format!("{wire}->{target_entity}"),
        )),
        _ => None,
    }
}

/// Catalogs that currently hold a `session_primary` DirectCapability seat.
pub(crate) fn session_primary_catalogs<'a>(
    seats: impl Iterator<Item = &'a RequirementWitness>,
) -> HashSet<String> {
    seats
        .filter(|w| w.co_seed_with.is_session_primary_seat())
        .filter_map(|w| match &w.kind {
            WitnessKind::DirectCapability { entry_id, .. } => Some(entry_id.clone()),
            _ => None,
        })
        .collect()
}

/// Pin `federated_primary` / `session_primary` DirectCapabilities into the closed
/// set before `max_witnesses` truncate, then fill remaining slots by lexical rank.
pub(crate) fn pin_identity_seats_then_truncate(
    ranked: &[RequirementWitness],
    max_witnesses: usize,
) -> Vec<RequirementWitness> {
    let mut shortlist: Vec<RequirementWitness> = Vec::new();
    let mut have: HashSet<(String, String, String)> = HashSet::new();

    for w in ranked {
        if !w.co_seed_with.admits_on_federated_primary() {
            continue;
        }
        let Some(key) = direct_cap_key(w) else {
            continue;
        };
        if !have.insert(key) {
            continue;
        }
        shortlist.push(w.clone());
    }

    for w in ranked {
        if shortlist.len() >= max_witnesses {
            break;
        }
        let Some(key) = direct_cap_key(w).or_else(|| relation_hop_key(w)) else {
            continue;
        };
        if !have.insert(key) {
            continue;
        }
        shortlist.push(w.clone());
    }
    shortlist
}

/// Password seat without principal is forbidden: whenever a `session_primary`
/// DirectCapability is in the closed set, force-admit same-catalog
/// `federated_primary` siblings from the ranked pool.
pub(crate) fn admit_identity_pair_siblings(
    shortlist: &mut Vec<RequirementWitness>,
    ranked: &[RequirementWitness],
) {
    let session_catalogs = session_primary_catalogs(shortlist.iter());
    if session_catalogs.is_empty() {
        return;
    }

    let mut have: HashSet<(String, String, String)> = HashSet::new();
    for w in shortlist.iter() {
        if let Some(key) = direct_cap_key(w) {
            have.insert(key);
        }
    }

    for w in ranked {
        if !w.co_seed_with.is_federated_principal_seat() {
            continue;
        }
        let WitnessKind::DirectCapability {
            entry_id,
            entity,
            capability_id,
            ..
        } = &w.kind
        else {
            continue;
        };
        if !session_catalogs.contains(entry_id) {
            continue;
        }
        let key = (entry_id.clone(), entity.clone(), capability_id.clone());
        if !have.insert(key) {
            continue;
        }
        shortlist.push(w.clone());
    }
}

/// Teaching-side hard pair: given catalogs that already seat `session_primary`,
/// insert same-catalog `federated_primary` `(entry_id, entity)` pairs from `pool`
/// that are not already in `already_seeded`.
pub(crate) fn admit_identity_pair_entity_siblings(
    session_catalogs: &HashSet<String>,
    pool: &[RequirementWitness],
    already_seeded: &HashSet<(&str, &str)>,
    out: &mut std::collections::BTreeSet<(String, String)>,
) {
    if session_catalogs.is_empty() {
        return;
    }
    for w in pool {
        if !w.co_seed_with.is_federated_principal_seat() {
            continue;
        }
        let WitnessKind::DirectCapability {
            entry_id, entity, ..
        } = &w.kind
        else {
            continue;
        };
        if !session_catalogs.contains(entry_id) {
            continue;
        }
        if already_seeded.contains(&(entry_id.as_str(), entity.as_str())) {
            continue;
        }
        out.insert((entry_id.clone(), entity.clone()));
    }
}

/// Resolve DirectCapability witnesses for `(entry_id, entity)` seats and collect
/// catalogs that hold a `session_primary` stamp.
pub(crate) fn session_primary_catalogs_for_entity_seats<'a>(
    pool: &[RequirementWitness],
    seats: impl Iterator<Item = (&'a str, &'a str)>,
) -> HashSet<String> {
    let mut catalogs = HashSet::new();
    for (entry_id, entity) in seats {
        if let Some(w) = pool.iter().find(|w| {
            matches!(
                &w.kind,
                WitnessKind::DirectCapability {
                    entry_id: e,
                    entity: ent,
                    ..
                } if e == entry_id && ent == entity
            )
        }) {
            if w.co_seed_with.is_session_primary_seat() {
                catalogs.insert(entry_id.to_string());
            }
        }
    }
    catalogs
}
