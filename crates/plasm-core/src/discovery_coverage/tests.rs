//! Coverage model unit tests (catalog phrase + brand mention).

use std::path::PathBuf;

use indexmap::IndexMap;

use crate::discovery_coverage::{
    coverage_entity_recall, coverage_plan_recall, derive_coverage_plan, evaluate_coverage,
    route_coverage, run_coverage_pipeline, CoverageRoute, ProviderConstraint, RequirementSlot,
};
use crate::loader::load_schema_dir;

fn prompt_matrix_catalogs() -> IndexMap<String, crate::schema::CGS> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).expect("prompt matrix");
    let mut out = IndexMap::new();
    out.insert("prompt_matrix".into(), cgs);
    out
}

fn load_api_pair(a: &str, b: &str) -> Option<(IndexMap<String, crate::schema::CGS>, Vec<String>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../apis");
    if !dir.exists() {
        return None;
    }
    let path_a = dir.join(a);
    let path_b = dir.join(b);
    if !path_a.exists() || !path_b.exists() {
        return None;
    }
    let mut catalogs = IndexMap::new();
    catalogs.insert(a.into(), load_schema_dir(&path_a).ok()?);
    catalogs.insert(b.into(), load_schema_dir(&path_b).ok()?);
    Some((catalogs, vec![a.into(), b.into()]))
}

#[test]
fn selection_matches_gold_empty_acceptable_is_false() {
    use crate::discovery_auto_seed::EntityCandidateBundle;
    use crate::discovery_coverage::{selection_matches_gold, SeedSatisfiability};
    let seed = SeedSatisfiability {
        entry_id: "github".into(),
        entity: "Issue".into(),
        candidate_id: "github:Issue".into(),
        lexical_score: 1,
        catalog_route_evidence: false,
        direct_slots: vec![0],
        via_relation_slots: vec![],
        bundle: EntityCandidateBundle {
            entry_id: "github".into(),
            entity: "Issue".into(),
            candidate_id: "github:Issue".into(),
            entity_description: String::new(),
            max_lexical_score: 1,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: false,
        },
    };
    assert!(!selection_matches_gold(std::slice::from_ref(&seed), &[]));
}

#[test]
fn format_coverage_reasoning_includes_slots_and_plan() {
    use crate::discovery_coverage::{
        format_coverage_reasoning, CoverageEvaluation, CoverageRoute, DiscoveryCoveragePlan,
        ProviderAmbiguity, ProviderConstraint, RequirementSlot, SeedPlan, SeedSatisfiability,
    };
    use crate::discovery_auto_seed::EntityCandidateBundle;

    let bundle = EntityCandidateBundle {
        entry_id: "github".into(),
        entity: "Issue".into(),
        candidate_id: "github:Issue".into(),
        entity_description: String::new(),
        max_lexical_score: 5,
        capabilities: vec![],
        relation_hints: String::new(),
        catalog_route_evidence: true,
    };
    let seed = SeedSatisfiability {
        entry_id: bundle.entry_id.clone(),
        entity: bundle.entity.clone(),
        candidate_id: bundle.candidate_id.clone(),
        lexical_score: 5,
        catalog_route_evidence: true,
        direct_slots: vec![0],
        via_relation_slots: vec![],
        bundle: bundle.clone(),
    };
    let plan = SeedPlan {
        seeds: vec![seed.clone()],
        covers: vec![0],
        lexical_score: 5,
        slot_signature: "read:Issue".into(),
    };
    let evaluation = CoverageEvaluation {
        plan: DiscoveryCoveragePlan {
            slots: vec![RequirementSlot::ReadRoot {
                entity_hint: Some("Issue".into()),
            }],
            provider_constraint: ProviderConstraint::Locked(vec!["github".into()]),
            catalog_route: vec!["github".into()],
        },
        satisfiable_plans_by_provider: Default::default(),
        satisfiable_by_provider: Default::default(),
        satisfiable_federation_tuples: Vec::new(),
        uncovered: Vec::new(),
        ambiguity: ProviderAmbiguity::None,
        bundles: vec![bundle],
    };
    let route = CoverageRoute::Select {
        selected: vec![seed],
        provider: "github".into(),
        tie_candidates: vec![],
        plan,
    };
    let reasoning = format_coverage_reasoning(&evaluation, &route, None, None);
    assert!(reasoning.contains("route=ready"));
    assert!(reasoning.contains("slots=["));
    assert!(reasoning.contains("github:Issue"));
}

