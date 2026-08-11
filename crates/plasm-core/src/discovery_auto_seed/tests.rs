use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::discovery::{CgsCatalog, CgsDiscovery, InMemoryCgsRegistry};
use crate::discovery_intent_class::DiscoveryIntentClass;
use crate::loader::load_schema_dir;
use crate::schema::CGS;

use super::helpers::ArcCgs;
use super::inject::{inject_workflow_mutation_targets, mutation_capabilities_for_entity};
use super::pool::{
    group_candidates_by_entity, merge_required_entity_bundles, readmit_scored_entity_drops,
};
use super::{
    capability_query_from_intent_phrase, diversify_entity_bundles,
    retrieve_entity_candidate_bundles, EntityCandidateBundle, EntityCandidateConfig,
    EntityCapabilityEvidence,
};

fn read_list_class() -> DiscoveryIntentClass {
    DiscoveryIntentClass::ReadListNav
}

fn localized_mutation_class() -> DiscoveryIntentClass {
    DiscoveryIntentClass::LocalizedMutation
}

fn repo_workflow_class() -> DiscoveryIntentClass {
    DiscoveryIntentClass::RepoScopedWorkflow
}

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
    let result = retrieve_entity_candidate_bundles(
        &reg,
        "list issues with labels",
        None,
        EntityCandidateConfig {
            retrieve_k: 32,
            max_entities: 8,
            max_per_catalog: 4,
            max_capabilities_per_entity: 3,
        },
        &read_list_class(),
        &[],
    )
    .expect("retrieve");
    let bundles = &result.bundles;
    assert!(!bundles.is_empty());
    for b in bundles {
        assert!(!b.candidate_id.is_empty());
        assert!(b.capabilities.len() <= 3);
    }
}

#[test]
fn tenant_filter_excludes_disallowed_catalogs() {
    let reg = matrix_registry();
    let all = retrieve_entity_candidate_bundles(
        &reg,
        "list issues",
        None,
        Default::default(),
        &read_list_class(),
        &[],
    )
    .expect("all");
    let filtered = retrieve_entity_candidate_bundles(
        &reg,
        "list issues",
        Some(&["nonexistent".into()]),
        Default::default(),
        &read_list_class(),
        &[],
    )
    .expect("filtered");
    assert!(!all.bundles.is_empty());
    assert!(
        filtered.bundles.is_empty() || filtered.bundles.iter().all(|b| b.entry_id == "nonexistent")
    );
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
                effect: crate::SemanticEffect::SideEffect,
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
    let intent = "Move the blocker Jira issue to Done in the current sprint.";
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
    let map: std::collections::HashMap<String, &CGS> = catalogs
        .iter()
        .map(|(id, cgs)| (id.clone(), cgs.as_ref()))
        .collect();
    let ctx = crate::discovery_seed_catalog::CatalogWorkflowContext::build(
        &map,
        intent,
        &localized_mutation_class(),
        &["jira".into()],
    );
    inject_workflow_mutation_targets(
        &mut pool,
        &catalogs,
        intent,
        &discovery,
        &ctx,
        &["jira".into()],
    );
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
    let intent = "Move the blocker Jira issue to Done in the current sprint.";
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
    let map: std::collections::HashMap<String, &CGS> = catalogs
        .iter()
        .map(|(id, cgs)| (id.clone(), cgs.as_ref()))
        .collect();
    let ctx = crate::discovery_seed_catalog::CatalogWorkflowContext::build(
        &map,
        intent,
        &localized_mutation_class(),
        &["jira".into()],
    );
    inject_workflow_mutation_targets(
        &mut pool,
        &catalogs,
        intent,
        &discovery,
        &ctx,
        &["jira".into()],
    );
    assert!(
        pool.contains_key(&("jira".into(), "Issue".into())),
        "pool after inject: {:?}",
        pool.keys().collect::<Vec<_>>()
    );
    let required: Vec<EntityCandidateBundle> = pool
        .iter()
        .filter(|(key, _)| !keys_before.contains(key) && key.1 == "Issue")
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
    // Scored siblings may be re-admitted beyond max_per_catalog (recall reserve).
    assert!(out.len() >= 2 && out.len() <= 3);
    assert!(out.iter().any(|b| b.entry_id == "a" && b.entity == "E1"));
    assert!(out.iter().any(|b| b.entry_id == "b"));
    assert!(out.iter().any(|b| b.entity == "E2"));
}

#[test]
fn github_repository_has_mutation_capabilities_for_inject() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).expect("load github");
    let caps = mutation_capabilities_for_entity(&cgs, "github", "Repository", 3);
    assert!(
        !caps.is_empty(),
        "expected Repository mutation capabilities, entities: {:?}",
        cgs.entities.keys().collect::<Vec<_>>()
    );
}

