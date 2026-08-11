use std::collections::HashSet;

use indexmap::IndexMap;

use crate::catalog_search_index::CatalogSearchIndex;
use crate::discovery::{outgoing_relation_hints_for_entity, DiscoveryResult, RankedCandidate};
use crate::discovery_intent_class::DiscoveryIntentClass;
use crate::discovery_seed_catalog::{
    inject_entity_targets, CatalogSeedIndex, CatalogWorkflowContext,
};
use crate::schema::{RelationSchema, CGS};

use super::helpers::{candidate_id, capability_id, entity_description_for, ArcCgs};
use super::types::{EntityCandidateBundle, EntityCapabilityEvidence};

fn is_schema_relation_leaf(cgs: &CGS, entity: &str) -> bool {
    !catalog_parent_entities(cgs, entity).is_empty()
}

/// When a schema relation leaf is in the pool, ensure parents exist with relation_coverage hints.
fn inject_relation_parent_bundles(
    bundles: &mut IndexMap<(String, String), EntityCandidateBundle>,
    catalogs: &IndexMap<String, ArcCgs>,
    discovery: &DiscoveryResult,
) {
    let route_set: HashSet<&str> = discovery
        .catalog_route
        .as_slice()
        .iter()
        .map(|s| s.as_str())
        .collect();
    let keys: Vec<(String, String)> = bundles.keys().cloned().collect();
    for (entry_id, leaf) in keys {
        let Some(cgs) = catalogs.get(&entry_id) else {
            continue;
        };
        if !is_schema_relation_leaf(cgs.as_ref(), &leaf) {
            continue;
        }
        let leaf_score = bundles
            .get(&(entry_id.clone(), leaf.clone()))
            .map(|bundle| bundle.max_lexical_score)
            .unwrap_or(1);
        for parent in catalog_parent_entities(cgs.as_ref(), &leaf) {
            upsert_relation_parent_bundle(
                bundles,
                cgs,
                discovery,
                RelationParentSeed {
                    entry_id: &entry_id,
                    parent_entity: &parent,
                    leaf_entity: &leaf,
                    catalog_route_evidence: route_set.contains(entry_id.as_str()),
                    score_floor: leaf_score.saturating_add(1),
                },
            );
        }
    }
}

pub(crate) fn catalog_parent_entities(cgs: &CGS, leaf: &str) -> Vec<String> {
    cgs.entities
        .keys()
        .filter(|name| {
            cgs.get_entity(name.as_str()).is_some_and(|ent| {
                ent.relations
                    .values()
                    .any(|rel| rel.target_resource == leaf)
            })
        })
        .map(|name| name.to_string())
        .collect()
}

pub(crate) fn read_capabilities_for_entity(
    cgs: &CGS,
    entry_id: &str,
    entity: &str,
    max: usize,
) -> Vec<EntityCapabilityEvidence> {
    use crate::schema::CapabilityKind;

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for kind in [
        CapabilityKind::Query,
        CapabilityKind::Get,
        CapabilityKind::Search,
        CapabilityKind::Action,
    ] {
        for cap in cgs.find_capabilities(entity, kind) {
            if !cap.is_read() {
                continue;
            }
            if push_capability_evidence(&mut out, &mut seen, entry_id, entity, cap, max) {
                return out;
            }
        }
    }
    out
}

fn relation_hint_includes_leaf(hints: &str, leaf: &str) -> bool {
    hints
        .split(';')
        .any(|hint| hint.trim().ends_with(&format!("→{leaf}")))
}

fn ensure_leaf_relation_hint(cgs: &CGS, parent_entity: &str, leaf: &str, hints: &mut String) {
    if relation_hint_includes_leaf(hints, leaf) {
        return;
    }
    let Some(ent) = cgs.get_entity(parent_entity) else {
        return;
    };
    for (wire, rel) in &ent.relations {
        if rel.target_resource == leaf {
            let hop = format!("{wire}→{leaf}");
            if hints.is_empty() {
                *hints = hop;
            } else {
                hints.push_str("; ");
                hints.push_str(&hop);
            }
            return;
        }
    }
}