#[test]
fn derive_unbranded_read_intent_has_read_slot() {
    let catalogs = prompt_matrix_catalogs();
    let plan = derive_coverage_plan(
        "list issues with labels",
        &catalogs,
        &["prompt_matrix".into()],
        &["prompt_matrix".into()],
    );
    assert!(matches!(
        plan.provider_constraint,
        ProviderConstraint::Unbranded
    ));
    assert!(!plan.slots.is_empty());
    assert!(plan
        .slots
        .iter()
        .any(|slot| matches!(slot, RequirementSlot::ReadRoot { .. })));
}

#[test]
fn brand_lock_from_entry_id_mention() {
    let catalogs = prompt_matrix_catalogs();
    let plan = derive_coverage_plan(
        "use prompt_matrix to triage",
        &catalogs,
        &["prompt_matrix".into()],
        &["prompt_matrix".into()],
    );
    assert!(
        matches!(plan.provider_constraint, ProviderConstraint::Locked(locked) if locked == vec!["prompt_matrix"])
    );
}

#[test]
fn multi_named_catalogs_emit_federate_slots() {
    let Some((catalogs, allowed)) = load_api_pair("github", "clickup") else {
        return;
    };
    let plan = derive_coverage_plan(
        "Link GitHub issues into ClickUp tasks for the sprint",
        &catalogs,
        &allowed,
        &allowed,
    );
    assert!(
        matches!(
            &plan.provider_constraint,
            ProviderConstraint::Locked(ids) if ids.len() == 2
        ),
        "expected dual brand lock, got {:?}",
        plan.provider_constraint
    );
    assert!(
        plan.slots
            .iter()
            .filter(|s| matches!(s, RequirementSlot::FederateSlot { .. }))
            .count()
            >= 2,
        "named multi-catalog without English sync verbs should federate, plan={plan:?}"
    );
}

#[test]
fn unbranded_multi_provider_pr_does_not_soft_ready() {
    let Some((catalogs, allowed)) = load_api_pair("github", "gitlab") else {
        return;
    };
    // Soft vocabulary ("PR"/"pull request") is GitHub-surface; GitLab owns "merge request".
    // Phrase-gated retrieve may uniquely ready GitHub when only one catalog admits the phrase.
    // Cross-vendor clarify requires competing phrase evidence or brand.
    let intent = "Who still needs to review the open pull requests on the release branch?";
    let evaluation = evaluate_coverage(intent, &catalogs, &allowed, &allowed);
    let route = route_coverage(&evaluation);
    match &route {
        CoverageRoute::Clarify { .. } | CoverageRoute::HardMiss { .. } => {}
        CoverageRoute::Select { provider, selected, .. } => {
            assert_eq!(
                provider, "github",
                "if Select fires under phrase gate, it must be github PR surface, got {route:?}"
            );
            assert!(
                selected.iter().all(|s| s.entry_id == "github"),
                "must not soft-ready gitlab under pull-request vocabulary, got {route:?}"
            );
            assert!(
                evaluation.bundles.len() <= 24,
                "phrase-gated pool must stay bounded, got {}",
                evaluation.bundles.len()
            );
            assert!(
                evaluation.bundles.iter().filter(|b| b.entry_id == "gitlab").count() <= 8,
                "must not dump gitlab schema beside github hits; got {}",
                evaluation.bundles.len()
            );
        }
    }
}

#[test]
fn can_ready_false_when_unbranded_ungrounded_multi_provider() {
    use crate::discovery_coverage::can_ready;

    let Some((catalogs, allowed)) = load_api_pair("github", "gitlab") else {
        return;
    };
    // OOD / ungrounded intent — phrase gate should abstain, not ready via schema dump.
    let intent = "Ship the hotfix once the release checklist is green.";
    let evaluation = evaluate_coverage(intent, &catalogs, &allowed, &allowed);
    assert!(
        !can_ready(&evaluation),
        "ungrounded unbranded paraphrase must not ready; bundles={} route_plans={:?}",
        evaluation.bundles.len(),
        evaluation
            .satisfiable_plans_by_provider
            .keys()
            .collect::<Vec<_>>()
    );
}

#[test]
fn neighbor_pin_with_slack_ready_not_clarify() {
    let Some((catalogs, allowed)) = load_api_pair("slack", "microsoft-teams") else {
        return;
    };
    let intent = "Pin the rollout decision message in Slack so the channel keeps it visible.";
    let evaluation = evaluate_coverage(intent, &catalogs, &allowed, &allowed);
    let route = route_coverage(&evaluation);
    assert!(
        matches!(route, CoverageRoute::Select { ref provider, .. } if provider == "slack"),
        "expected slack ready select, got {route:?}"
    );
}