#[test]
fn github_pr_review_leaf_injects_pull_request_parent() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
    if !dir.exists() {
        return;
    }
    let cgs = Arc::new(load_schema_dir(&dir).expect("load github"));
    let reg =
        InMemoryCgsRegistry::from_pairs(vec![("github".into(), "GitHub".into(), vec![], cgs)]);
    let intent = "For every open PR in the monorepo, include the requested reviewer list.";
    let result = retrieve_entity_candidate_bundles(
        &reg,
        intent,
        None,
        EntityCandidateConfig {
            retrieve_k: 32,
            ..Default::default()
        },
        &read_list_class(),
        &["github".into()],
    )
    .expect("retrieve");
    let has_leaf = result
        .bundles
        .iter()
        .any(|b| b.entry_id == "github" && b.entity == "PullRequestReview");
    let parent = result
        .bundles
        .iter()
        .find(|b| b.entry_id == "github" && b.entity == "PullRequest");
    assert!(has_leaf, "expected PullRequestReview in pool");
    let parent = parent.expect("expected PullRequest parent injected");
    assert!(
        parent.relation_hints.contains("PullRequestReview"),
        "parent relation_hints: {}",
        parent.relation_hints
    );
    assert!(
        !parent.capabilities.is_empty(),
        "parent should carry read capabilities"
    );
}

#[test]
fn github_repo_workflow_inject_surfaces_create_capabilities() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
    if !dir.exists() {
        return;
    }
    use super::inject::mutation_capabilities_for_entity_with_intent;
    use crate::discovery_seed_catalog::CatalogWorkflowContext;

    let cgs = load_schema_dir(&dir).expect("load github");
    let intent =
        "In the GitHub monorepo: open a bug issue, cut a feature branch, commit a readme update, open a pull request, and leave an issue comment with the PR link.";
    let map = std::collections::HashMap::from([("github".to_string(), &cgs)]);
    let ctx =
        CatalogWorkflowContext::build(&map, intent, &repo_workflow_class(), &["github".into()]);
    let issue_caps = mutation_capabilities_for_entity_with_intent(
        &cgs,
        "github",
        "Issue",
        3,
        intent,
        Some(&ctx),
    );
    assert!(
        issue_caps
            .iter()
            .any(|cap| cap.capability_name == "issue_create"),
        "expected issue_create in workflow inject, got {:?}",
        issue_caps
            .iter()
            .map(|cap| cap.capability_name.as_str())
            .collect::<Vec<_>>()
    );
    let pr_caps = mutation_capabilities_for_entity_with_intent(
        &cgs,
        "github",
        "PullRequest",
        3,
        intent,
        Some(&ctx),
    );
    assert!(
        pr_caps.iter().any(|cap| cap.capability_name == "pr_create"),
        "expected pr_create in workflow inject, got {:?}",
        pr_caps
            .iter()
            .map(|cap| cap.capability_name.as_str())
            .collect::<Vec<_>>()
    );
}

fn discovery_eval_catalog_ids() -> Vec<String> {
    vec![
        "github".into(),
        "jira".into(),
        "linear".into(),
        "clickup".into(),
        "gitlab".into(),
        "slack".into(),
        "discord".into(),
        "gmail".into(),
        "outlook".into(),
        "google-drive".into(),
        "google-sheets".into(),
        "google-calendar".into(),
        "google-docs".into(),
        "notion".into(),
        "microsoft-teams".into(),
        "reddit".into(),
    ]
}

fn load_multi_catalog_registry(entry_ids: &[String]) -> Option<InMemoryCgsRegistry> {
    let apis_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis");
    if !apis_root.is_dir() {
        return None;
    }
    let mut pairs = Vec::new();
    for id in entry_ids {
        let dir = apis_root.join(id);
        if !dir.join("domain.yaml").is_file() {
            continue;
        }
        let cgs = load_schema_dir(&dir).ok()?;
        pairs.push((id.clone(), id.clone(), Vec::new(), Arc::new(cgs)));
    }
    if pairs.len() < 8 {
        return None;
    }
    Some(InMemoryCgsRegistry::from_pairs(pairs))
}

#[test]
fn github_repo_workflow_merge_retains_repository() {
    let Some(reg) = load_multi_catalog_registry(&discovery_eval_catalog_ids()) else {
        return;
    };
    let intent = "On github repo ryan-s-roberts/tool-test: open an issue with a bug label, create a branch, commit a small markdown file, open a PR linking the issue, and comment on the issue with the PR link.";
    let allowed: Vec<String> = discovery_eval_catalog_ids();
    let bundles = retrieve_entity_candidate_bundles(
        &reg,
        intent,
        Some(&allowed),
        Default::default(),
        &repo_workflow_class(),
        &["github".into()],
    )
    .expect("retrieve bundles");
    assert!(
        bundles
            .bundles
            .iter()
            .any(|b| b.entry_id == "github" && b.entity == "Repository"),
        "retrieve path must retain github:Repository"
    );
}

