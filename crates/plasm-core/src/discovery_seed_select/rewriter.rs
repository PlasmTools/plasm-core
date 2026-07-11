//! Bundle and candidate-id rewrite rules (single correction layer).

use std::collections::{HashMap, HashSet};

use crate::discovery_intent_signals as signals;

use super::index::SeedBundleIndexTables;
use super::validation::SeedSelectionValidationError;

fn support_union(support_by_requirement: &[HashSet<usize>]) -> HashSet<usize> {
    support_by_requirement
        .iter()
        .flat_map(|support| support.iter().copied())
        .collect()
}

fn prefer_minimal_subset_bundle(
    bundle_index: usize,
    _support: &HashSet<usize>,
    tables: &SeedBundleIndexTables,
) -> usize {
    let Some(provider) = tables.provider_index_for_bundle(bundle_index) else {
        return bundle_index;
    };
    let current_ids: HashSet<&String> = tables.candidate_ids_by_bundle[bundle_index]
        .iter()
        .collect();
    let mut best = bundle_index;
    let mut best_roots = tables.bundle_root_count(bundle_index).unwrap_or(usize::MAX);
    for other in 0..tables.bundle_count() {
        if other == bundle_index || tables.provider_index_for_bundle(other) != Some(provider) {
            continue;
        }
        let other_roots = tables.bundle_root_count(other).unwrap_or(usize::MAX);
        if other_roots >= best_roots {
            continue;
        }
        let other_ids: HashSet<&String> = tables.candidate_ids_by_bundle[other].iter().collect();
        if !other_ids.is_empty() && other_ids.is_subset(&current_ids) {
            best = other;
            best_roots = other_roots;
        }
    }
    best
}
fn relation_hint_covers_entity(hints: &str, entity: &str) -> bool {
    hints.contains(&format!("→{entity}"))
}

fn prefer_relation_nav_root(
    bundle_index: usize,
    requirement_texts: &[String],
    tables: &SeedBundleIndexTables,
) -> usize {
    if requirement_texts.iter().any(|text| {
        signals::requirement_implies_mutation(text)
            || signals::requirement_implies_create_on_related(text)
    }) {
        return bundle_index;
    }
    let Some(provider) = tables.provider_index_for_bundle(bundle_index) else {
        return bundle_index;
    };
    for sel_id in &tables.candidate_ids_by_bundle[bundle_index] {
        let Some((_, sel_entity)) = tables.entry_entity_by_candidate_id.get(sel_id) else {
            continue;
        };
        for other in 0..tables.bundle_count() {
            if tables.provider_index_for_bundle(other) != Some(provider) {
                continue;
            }
            if tables.bundle_root_count(other) != Some(1) {
                continue;
            }
            let Some(other_id) = tables.candidate_ids_by_bundle[other].first() else {
                continue;
            };
            if other_id == sel_id {
                continue;
            }
            let Some(hints) = tables.relation_hints_by_candidate_id.get(other_id) else {
                continue;
            };
            if relation_hint_covers_entity(hints, sel_entity) {
                return other;
            }
        }
    }
    bundle_index
}

pub(crate) fn bundle_mutation_score(tables: &SeedBundleIndexTables, bundle_index: usize) -> usize {
    tables
        .capability_kinds_by_bundle
        .get(bundle_index)
        .map(|kinds| {
            kinds
                .iter()
                .filter(|kind| {
                    matches!(
                        kind.as_str(),
                        "Action" | "Create" | "Update" | "Delete" | "Transition"
                    )
                })
                .count()
        })
        .unwrap_or(0)
}