#[test]
fn ground_slots_drops_ungrounded_invented_hint() {
    use crate::discovery_coverage::{ground_slots, DiscoveryCoveragePlan};

    let catalogs = prompt_matrix_catalogs();
    let allowed = vec!["prompt_matrix".into()];
    let plan = DiscoveryCoveragePlan {
        slots: vec![RequirementSlot::ReadRoot {
            entity_hint: Some("Issue".into()),
        }],
        provider_constraint: ProviderConstraint::Unbranded,
        catalog_route: allowed.clone(),
    };
    let grounded = ground_slots(
        &plan,
        "show the weekly digest for tonight",
        &catalogs,
        &allowed,
    );
    let root = grounded.slots.iter().find_map(|slot| match slot {
        RequirementSlot::ReadRoot { entity_hint } => entity_hint.clone(),
        _ => None,
    });
    assert!(
        root.is_none(),
        "expected ungrounded invented Issue hint cleared, got {root:?}"
    );
}

#[test]
fn brand_locked_grounded_hint_survives() {
    use crate::discovery_coverage::{ground_slots, DiscoveryCoveragePlan};

    let Some((catalogs, allowed)) = load_api_pair("slack", "microsoft-teams") else {
        return;
    };
    let plan = DiscoveryCoveragePlan {
        slots: vec![RequirementSlot::MutateAnchor {
            op: crate::schema::CapabilityKind::Action,
            entity_hint: Some("Message".into()),
        }],
        provider_constraint: ProviderConstraint::Locked(vec!["slack".into()]),
        catalog_route: allowed.clone(),
    };
    let grounded = ground_slots(
        &plan,
        "Pin the rollout decision message in Slack",
        &catalogs,
        &allowed,
    );
    let root = grounded.slots.iter().find_map(|slot| match slot {
        RequirementSlot::MutateAnchor { entity_hint, .. } => entity_hint.clone(),
        _ => None,
    });
    assert_eq!(root.as_deref(), Some("Message"));
}

#[test]
fn unbranded_weak_entity_hits_omit_root_hint() {
    let catalogs = prompt_matrix_catalogs();
    let plan = derive_coverage_plan(
        "show the things we need for tonight",
        &catalogs,
        &["prompt_matrix".into()],
        &["prompt_matrix".into()],
    );
    let root = plan.slots.iter().find_map(|slot| match slot {
        RequirementSlot::ReadRoot { entity_hint } => entity_hint.as_ref(),
        _ => None,
    });
    assert!(
        root.is_none(),
        "expected empty root hint under evidence floor, got {root:?} plan={plan:?}"
    );
}

#[test]
fn branded_intent_keeps_root_hint_despite_competition() {
    let Some((catalogs, allowed)) = load_api_pair("gmail", "outlook") else {
        return;
    };
    let plan = derive_coverage_plan(
        "Show unread Gmail messages from the CFO this week.",
        &catalogs,
        &allowed,
        &allowed,
    );
    assert!(
        matches!(
            plan.provider_constraint,
            ProviderConstraint::Locked(ref locked) if locked == &["gmail".to_string()]
        ),
        "expected gmail lock, got {:?}",
        plan.provider_constraint
    );
    let root = plan.slots.iter().find_map(|slot| match slot {
        RequirementSlot::ReadRoot { entity_hint } => entity_hint.clone(),
        _ => None,
    });
    let gmail = catalogs.get("gmail").expect("gmail");
    assert!(
        root.as_ref()
            .is_some_and(|name| gmail.entities.contains_key(name.as_str())),
        "expected grounded gmail entity hint, got {root:?}"
    );
}

#[test]
fn multi_catalog_lock_without_federation_clarifies() {
    let Some((catalogs, allowed)) = load_api_pair("gitlab", "jira") else {
        return;
    };
    let intent = "Link GitLab merge requests to Jira release-note issues for the 2.4 launch.";
    let evaluation = evaluate_coverage(intent, &catalogs, &allowed, &allowed);
    let route = route_coverage(&evaluation);
    assert!(
        !matches!(route, CoverageRoute::Select { .. })
            || matches!(
                &evaluation.plan.provider_constraint,
                ProviderConstraint::Locked(ids) if ids.len() == 1
            ),
        "expected clarify/hard_miss or true federation ready, got {route:?} lock={:?}",
        evaluation.plan.provider_constraint
    );
    if evaluation.satisfiable_federation_tuples.is_empty()
        && matches!(
            evaluation.plan.provider_constraint,
            ProviderConstraint::Locked(ref ids) if ids.len() > 1
        )
    {
        assert!(
            matches!(route, CoverageRoute::Clarify { .. } | CoverageRoute::HardMiss { .. }),
            "incomplete federation must not soft-ready, got {route:?}"
        );
    }
}