#[test]
fn github_repo_workflow_inject_survives_multi_catalog_diversification() {
    let Some(reg) = load_multi_catalog_registry(&discovery_eval_catalog_ids()) else {
        return;
    };
    let intent = "On github repo ryan-s-roberts/tool-test: open an issue with a bug label, create a branch, commit a small markdown file, open a PR linking the issue, and comment on the issue with the PR link.";
    let allowed: Vec<String> = discovery_eval_catalog_ids();
    let bundles = retrieve_entity_candidate_bundles(
        &reg,
        intent,
        Some(&allowed),
        Default::default(),
        &repo_workflow_class(),
        &["github".into()],
    )
    .expect("retrieve bundles");
    assert!(
        bundles
            .bundles
            .iter()
            .any(|b| b.entry_id == "github" && b.entity == "Repository"),
        "expected github:Repository in diversified pool, got: {:?}",
        bundles
            .bundles
            .iter()
            .filter(|b| b.entry_id == "github")
            .map(|b| &b.entity)
            .collect::<Vec<_>>()
    );
}

#[test]
fn github_repo_workflow_seed_bundles_include_repository() {
    use crate::discovery_seed_bundle::build_candidate_seed_bundles;
    let Some(reg) = load_multi_catalog_registry(&discovery_eval_catalog_ids()) else {
        return;
    };
    let intent = "On github repo ryan-s-roberts/tool-test: open an issue with a bug label, create a branch, commit a small markdown file, open a PR linking the issue, and comment on the issue with the PR link.";
    let allowed: Vec<String> = discovery_eval_catalog_ids();
    let bundles = retrieve_entity_candidate_bundles(
        &reg,
        intent,
        Some(&allowed),
        Default::default(),
        &repo_workflow_class(),
        &["github".into()],
    )
    .expect("retrieve bundles");
    let seed_bundles = build_candidate_seed_bundles(intent, &bundles.bundles, Default::default());
    let github_repo = seed_bundles
        .iter()
        .flat_map(|b| b.candidate_ids.iter())
        .any(|id| id == "github:Repository");
    assert!(
        github_repo,
        "expected github:Repository seed bundle, github bundles: {:?}",
        seed_bundles
            .iter()
            .filter(|b| b.catalogs.contains(&"github".into()))
            .map(|b| &b.candidate_ids)
            .collect::<Vec<_>>()
    );
}

#[test]
fn github_tool_test_intent_pool_includes_label_comment_branch() {
    let Some(reg) = load_multi_catalog_registry(&discovery_eval_catalog_ids()) else {
        return;
    };
    let intent = "GitHub repository workflow for ryan-s-roberts/tool-test: create an issue, list/read existing labels, create a branch, create a file on that branch, add labels to the issue, create a pull request, add labels to the PR, and add a comment to the issue.";
    let allowed: Vec<String> = discovery_eval_catalog_ids();
    let bundles = retrieve_entity_candidate_bundles(
        &reg,
        intent,
        Some(&allowed),
        Default::default(),
        &repo_workflow_class(),
        &["github".into()],
    )
    .expect("retrieve bundles");
    assert_eq!(
        bundles
            .catalog_context
            .entity_seed_class("github", "IssueComment"),
        Some(crate::schema::DiscoverySeedClass::Dependent),
        "IssueComment must stamp seed_class=dependent"
    );
    let github: Vec<&str> = bundles
        .bundles
        .iter()
        .filter(|b| b.entry_id == "github")
        .map(|b| b.entity.as_str())
        .collect();
    for need in ["Label", "IssueComment", "Branch", "Repository", "Issue"] {
        assert!(
            github.contains(&need),
            "expected github:{need} in pool for tool-test intent, got {github:?}"
        );
    }
}

fn bundle(entry: &str, entity: &str, score: u32) -> EntityCandidateBundle {
    EntityCandidateBundle {
        candidate_id: format!("{entry}:{entity}"),
        entry_id: entry.into(),
        entity: entity.into(),
        entity_description: String::new(),
        max_lexical_score: score,
        capabilities: vec![],
        relation_hints: String::new(),
        catalog_route_evidence: true,
    }
}