struct RelationParentSeed<'a> {
    entry_id: &'a str,
    parent_entity: &'a str,
    leaf_entity: &'a str,
    catalog_route_evidence: bool,
    score_floor: u32,
}

fn upsert_relation_parent_bundle(
    bundles: &mut IndexMap<(String, String), EntityCandidateBundle>,
    cgs: &ArcCgs,
    discovery: &DiscoveryResult,
    seed: RelationParentSeed<'_>,
) {
    let RelationParentSeed {
        entry_id,
        parent_entity,
        leaf_entity,
        catalog_route_evidence,
        score_floor,
    } = seed;
    let key = (entry_id.to_string(), parent_entity.to_string());
    let mut hints = outgoing_relation_hints_for_entity(
        cgs.as_ref(),
        parent_entity,
        crate::discovery::DISCOVERY_OUTGOING_RELATIONS_MAX,
    );
    ensure_leaf_relation_hint(cgs.as_ref(), parent_entity, leaf_entity, &mut hints);
    if let Some(existing) = bundles.get_mut(&key) {
        ensure_leaf_relation_hint(
            cgs.as_ref(),
            parent_entity,
            leaf_entity,
            &mut existing.relation_hints,
        );
        if existing.relation_hints.is_empty() && !hints.is_empty() {
            existing.relation_hints = hints;
        }
        existing.max_lexical_score = existing.max_lexical_score.max(score_floor);
        if existing.capabilities.is_empty() {
            existing.capabilities =
                read_capabilities_for_entity(cgs.as_ref(), entry_id, parent_entity, 2);
        }
        return;
    }
    let capabilities = read_capabilities_for_entity(cgs.as_ref(), entry_id, parent_entity, 2);
    bundles.insert(
        key.clone(),
        EntityCandidateBundle {
            candidate_id: candidate_id(&key.0, &key.1),
            entry_id: key.0.clone(),
            entity: key.1.clone(),
            entity_description: entity_description_for(
                &discovery.entity_summaries,
                &key.0,
                &key.1,
                Some(cgs.as_ref()),
            ),
            max_lexical_score: score_floor.max(1),
            capabilities,
            relation_hints: hints,
            catalog_route_evidence,
        },
    );
}

fn relation_intent_score(intent_tokens: &HashSet<String>, rel: &RelationSchema) -> u32 {
    let Some(h) = &rel.discovery else {
        return 1;
    };
    if h.qualifier_terms.is_empty() {
        return 1;
    }
    let mut total = 0u32;
    for term in &h.qualifier_terms {
        for tok in CatalogSearchIndex::tokenize(term) {
            if intent_tokens.contains(&tok) {
                total = total.saturating_add(1);
            }
        }
    }
    total
}

#[allow(dead_code)]
pub(crate) fn mutation_capabilities_for_entity(
    cgs: &CGS,
    entry_id: &str,
    entity: &str,
    max: usize,
) -> Vec<EntityCapabilityEvidence> {
    mutation_capabilities_for_entity_with_intent(cgs, entry_id, entity, max, "", None)
}

pub(crate) fn push_capability_evidence(
    out: &mut Vec<EntityCapabilityEvidence>,
    seen: &mut HashSet<String>,
    entry_id: &str,
    entity: &str,
    cap: &crate::schema::CapabilitySchema,
    max: usize,
) -> bool {
    if !seen.insert(cap.name.to_string()) {
        return out.len() >= max;
    }
    out.push(EntityCapabilityEvidence {
        capability_id: capability_id(entry_id, entity, cap.name.as_str()),
        capability_name: cap.name.to_string(),
        kind: format!("{:?}", cap.kind),
        effect: cap.effective_effect(),
        description: cap.description.clone(),
        reason_codes: Vec::new(),
        lexical_score: 1,
    });
    out.len() >= max
}