#[test]
fn holdout_unbranded_inbox_still_clarifies() {
    let Some((catalogs, allowed)) = load_api_pair("gmail", "outlook") else {
        return;
    };
    let intent = "Show unread messages in my work inbox from finance this week.";
    let evaluation = evaluate_coverage(intent, &catalogs, &allowed, &allowed);
    let route = route_coverage(&evaluation);
    assert!(
        matches!(route, CoverageRoute::Clarify { .. }),
        "expected clarify for unbranded inbox, got {route:?}"
    );
}

#[test]
fn plan_recall_accepts_multi_entity_gold_tuple() {
    let catalogs = prompt_matrix_catalogs();
    let pipeline = run_coverage_pipeline(
        "list issues with labels",
        &catalogs,
        &["prompt_matrix".into()],
    );
    let gold = vec![vec![
        ("prompt_matrix".into(), "Issue".into()),
        ("prompt_matrix".into(), "Label".into()),
    ]];
    let _ = coverage_entity_recall(&pipeline.evaluation, &gold);
    let issue_only = vec![vec![("prompt_matrix".into(), "Issue".into())]];
    let _ = coverage_plan_recall(&pipeline.evaluation, &issue_only);
}

#[test]
fn ood_intent_does_not_brand_lock_without_entry_id() {
    let catalogs = prompt_matrix_catalogs();
    let plan = derive_coverage_plan(
        "Rotate the Kubernetes etcd encryption keys for our on-prem cluster.",
        &catalogs,
        &["prompt_matrix".into()],
        &["prompt_matrix".into()],
    );
    assert!(
        matches!(plan.provider_constraint, ProviderConstraint::Unbranded),
        "OOD product names must not invent a brand lock, got {:?}",
        plan.provider_constraint
    );
    let _ = run_coverage_pipeline(
        "Rotate the Kubernetes etcd encryption keys for our on-prem cluster.",
        &catalogs,
        &["prompt_matrix".into()],
    );
}

#[test]
fn enumerate_is_phrase_gated_not_federated_schema_dump() {
    use crate::discovery_coverage::enumerate_schema_bundles;

    let Some((catalogs, allowed)) = load_api_pair("clickup", "github") else {
        return;
    };
    let k8s = "Deploy the monorepo to production Kubernetes and rotate TLS certificates.";
    let bundles = enumerate_schema_bundles(k8s, &catalogs, &allowed, &[]);
    assert!(
        !bundles.iter().any(|b| b.entry_id == "clickup"),
        "k8s OOD must not dump ClickUp entities; got {:?}",
        bundles
            .iter()
            .filter(|b| b.entry_id == "clickup")
            .map(|b| b.entity.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        bundles.len() <= 24,
        "diversified pool must stay bounded, got {}",
        bundles.len()
    );

    let pin = "Pin the rollout decision summary in the engineering Slack channel.";
    let Some((slack_gh, allowed_sg)) = load_api_pair("slack", "github") else {
        return;
    };
    let pin_bundles = enumerate_schema_bundles(pin, &slack_gh, &allowed_sg, &["slack".into()]);
    assert!(
        pin_bundles.iter().any(|b| b.entry_id == "slack" && b.entity == "Pin"),
        "pin intent must retrieve Slack Pin; pool={:?}",
        pin_bundles
            .iter()
            .map(|b| format!("{}:{}", b.entry_id, b.entity))
            .collect::<Vec<_>>()
    );
    assert!(
        pin_bundles.len() < 40,
        "pin pool must not dump federated schema, got {}",
        pin_bundles.len()
    );
}

#[test]
fn eval_case_intents_compile_coverage_pipeline() {
    let cases_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/discovery-eval");
    for name in [
        "cases.yaml",
        "cases-holdout.yaml",
        "cases-neighbor-minimality.yaml",
    ] {
        let path = cases_dir.join(name);
        if !path.exists() {
            continue;
        }
        let raw = std::fs::read_to_string(&path).expect("read cases");
        let cases: Vec<serde_yaml::Value> = serde_yaml::from_str(&raw).expect("parse cases");
        for case in cases {
            let intent = case
                .get("intent")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if intent.is_empty() {
                continue;
            }
            let catalogs = prompt_matrix_catalogs();
            let _ = run_coverage_pipeline(&intent, &catalogs, &["prompt_matrix".into()]);
        }
    }
}