#[test]
fn readmit_scored_drops_keeps_issue_under_pr_noise() {
    // Diversify with max_per_catalog=4 keeps PR siblings and drops Issue (score 3).
    let pre = vec![
        bundle("github", "PullRequest", 5),
        bundle("github", "PullRequestReview", 4),
        bundle("github", "PullRequestComment", 4),
        bundle("github", "Release", 3),
        bundle("github", "Issue", 3),
        bundle("github", "Label", 2),
        bundle("github", "IssueComment", 0), // inject neighbor — not readmitted
    ];
    let diversified = diversify_entity_bundles(
        pre.clone(),
        EntityCandidateConfig {
            max_entities: 24,
            max_per_catalog: 4,
            ..Default::default()
        },
    );
    assert!(
        diversified.iter().any(|b| b.entity == "Issue"),
        "Issue must be readmitted: {:?}",
        diversified.iter().map(|b| &b.entity).collect::<Vec<_>>()
    );
    assert!(
        !diversified.iter().any(|b| b.entity == "IssueComment"),
        "score-0 inject neighbor must not be readmitted"
    );
}

#[test]
fn readmit_after_merge_required_restores_scored_message() {
    let pre = vec![
        bundle("gmail", "Thread", 4),
        bundle("gmail", "Label", 3),
        bundle("gmail", "Draft", 2),
        bundle("gmail", "MailboxSnapshot", 2),
        bundle("gmail", "Message", 3),
    ];
    let diversified = diversify_entity_bundles(
        pre.clone(),
        EntityCandidateConfig {
            max_entities: 24,
            max_per_catalog: 4,
            ..Default::default()
        },
    );
    // Simulate workflow inject evicting Message for a required root.
    let without_message: Vec<_> = diversified
        .into_iter()
        .filter(|b| b.entity != "Message")
        .collect();
    let restored = readmit_scored_entity_drops(
        without_message,
        &pre,
        EntityCandidateConfig {
            max_entities: 24,
            max_per_catalog: 4,
            ..Default::default()
        },
    );
    assert!(
        restored.iter().any(|b| b.entity == "Message"),
        "Message restored: {:?}",
        restored.iter().map(|b| &b.entity).collect::<Vec<_>>()
    );
}

#[test]
fn inject_phrase_adds_issuecomment_when_pool_has_parents() {
    use super::helpers::ArcCgs;
    use super::inject_phrase::inject_phrase_named_leaves;
    use super::types::EntityCandidateBundle;
    use crate::discovery::CapabilityQuery;
    use indexmap::IndexMap;
    let Some(reg) = load_multi_catalog_registry(&discovery_eval_catalog_ids()) else {
        return;
    };
    let intent = "GitHub repository workflow for ryan-s-roberts/tool-test: create an issue, list/read existing labels, create a branch, create a file on that branch, add labels to the issue, create a pull request, add labels to the PR, and add a comment to the issue.";
    let ctx = reg.load_context("github").expect("github");
    let cgs: ArcCgs = ctx.cgs;
    let mut catalogs: IndexMap<String, ArcCgs> = IndexMap::new();
    catalogs.insert("github".into(), cgs.clone());
    let discovery = reg
        .discover(&CapabilityQuery {
            phrases: vec![intent.into()],
            entry_ids: Some(vec!["github".into()]),
            ..Default::default()
        })
        .expect("discover");
    let map: std::collections::HashMap<String, &crate::schema::CGS> = catalogs
        .iter()
        .map(|(k, v)| (k.clone(), v.as_ref()))
        .collect();
    let catalog_context = crate::discovery_seed_catalog::CatalogWorkflowContext::build(
        &map,
        intent,
        &repo_workflow_class(),
        &["github".into()],
    );
    let mut pool = IndexMap::new();
    for entity in ["Issue", "PullRequest", "Repository"] {
        pool.insert(
            ("github".into(), entity.into()),
            EntityCandidateBundle {
                candidate_id: format!("github:{entity}"),
                entry_id: "github".into(),
                entity: entity.into(),
                entity_description: String::new(),
                max_lexical_score: 10,
                capabilities: vec![],
                relation_hints: String::new(),
                catalog_route_evidence: true,
            },
        );
    }
    inject_phrase_named_leaves(&mut pool, &catalogs, intent, &discovery, &catalog_context);
    let ents: Vec<_> = pool.keys().map(|(_, e)| e.as_str()).collect();
    assert!(
        ents.contains(&"IssueComment"),
        "phrase inject must add IssueComment, got {ents:?}"
    );
    assert!(
        ents.contains(&"Label"),
        "phrase inject must add/boost Label, got {ents:?}"
    );
    assert!(
        ents.contains(&"Branch"),
        "phrase inject must add Branch, got {ents:?}"
    );
}
