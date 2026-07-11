use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::discovery::{CgsCatalog, CgsDiscovery, InMemoryCgsRegistry};
use crate::loader::load_schema_dir;
use crate::schema::CGS;

use super::helpers::ArcCgs;
use super::inject::{inject_workflow_mutation_targets, mutation_capabilities_for_entity};
use super::pool::{group_candidates_by_entity, merge_required_entity_bundles};
use super::{
    capability_query_from_intent_phrase, diversify_entity_bundles,
    retrieve_entity_candidate_bundles, EntityCandidateBundle, EntityCandidateConfig,
    EntityCapabilityEvidence,
};

fn matrix_registry() -> InMemoryCgsRegistry {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_prompt_matrix");
    let cgs = Arc::new(load_schema_dir(&dir).expect("load matrix"));
    InMemoryCgsRegistry::from_pairs(vec![(
        "prompt_matrix".into(),
        "Prompt Matrix".into(),
        vec![],
        cgs,
    )])
}

#[test]
fn groups_capabilities_under_entity() {
    let reg = matrix_registry();
    let bundles = retrieve_entity_candidate_bundles(
        &reg,
        "list issues with labels",
        None,
        EntityCandidateConfig {
            retrieve_k: 32,
            max_entities: 8,
            max_per_catalog: 4,
            max_capabilities_per_entity: 3,
        },
    )
    .expect("retrieve");
    assert!(!bundles.is_empty());
    for b in &bundles {
        assert!(!b.candidate_id.is_empty());
        assert!(b.capabilities.len() <= 3);
    }
}

#[test]
fn tenant_filter_excludes_disallowed_catalogs() {
    let reg = matrix_registry();
    let all = retrieve_entity_candidate_bundles(&reg, "list issues", None, Default::default())
        .expect("all");
    let filtered = retrieve_entity_candidate_bundles(
        &reg,
        "list issues",
        Some(&["nonexistent".into()]),
        Default::default(),
    )
    .expect("filtered");
    assert!(!all.is_empty());
    assert!(filtered.is_empty() || filtered.iter().all(|b| b.entry_id == "nonexistent"));
}

#[test]
fn merge_required_adds_injected_issue_from_pool() {
    let diversified = vec![
        EntityCandidateBundle {
            candidate_id: "jira:SprintBoardSnapshot".into(),
            entry_id: "jira".into(),
            entity: "SprintBoardSnapshot".into(),
            entity_description: String::new(),
            max_lexical_score: 10,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: true,
        },
        EntityCandidateBundle {
            candidate_id: "jira:Sprint".into(),
            entry_id: "jira".into(),
            entity: "Sprint".into(),
            entity_description: String::new(),
            max_lexical_score: 9,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: false,
        },
        EntityCandidateBundle {
            candidate_id: "jira:MyPermissionSet".into(),
            entry_id: "jira".into(),
            entity: "MyPermissionSet".into(),
            entity_description: String::new(),
            max_lexical_score: 8,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: false,
        },
        EntityCandidateBundle {
            candidate_id: "jira:Comment".into(),
            entry_id: "jira".into(),
            entity: "Comment".into(),
            entity_description: String::new(),
            max_lexical_score: 7,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: false,
        },
    ];
    let mut pool: IndexMap<(String, String), EntityCandidateBundle> = diversified
        .iter()
        .map(|b| ((b.entry_id.clone(), b.entity.clone()), b.clone()))
        .collect();
    pool.insert(
        ("jira".into(), "Issue".into()),
        EntityCandidateBundle {
            candidate_id: "jira:Issue".into(),
            entry_id: "jira".into(),
            entity: "Issue".into(),
            entity_description: String::new(),
            max_lexical_score: 1,
            capabilities: vec![EntityCapabilityEvidence {
                capability_id: "jira:Issue:issue_transition".into(),
                capability_name: "issue_transition".into(),
                kind: "Action".into(),
                description: String::new(),
                reason_codes: vec![],
                lexical_score: 1,
            }],
            relation_hints: String::new(),
            catalog_route_evidence: false,
        },
    );
    let required = vec![pool[&("jira".into(), "Issue".into())].clone()];
    let merged =
        merge_required_entity_bundles(diversified, &required, EntityCandidateConfig::default());
    assert!(
        merged.iter().any(|b| b.entity == "Issue"),
        "merged: {:?}",
        merged.iter().map(|b| &b.entity).collect::<Vec<_>>()
    );
}

#[test]
fn inject_workflow_adds_jira_issue_to_pool() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/jira");
    if !dir.exists() {
        return;
    }
    let cgs = Arc::new(load_schema_dir(&dir).expect("load jira"));
    let reg =
        InMemoryCgsRegistry::from_pairs(vec![("jira".into(), "Jira".into(), vec![], cgs.clone())]);
    let intent = "Move the blocker Jira ticket to Done in the current sprint.";
    let query = capability_query_from_intent_phrase(intent);
    let discovery = reg.discover(&query).expect("discover");
    let mut catalogs: IndexMap<String, Arc<CGS>> = IndexMap::new();
    catalogs.insert("jira".into(), cgs);
    let mut pool: IndexMap<(String, String), EntityCandidateBundle> = IndexMap::new();
    pool.insert(
        ("jira".into(), "SprintBoardSnapshot".into()),
        EntityCandidateBundle {
            candidate_id: "jira:SprintBoardSnapshot".into(),
            entry_id: "jira".into(),
            entity: "SprintBoardSnapshot".into(),
            entity_description: String::new(),
            max_lexical_score: 10,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: true,
        },
    );
    inject_workflow_mutation_targets(&mut pool, &catalogs, intent, &discovery);
    assert!(
        pool.contains_key(&("jira".into(), "Issue".into())),
        "pool keys: {:?}",
        pool.keys().collect::<Vec<_>>()
    );
}