fn select_intent_relevant_mutation_caps(
    cgs: &CGS,
    entry_id: &str,
    entity: &str,
    intent: &str,
    index: &CatalogSeedIndex,
    max: usize,
) -> Vec<EntityCapabilityEvidence> {
    use crate::schema::CapabilityKind;

    let intent_lower = intent.to_lowercase();
    let intent_tokens: HashSet<String> = CatalogSearchIndex::tokenize(intent);
    let mut scored: Vec<(i32, CapabilityKind, &crate::schema::CapabilitySchema)> = Vec::new();

    for kind in [
        CapabilityKind::Create,
        CapabilityKind::Action,
        CapabilityKind::Update,
    ] {
        for cap in cgs.find_capabilities(entity, kind) {
            if !cap.is_remote_mutation() {
                continue;
            }
            let mut score = 0i32;
            if let Some(caps) = index.mutation_caps.get(entity) {
                if let Some(meta) = caps.iter().find(|meta| meta.name == cap.name.as_str()) {
                    for phrase in meta
                        .operation_phrases
                        .iter()
                        .chain(meta.target_phrases.iter())
                    {
                        if phrase.contains(' ') {
                            if intent_lower.contains(phrase) {
                                score += 3;
                            }
                        } else if intent_tokens.contains(phrase) || intent_lower.contains(phrase) {
                            score += 3;
                        }
                    }
                    for token in CatalogSearchIndex::tokenize(&cap.name) {
                        if intent_tokens.contains(&token) || intent_lower.contains(&token) {
                            score += 1;
                        }
                    }
                }
            }
            for token in CatalogSearchIndex::tokenize(&cap.name) {
                if intent_tokens.contains(&token) || intent_lower.contains(&token) {
                    score += 1;
                }
            }
            scored.push((score, kind, cap));
        }
    }

    scored.sort_by(|left, right| {
        let kind_rank = |kind: CapabilityKind| match kind {
            CapabilityKind::Create => 0,
            CapabilityKind::Action => 1,
            CapabilityKind::Update => 2,
            _ => 3,
        };
        right
            .0
            .cmp(&left.0)
            .then_with(|| kind_rank(left.1).cmp(&kind_rank(right.1)))
            .then_with(|| left.2.name.cmp(&right.2.name))
    });

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (_, _, cap) in scored.into_iter().take(max) {
        if push_capability_evidence(&mut out, &mut seen, entry_id, entity, cap, max) {
            break;
        }
    }
    out
}

pub(crate) fn mutation_capabilities_for_entity_with_intent(
    cgs: &CGS,
    entry_id: &str,
    entity: &str,
    max: usize,
    intent: &str,
    catalog_context: Option<&CatalogWorkflowContext>,
) -> Vec<EntityCapabilityEvidence> {
    let prefer_workflow = catalog_context.is_some_and(|ctx| {
        ctx.suggests_multi_entity_workflow(entry_id) || ctx.suggests_mutation_workflow(entry_id)
    });
    if prefer_workflow {
        if let Some(ctx) = catalog_context {
            if let Some(index) = ctx.index(entry_id) {
                let selected =
                    select_intent_relevant_mutation_caps(cgs, entry_id, entity, intent, index, max);
                if !selected.is_empty() {
                    return selected;
                }
            }
        }
    }
    use crate::schema::CapabilityKind;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for kind in [
        CapabilityKind::Create,
        CapabilityKind::Action,
        CapabilityKind::Update,
    ] {
        for cap in cgs.find_capabilities(entity, kind) {
            if !cap.is_remote_mutation() {
                continue;
            }
            if push_capability_evidence(&mut out, &mut seen, entry_id, entity, cap, max) {
                return out;
            }
        }
    }
    out
}