pub(crate) fn prefer_mutation_capable_singleton(
    bundle_index: usize,
    requirement_texts: &[String],
    tables: &SeedBundleIndexTables,
) -> usize {
    if !requirement_texts
        .iter()
        .any(|text| signals::requirement_implies_mutation(text))
    {
        return bundle_index;
    }
    let Some(provider) = tables.provider_index_for_bundle(bundle_index) else {
        return bundle_index;
    };
    let mut best = bundle_index;
    let mut best_score = bundle_mutation_score(tables, bundle_index);
    for other in 0..tables.bundle_count() {
        if tables.provider_index_for_bundle(other) != Some(provider) {
            continue;
        }
        if tables.bundle_root_count(other) != Some(1) {
            continue;
        }
        let mutation = bundle_mutation_score(tables, other);
        if mutation == 0 {
            continue;
        }
        let mention = tables.candidate_ids_by_bundle[other]
            .first()
            .and_then(|id| tables.entry_entity_by_candidate_id.get(id))
            .map(|(_, entity)| {
                requirement_texts
                    .iter()
                    .map(|text| entity_mention_score(text, entity))
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        let score = mutation * 10 + mention;
        if tables.candidate_ids_by_bundle[other]
            .first()
            .and_then(|id| tables.entry_entity_by_candidate_id.get(id))
            .map(|(_, entity)| signals::is_auxiliary_entity_for_mutation(entity))
            .unwrap_or(false)
        {
            continue;
        }
        if score > best_score {
            best_score = score;
            best = other;
        }
    }
    best
}

fn candidate_id_for_entity(
    entry_id: &str,
    entity: &str,
    tables: &SeedBundleIndexTables,
) -> Option<String> {
    tables
        .entry_entity_by_candidate_id
        .iter()
        .find(|(_, (eid, ent))| eid == entry_id && ent == entity)
        .map(|(id, _)| id.clone())
}

fn replace_auxiliary_candidate_ids(
    selected_ids: Vec<String>,
    requirement_texts: &[String],
    intent: &str,
    tables: &SeedBundleIndexTables,
) -> Vec<String> {
    let combined = format!("{} {}", requirement_texts.join(" "), intent);
    let wants_share = signals::requirement_implies_share_or_access(&combined);
    let wants_mutation = requirement_texts
        .iter()
        .any(|text| signals::requirement_implies_mutation(text))
        || signals::requirement_implies_mutation(&combined);
    selected_ids
        .into_iter()
        .map(|id| {
            let Some((entry_id, entity)) = tables.entry_entity_by_candidate_id.get(&id) else {
                return id;
            };
            if wants_share {
                for preferred in ["Page", "Document"] {
                    if entity == preferred {
                        return id;
                    }
                    if let Some(replacement) = candidate_id_for_entity(entry_id, preferred, tables)
                    {
                        return replacement;
                    }
                }
                if entity == "Database" {
                    if let Some(replacement) = candidate_id_for_entity(entry_id, "Page", tables) {
                        return replacement;
                    }
                }
            }
            if wants_mutation && signals::is_auxiliary_entity_for_mutation(entity) {
                for preferred in [
                    "Issue",
                    "Transition",
                    "Dashboard",
                    "PullRequest",
                    "MergeRequest",
                ] {
                    if let Some(replacement) = candidate_id_for_entity(entry_id, preferred, tables)
                    {
                        return replacement;
                    }
                }
            }
            id
        })
        .collect()
}

fn prefer_workflow_entity_over_auxiliary(
    bundle_index: usize,
    requirement_texts: &[String],
    intent: &str,
    tables: &SeedBundleIndexTables,
) -> usize {
    let combined = format!("{} {}", requirement_texts.join(" "), intent);
    if !requirement_texts
        .iter()
        .any(|text| signals::requirement_implies_mutation(text))
        && !signals::requirement_implies_mutation(&combined)
    {
        return bundle_index;
    }
    let Some(current_id) = tables.candidate_ids_by_bundle[bundle_index].first() else {
        return bundle_index;
    };
    let Some((entry_id, entity)) = tables.entry_entity_by_candidate_id.get(current_id) else {
        return bundle_index;
    };
    if !signals::is_auxiliary_entity_for_mutation(entity) {
        return bundle_index;
    }
    let Some(provider) = tables.provider_index_for_bundle(bundle_index) else {
        return bundle_index;
    };
    for preferred in [
        "Issue",
        "Transition",
        "Dashboard",
        "PullRequest",
        "MergeRequest",
    ] {
        for other in 0..tables.bundle_count() {
            if tables.provider_index_for_bundle(other) != Some(provider) {
                continue;
            }
            if tables.bundle_root_count(other) != Some(1) {
                continue;
            }
            let Some(other_id) = tables.candidate_ids_by_bundle[other].first() else {
                continue;
            };
            let Some((other_entry, other_entity)) =
                tables.entry_entity_by_candidate_id.get(other_id)
            else {
                continue;
            };
            if other_entry != entry_id || other_entity != preferred {
                continue;
            }
            if bundle_mutation_score(tables, other) > 0 {
                return other;
            }
        }
    }
    bundle_index
}

fn prefer_share_publish_target(
    bundle_index: usize,
    requirement_texts: &[String],
    intent: &str,
    tables: &SeedBundleIndexTables,
) -> usize {
    let combined = format!("{} {}", requirement_texts.join(" "), intent);
    if !signals::requirement_implies_share_or_access(&combined) {
        return bundle_index;
    }
    let Some(provider) = tables.provider_index_for_bundle(bundle_index) else {
        return bundle_index;
    };
    for preferred in ["Page", "Document"] {
        for other in 0..tables.bundle_count() {
            if tables.provider_index_for_bundle(other) != Some(provider) {
                continue;
            }
            if tables.bundle_root_count(other) != Some(1) {
                continue;
            }
            let Some(other_id) = tables.candidate_ids_by_bundle[other].first() else {
                continue;
            };
            let Some((_, other_entity)) = tables.entry_entity_by_candidate_id.get(other_id) else {
                continue;
            };
            if other_entity == preferred && bundle_mutation_score(tables, other) > 0 {
                return other;
            }
        }
    }
    bundle_index
}

fn prefer_create_target_entity(
    bundle_index: usize,
    requirement_texts: &[String],
    tables: &SeedBundleIndexTables,
) -> usize {
    if !requirement_texts
        .iter()
        .any(|text| signals::requirement_implies_create_on_related(text))
    {
        return bundle_index;
    }
    let lower = requirement_texts.join(" ").to_lowercase();
    if !lower.contains("comment") {
        return bundle_index;
    }
    let Some(provider) = tables.provider_index_for_bundle(bundle_index) else {
        return bundle_index;
    };
    for other in 0..tables.bundle_count() {
        if tables.provider_index_for_bundle(other) != Some(provider) {
            continue;
        }
        if tables.bundle_root_count(other) != Some(1) {
            continue;
        }
        let Some(other_id) = tables.candidate_ids_by_bundle[other].first() else {
            continue;
        };
        let Some((_, other_entity)) = tables.entry_entity_by_candidate_id.get(other_id) else {
            continue;
        };
        if other_entity.contains("Comment") && bundle_mutation_score(tables, other) > 0 {
            return other;
        }
    }
    bundle_index
}

fn prefer_parent_entity_for_relation_read(
    bundle_index: usize,
    tables: &SeedBundleIndexTables,
) -> usize {
    let Some(current_id) = tables.candidate_ids_by_bundle[bundle_index].first() else {
        return bundle_index;
    };
    let Some((entry_id, entity)) = tables.entry_entity_by_candidate_id.get(current_id) else {
        return bundle_index;
    };
    if entity != "Comment" && !entity.ends_with("Comment") {
        return bundle_index;
    }
    let Some(provider) = tables.provider_index_for_bundle(bundle_index) else {
        return bundle_index;
    };
    for other in 0..tables.bundle_count() {
        if tables.provider_index_for_bundle(other) != Some(provider) {
            continue;
        }
        if tables.bundle_root_count(other) != Some(1) {
            continue;
        }
        let Some(other_id) = tables.candidate_ids_by_bundle[other].first() else {
            continue;
        };
        let Some((other_entry, other_entity)) = tables.entry_entity_by_candidate_id.get(other_id)
        else {
            continue;
        };
        if other_entry != entry_id {
            continue;
        }
        if !matches!(other_entity.as_str(), "Issue" | "Post") {
            continue;
        }
        let Some(hints) = tables.relation_hints_by_candidate_id.get(other_id) else {
            continue;
        };
        if relation_hint_covers_entity(hints, entity) {
            return other;
        }
    }
    bundle_index
}

fn replace_context_trap_candidate_ids(
    selected_ids: &[String],
    requirement_texts: &[String],
    tables: &SeedBundleIndexTables,
) -> Vec<String> {
    let wants_issue_comment = requirement_texts
        .iter()
        .any(|text| signals::requirement_implies_issue_comment_create(text));
    selected_ids
        .iter()
        .map(|id| {
            let Some((entry_id, entity)) = tables.entry_entity_by_candidate_id.get(id) else {
                return id.clone();
            };
            if !entity.ends_with("Context") {
                if wants_issue_comment && entity == "Issue" {
                    for (candidate_id, (eid, ent)) in &tables.entry_entity_by_candidate_id {
                        if eid == entry_id && ent == "IssueComment" {
                            return candidate_id.clone();
                        }
                    }
                }
                return id.clone();
            }
            let preferred_entities: &[&str] = if wants_issue_comment {
                &["IssueComment", "Comment"]
            } else {
                &["Issue", "Post", "Message", "Thread"]
            };
            for preferred in preferred_entities {
                for (candidate_id, (eid, ent)) in &tables.entry_entity_by_candidate_id {
                    if eid == entry_id && ent == preferred {
                        return candidate_id.clone();
                    }
                }
            }
            id.clone()
        })
        .collect()
}

pub(crate) fn supporting_capabilities_for_candidate_ids(
    candidate_ids: &[String],
    tables: &SeedBundleIndexTables,
) -> Vec<String> {
    let mut capabilities = Vec::new();
    for bundle_index in 0..tables.bundle_count() {
        for capability_id in tables
            .capability_ids_by_bundle
            .get(bundle_index)
            .into_iter()
            .flatten()
        {
            if candidate_ids
                .iter()
                .any(|candidate_id| capability_id.starts_with(&format!("{candidate_id}:")))
            {
                capabilities.push(capability_id.clone());
            }
        }
    }
    capabilities.sort_unstable();
    capabilities.dedup();
    capabilities
}

fn prefer_non_context_singleton(
    bundle_index: usize,
    requirement_texts: &[String],
    tables: &SeedBundleIndexTables,
) -> usize {
    let Some(provider) = tables.provider_index_for_bundle(bundle_index) else {
        return bundle_index;
    };
    let Some(current_id) = tables.candidate_ids_by_bundle[bundle_index].first() else {
        return bundle_index;
    };
    let Some((_, current_entity)) = tables.entry_entity_by_candidate_id.get(current_id) else {
        return bundle_index;
    };
    if !current_entity.ends_with("Context") {
        return bundle_index;
    }
    let wants_create = requirement_texts
        .iter()
        .any(|text| signals::requirement_implies_create_on_related(text));
    let candidates: Vec<usize> = (0..tables.bundle_count())
        .filter(|&other| {
            tables.provider_index_for_bundle(other) == Some(provider)
                && tables.bundle_root_count(other) == Some(1)
        })
        .filter(|&other| {
            tables.candidate_ids_by_bundle[other]
                .first()
                .and_then(|id| tables.entry_entity_by_candidate_id.get(id))
                .map(|(_, entity)| !entity.ends_with("Context"))
                .unwrap_or(false)
        })
        .collect();
    if wants_create {
        if let Some(found) = candidates.iter().copied().find(|&other| {
            tables.candidate_ids_by_bundle[other]
                .first()
                .and_then(|id| tables.entry_entity_by_candidate_id.get(id))
                .map(|(_, entity)| {
                    entity.contains("Comment") && bundle_mutation_score(tables, other) > 0
                })
                .unwrap_or(false)
        }) {
            return found;
        }
    }
    if let Some(found) = candidates.iter().copied().find(|&other| {
        tables.candidate_ids_by_bundle[other]
            .first()
            .and_then(|id| tables.entry_entity_by_candidate_id.get(id))
            .map(|(_, entity)| entity == "Issue" || entity == "Post")
            .unwrap_or(false)
    }) {
        return found;
    }
    candidates
        .into_iter()
        .min_by_key(|&other| {
            tables.candidate_ids_by_bundle[other]
                .first()
                .and_then(|id| tables.entry_entity_by_candidate_id.get(id))
                .map(|(_, entity)| entity.len())
                .unwrap_or(usize::MAX)
        })
        .unwrap_or(bundle_index)
}

pub(crate) fn finalize_ready_bundle_index(
    bundle_index: usize,
    support_by_requirement: &[HashSet<usize>],
    requirement_texts: &[String],
    intent: &str,
    tables: &SeedBundleIndexTables,
) -> usize {
    let union = support_union(support_by_requirement);
    let bundle_index = prefer_minimal_subset_bundle(bundle_index, &union, tables);
    let bundle_index = prefer_non_context_singleton(bundle_index, requirement_texts, tables);
    let bundle_index = prefer_relation_nav_root(bundle_index, requirement_texts, tables);
    let bundle_index = prefer_parent_entity_for_relation_read(bundle_index, tables);
    let bundle_index = prefer_create_target_entity(bundle_index, requirement_texts, tables);
    let bundle_index =
        prefer_workflow_entity_over_auxiliary(bundle_index, requirement_texts, intent, tables);
    let bundle_index = prefer_share_publish_target(bundle_index, requirement_texts, intent, tables);
    prefer_mutation_capable_singleton(bundle_index, requirement_texts, tables)
}
pub(crate) fn bundle_is_read_only_search(
    tables: &SeedBundleIndexTables,
    bundle_index: usize,
) -> bool {
    let Some(kinds) = tables.capability_kinds_by_bundle.get(bundle_index) else {
        return false;
    };
    !kinds.is_empty()
        && kinds
            .iter()
            .all(|kind| matches!(kind.as_str(), "Query" | "Get" | "Search"))
}

fn entity_mention_score(requirement_text: &str, entity: &str) -> usize {
    let lower = requirement_text.to_lowercase();
    let mut spaced = String::new();
    for ch in entity.chars() {
        if ch.is_uppercase() && !spaced.is_empty() {
            spaced.push(' ');
        }
        spaced.push(ch.to_ascii_lowercase());
    }
    if lower.contains(spaced.trim()) {
        return 2;
    }
    if lower.contains(&entity.to_lowercase()) {
        return 1;
    }
    0
}

pub(crate) fn candidate_ids_for_federated_bundle(
    bundle_index: usize,
    requirement_text: &str,
    tables: &SeedBundleIndexTables,
) -> Result<Vec<String>, SeedSelectionValidationError> {
    let ids = tables.candidate_ids_by_bundle.get(bundle_index).ok_or(
        SeedSelectionValidationError::UnknownBundleIndex(bundle_index as i64),
    )?;
    if ids.len() <= 1 {
        return Ok(ids.clone());
    }
    let lower = requirement_text.to_lowercase();
    let mut by_entry: HashMap<String, (String, usize)> = HashMap::new();
    for id in ids {
        let Some((entry_id, entity)) = tables.entry_entity_by_candidate_id.get(id) else {
            continue;
        };
        let entry_lower = entry_id.to_lowercase().replace('-', " ");
        let mut score = entity_mention_score(requirement_text, entity)
            + if lower.contains(&entry_lower) { 1 } else { 0 };
        if entity.contains("Comment") && !lower.contains("comment") {
            score = score.saturating_sub(3);
        }
        if entity.ends_with("Context") {
            score = score.saturating_sub(2);
        }
        by_entry
            .entry(entry_id.clone())
            .and_modify(|(current, best_score)| {
                if score > *best_score {
                    *current = id.clone();
                    *best_score = score;
                }
            })
            .or_insert_with(|| (id.clone(), score));
    }
    Ok(by_entry.into_values().map(|(id, _)| id).collect())
}

/// Single candidate-id correction pass (context traps, auxiliary swaps, share targets).
pub fn rewrite_selected_candidate_ids(
    selected_ids: Vec<String>,
    requirement_texts: &[String],
    intent: &str,
    tables: &SeedBundleIndexTables,
) -> Vec<String> {
    let context_fixed =
        replace_context_trap_candidate_ids(&selected_ids, requirement_texts, tables);
    replace_auxiliary_candidate_ids(context_fixed, requirement_texts, intent, tables)
}