#[test]
fn mutation_capabilities_jira_issue_has_action() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/jira");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).expect("load jira");
    let caps = mutation_capabilities_for_entity(&cgs, "jira", "Issue", 3);
    assert!(
        !caps.is_empty(),
        "expected action/update/create on Issue, entities: {:?}",
        cgs.entities.keys().collect::<Vec<_>>()
    );
}

#[test]
fn workflow_mutation_inject_survives_diversification_for_jira_transition() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/jira");
    if !dir.exists() {
        return;
    }
    let cgs = Arc::new(load_schema_dir(&dir).expect("load jira"));
    let reg = InMemoryCgsRegistry::from_pairs(vec![("jira".into(), "Jira".into(), vec![], cgs)]);
    let intent = "Move the blocker Jira ticket to Done in the current sprint.";
    let config = EntityCandidateConfig::default();
    let query = capability_query_from_intent_phrase(intent);
    let discovery = reg.discover(&query).expect("discover");
    let candidates: Vec<_> = discovery
        .candidates
        .iter()
        .take(config.retrieve_k)
        .cloned()
        .collect();
    let mut catalogs: IndexMap<String, ArcCgs> = IndexMap::new();
    for c in &candidates {
        if catalogs.contains_key(&c.entry_id) {
            continue;
        }
        if let Ok(ctx) = reg.load_context(&c.entry_id) {
            catalogs.insert(c.entry_id.clone(), ctx.cgs);
        }
    }
    for entry_id in discovery.catalog_route.as_slice() {
        if catalogs.contains_key(entry_id) {
            continue;
        }
        if let Ok(ctx) = reg.load_context(entry_id) {
            catalogs.insert(entry_id.clone(), ctx.cgs);
        }
    }
    assert!(
        catalogs.contains_key("jira"),
        "catalogs: {:?}",
        catalogs.keys()
    );
    let grouped = group_candidates_by_entity(&candidates, &discovery, &catalogs, config);
    let diversified = diversify_entity_bundles(grouped.values().cloned().collect(), config);
    let mut pool: IndexMap<(String, String), EntityCandidateBundle> = diversified
        .iter()
        .map(|b| ((b.entry_id.clone(), b.entity.clone()), b.clone()))
        .collect();
    let keys_before: HashSet<_> = pool.keys().cloned().collect();
    inject_workflow_mutation_targets(&mut pool, &catalogs, intent, &discovery);
    assert!(
        pool.contains_key(&("jira".into(), "Issue".into())),
        "pool after inject: {:?}",
        pool.keys().collect::<Vec<_>>()
    );
    let required: Vec<EntityCandidateBundle> = pool
        .iter()
        .filter(|(key, _)| !keys_before.contains(key))
        .map(|(_, bundle)| bundle.clone())
        .collect();
    let merged = merge_required_entity_bundles(diversified, &required, config);
    assert!(
        merged.iter().any(|b| b.entity == "Issue"),
        "merged: {:?}",
        merged.iter().map(|b| &b.entity).collect::<Vec<_>>()
    );
}

#[test]
fn merge_required_entity_bundles_evicts_low_score_same_catalog() {
    let diversified = vec![
        EntityCandidateBundle {
            candidate_id: "jira:SprintBoardSnapshot".into(),
            entry_id: "jira".into(),
            entity: "SprintBoardSnapshot".into(),
            entity_description: String::new(),
            max_lexical_score: 10,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: true,
        },
        EntityCandidateBundle {
            candidate_id: "jira:Board".into(),
            entry_id: "jira".into(),
            entity: "Board".into(),
            entity_description: String::new(),
            max_lexical_score: 8,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: false,
        },
    ];
    let mut pool: IndexMap<(String, String), EntityCandidateBundle> = diversified
        .iter()
        .map(|b| ((b.entry_id.clone(), b.entity.clone()), b.clone()))
        .collect();
    pool.insert(
        ("jira".into(), "Issue".into()),
        EntityCandidateBundle {
            candidate_id: "jira:Issue".into(),
            entry_id: "jira".into(),
            entity: "Issue".into(),
            entity_description: String::new(),
            max_lexical_score: 1,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: false,
        },
    );
    let required = vec![pool[&("jira".into(), "Issue".into())].clone()];
    let merged = merge_required_entity_bundles(
        diversified,
        &required,
        EntityCandidateConfig {
            max_entities: 4,
            max_per_catalog: 2,
            ..Default::default()
        },
    );
    assert!(merged.iter().any(|b| b.entity == "Issue"));
    assert_eq!(merged.iter().filter(|b| b.entry_id == "jira").count(), 2);
}

#[test]
fn diversification_caps_per_catalog() {
    let bundles = vec![
        EntityCandidateBundle {
            candidate_id: "a:E1".into(),
            entry_id: "a".into(),
            entity: "E1".into(),
            entity_description: String::new(),
            max_lexical_score: 10,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: false,
        },
        EntityCandidateBundle {
            candidate_id: "a:E2".into(),
            entry_id: "a".into(),
            entity: "E2".into(),
            entity_description: String::new(),
            max_lexical_score: 9,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: false,
        },
        EntityCandidateBundle {
            candidate_id: "b:E1".into(),
            entry_id: "b".into(),
            entity: "E1".into(),
            entity_description: String::new(),
            max_lexical_score: 8,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: false,
        },
    ];
    let out = diversify_entity_bundles(
        bundles,
        EntityCandidateConfig {
            max_entities: 3,
            max_per_catalog: 1,
            ..Default::default()
        },
    );
    assert_eq!(out.len(), 2);
    assert!(out.iter().any(|b| b.entry_id == "a"));
    assert!(out.iter().any(|b| b.entry_id == "b"));
}