fn extra_workflow_mutation_entities(
    entry_id: &str,
    catalog_context: Option<&CatalogWorkflowContext>,
) -> Vec<String> {
    let Some(ctx) = catalog_context else {
        return Vec::new();
    };
    let Some(index) = ctx.index(entry_id) else {
        return Vec::new();
    };
    let Some(workflow) = ctx.workflow_match(entry_id) else {
        return Vec::new();
    };
    if matches!(
        ctx.intent_class(),
        DiscoveryIntentClass::CatalogExploration
            | DiscoveryIntentClass::ReadListNav
            | DiscoveryIntentClass::ReadListLeafCollection
            | DiscoveryIntentClass::HostCapabilityMiss { .. }
    ) {
        return Vec::new();
    }
    let repo_scoped = ctx.suggests_repo_scoped_workflow(entry_id);
    if !repo_scoped
        && !ctx.is_localized_mutation(entry_id)
        && !ctx.intent_class().allows_workflow_inject()
    {
        return Vec::new();
    }
    inject_entity_targets(index, workflow, repo_scoped)
}

fn inject_relation_targets(
    bundles: &mut IndexMap<(String, String), EntityCandidateBundle>,
    catalogs: &IndexMap<String, ArcCgs>,
    candidates: &[RankedCandidate],
    intent: &str,
    discovery: &DiscoveryResult,
) {
    let intent_tokens: HashSet<String> = CatalogSearchIndex::tokenize(intent);
    let present: HashSet<(String, String)> = bundles.keys().cloned().collect();
    let route_set: HashSet<&str> = discovery
        .catalog_route
        .as_slice()
        .iter()
        .map(|s| s.as_str())
        .collect();

    let mut seen_rel_targets: HashSet<(String, String)> = HashSet::new();
    for cand in candidates.iter().take(32) {
        let Some(cgs) = catalogs.get(&cand.entry_id) else {
            continue;
        };
        let Some(ent) = cgs.get_entity(cand.entity.as_str()) else {
            continue;
        };
        for rel in ent.relations.values() {
            let target = rel.target_resource.as_str();
            if cgs.get_entity(target).is_none() {
                continue;
            }
            if relation_intent_score(&intent_tokens, rel) == 0 {
                continue;
            }
            let key = (cand.entry_id.clone(), target.to_string());
            if present.contains(&key) || !seen_rel_targets.insert(key.clone()) {
                continue;
            }
            upsert_relation_parent_bundle(
                bundles,
                cgs,
                discovery,
                RelationParentSeed {
                    entry_id: &cand.entry_id,
                    parent_entity: cand.entity.as_str(),
                    leaf_entity: target,
                    catalog_route_evidence: route_set.contains(cand.entry_id.as_str()),
                    score_floor: relation_intent_score(&intent_tokens, rel).max(1),
                },
            );
            bundles.insert(
                key.clone(),
                EntityCandidateBundle {
                    candidate_id: candidate_id(&key.0, &key.1),
                    entry_id: key.0.clone(),
                    entity: key.1.clone(),
                    entity_description: entity_description_for(
                        &discovery.entity_summaries,
                        &key.0,
                        &key.1,
                        Some(cgs.as_ref()),
                    ),
                    max_lexical_score: 0,
                    capabilities: Vec::new(),
                    relation_hints: outgoing_relation_hints_for_entity(
                        cgs.as_ref(),
                        target,
                        crate::discovery::DISCOVERY_OUTGOING_RELATIONS_MAX,
                    ),
                    catalog_route_evidence: route_set.contains(key.0.as_str()),
                },
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn inject_mutation_entity_bundle(
    bundles: &mut IndexMap<(String, String), EntityCandidateBundle>,
    cgs: &ArcCgs,
    entry_id: &str,
    entity: &str,
    discovery: &DiscoveryResult,
    route_set: &HashSet<&str>,
    intent: &str,
    catalog_context: Option<&CatalogWorkflowContext>,
) {
    let key = (entry_id.to_string(), entity.to_string());
    let capabilities = mutation_capabilities_for_entity_with_intent(
        cgs.as_ref(),
        entry_id,
        entity,
        3,
        intent,
        catalog_context,
    );
    if capabilities.is_empty() {
        return;
    }
    if let Some(existing) = bundles.get_mut(&key) {
        let refresh_for_workflow = catalog_context.is_some_and(|ctx| {
            ctx.suggests_multi_entity_workflow(entry_id)
                && (existing.capabilities.is_empty()
                    || !existing.capabilities.iter().any(|cap| cap.kind == "Create"))
        });
        if !refresh_for_workflow && !existing.capabilities.is_empty() {
            return;
        }
        existing.capabilities = capabilities;
        existing.max_lexical_score = existing.max_lexical_score.max(1);
        return;
    }
    bundles.insert(
        key.clone(),
        EntityCandidateBundle {
            candidate_id: candidate_id(&key.0, &key.1),
            entry_id: key.0.clone(),
            entity: key.1.clone(),
            entity_description: entity_description_for(
                &discovery.entity_summaries,
                &key.0,
                &key.1,
                Some(cgs.as_ref()),
            ),
            max_lexical_score: 1,
            capabilities,
            relation_hints: outgoing_relation_hints_for_entity(
                cgs.as_ref(),
                entity,
                crate::discovery::DISCOVERY_OUTGOING_RELATIONS_MAX,
            ),
            catalog_route_evidence: route_set.contains(entry_id),
        },
    );
}

pub(crate) fn workflow_inject_active_for(
    intent_class: &DiscoveryIntentClass,
    catalog_context: &CatalogWorkflowContext,
) -> bool {
    if matches!(
        intent_class,
        DiscoveryIntentClass::CatalogExploration
            | DiscoveryIntentClass::ReadListNav
            | DiscoveryIntentClass::ReadListLeafCollection
            | DiscoveryIntentClass::HostCapabilityMiss { .. }
    ) {
        return false;
    }
    intent_class.allows_workflow_inject()
        || catalog_context.any_localized_mutation()
        || catalog_context.any_repo_scoped_workflow()
}

pub(crate) fn inject_workflow_mutation_targets(
    bundles: &mut IndexMap<(String, String), EntityCandidateBundle>,
    catalogs: &IndexMap<String, ArcCgs>,
    intent: &str,
    discovery: &DiscoveryResult,
    catalog_context: &CatalogWorkflowContext,
    named_catalogs: &[String],
) {
    if !workflow_inject_active_for(catalog_context.intent_class(), catalog_context) {
        return;
    }
    let route_set: HashSet<&str> = discovery
        .catalog_route
        .as_slice()
        .iter()
        .map(|s| s.as_str())
        .collect();
    let mut catalog_ids: HashSet<String> = bundles
        .keys()
        .map(|(entry_id, _)| entry_id.clone())
        .collect();
    for entry_id in discovery.catalog_route.as_slice() {
        catalog_ids.insert(entry_id.clone());
    }
    for entry_id in catalogs.keys() {
        if named_catalogs
            .iter()
            .any(|catalog| catalog.eq_ignore_ascii_case(entry_id))
        {
            catalog_ids.insert(entry_id.clone());
        }
    }
    for entry_id in catalog_ids {
        let Some(cgs) = catalogs.get(&entry_id) else {
            continue;
        };
        let entity_names = extra_workflow_mutation_entities(&entry_id, Some(catalog_context));
        for entity in entity_names {
            if cgs.get_entity(entity.as_str()).is_none() {
                continue;
            }
            inject_mutation_entity_bundle(
                bundles,
                cgs,
                &entry_id,
                entity.as_str(),
                discovery,
                &route_set,
                intent,
                Some(catalog_context),
            );
        }
    }
}

fn primary_read_entity_for_mirror(cgs: &CGS, entry_id: &str) -> Option<String> {
    use crate::discovery_seed_catalog::build_catalog_seed_index;
    use crate::schema::CapabilityKind;

    let index = build_catalog_seed_index(entry_id, cgs);
    let mut best: Option<(usize, String)> = None;
    for entity_name in cgs.entities.keys() {
        let entity = entity_name.as_str();
        let has_read = [
            CapabilityKind::Query,
            CapabilityKind::Get,
            CapabilityKind::Search,
            CapabilityKind::Action,
        ]
        .into_iter()
        .flat_map(|kind| cgs.find_capabilities(entity, kind))
        .any(|cap| cap.is_read());
        if !has_read {
            continue;
        }
        let out = index.outgoing_relation_count(entity);
        if best.as_ref().is_none_or(|(score, _)| out > *score) {
            best = Some((out, entity.to_string()));
        }
    }
    best.map(|(_, entity)| entity)
}

fn inject_mirror_catalog_targets(
    bundles: &mut IndexMap<(String, String), EntityCandidateBundle>,
    catalogs: &IndexMap<String, ArcCgs>,
    _intent: &str,
    discovery: &DiscoveryResult,
    named_catalogs: &[String],
) {
    if named_catalogs.len() < 2 {
        return;
    }
    let route_set: HashSet<&str> = discovery
        .catalog_route
        .as_slice()
        .iter()
        .map(|s| s.as_str())
        .collect();
    let present_catalogs: HashSet<String> = bundles
        .keys()
        .map(|(entry_id, _)| entry_id.clone())
        .collect();
    let mut mirror_catalogs: Vec<String> = catalogs.keys().cloned().collect();
    for entry_id in discovery.catalog_route.as_slice() {
        if !mirror_catalogs.iter().any(|id| id == entry_id) {
            mirror_catalogs.push(entry_id.clone());
        }
    }
    for catalog in mirror_catalogs {
        if present_catalogs.contains(&catalog) {
            continue;
        }
        if !named_catalogs
            .iter()
            .any(|named| named.eq_ignore_ascii_case(&catalog))
        {
            continue;
        }
        let Some(cgs) = catalogs.get(&catalog) else {
            continue;
        };
        let Some(entity) = primary_read_entity_for_mirror(cgs.as_ref(), &catalog) else {
            continue;
        };
        let key = (catalog.clone(), entity.clone());
        if bundles.contains_key(&key) {
            continue;
        }
        let capabilities = read_capabilities_for_entity(cgs.as_ref(), &catalog, entity.as_str(), 2);
        bundles.insert(
            key.clone(),
            EntityCandidateBundle {
                candidate_id: candidate_id(&key.0, &key.1),
                entry_id: key.0.clone(),
                entity: key.1.clone(),
                entity_description: entity_description_for(
                    &discovery.entity_summaries,
                    &key.0,
                    &key.1,
                    Some(cgs.as_ref()),
                ),
                max_lexical_score: 1,
                capabilities,
                relation_hints: outgoing_relation_hints_for_entity(
                    cgs.as_ref(),
                    entity.as_str(),
                    crate::discovery::DISCOVERY_OUTGOING_RELATIONS_MAX,
                ),
                catalog_route_evidence: route_set.contains(catalog.as_str()),
            },
        );
    }
}

pub(crate) fn inject_retrieval_targets(
    grouped: &mut IndexMap<(String, String), EntityCandidateBundle>,
    catalogs: &IndexMap<String, ArcCgs>,
    candidates: &[RankedCandidate],
    intent: &str,
    discovery: &DiscoveryResult,
    named_catalogs: &[String],
) {
    inject_relation_targets(grouped, catalogs, candidates, intent, discovery);
    inject_relation_parent_bundles(grouped, catalogs, discovery);
    inject_mirror_catalog_targets(grouped, catalogs, intent, discovery, named_catalogs);
}
