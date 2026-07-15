use super::*;
use std::collections::{BTreeSet, HashMap};

use crate::discovery_seed_witness::corpus::{RequirementWitness, WitnessCorpus, WitnessKind};
use crate::discovery_seed_witness::role_index::CorpusRoleIndex;
use crate::discovery_seed_witness::roles::{
    OwnEdge, OwnPairs, PoolChild, PoolLinks, SeedClassStamp, SeedNavStamp,
};
use crate::schema::{DiscoverySeedClass, DiscoverySeedNav};

fn class(s: &str) -> SeedClassStamp {
    match s {
        "primary" => SeedClassStamp::Authored(DiscoverySeedClass::Primary),
        "dependent" => SeedClassStamp::Authored(DiscoverySeedClass::Dependent),
        "ambient" => SeedClassStamp::Authored(DiscoverySeedClass::Ambient),
        _ => SeedClassStamp::Unset,
    }
}

fn nav(s: &str) -> SeedNavStamp {
    match s {
        "attach" => SeedNavStamp::Authored(DiscoverySeedNav::Attach),
        "own" => SeedNavStamp::Authored(DiscoverySeedNav::Own),
        "locate" => SeedNavStamp::Authored(DiscoverySeedNav::Locate),
        _ => SeedNavStamp::Unset,
    }
}

fn pool_from_note(graph_note: &str) -> PoolLinks {
    let mut parents = BTreeSet::new();
    let mut children = BTreeSet::new();
    for part in graph_note.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("relation_child_of=") {
            for p in rest.split('|').map(str::trim).filter(|s| !s.is_empty()) {
                parents.insert(p.to_string());
            }
        } else if let Some(rest) = part.strip_prefix("relation_anchor_to=") {
            for item in rest.split('|').map(str::trim).filter(|s| !s.is_empty()) {
                if let Some((wire, target)) = item.split_once(':') {
                    children.insert(PoolChild::new(wire, target));
                } else {
                    children.insert(PoolChild::new("", item));
                }
            }
        }
    }
    PoolLinks {
        parents,
        children,
        siblings: BTreeSet::new(),
    }
}

fn own_from_label(own_pair: &str) -> OwnPairs {
    if own_pair.is_empty() || own_pair == "unset" {
        return OwnPairs::default();
    }
    OwnPairs::new(own_pair.split('|').filter_map(|part| {
        let part = part.trim();
        let (source, target) = part.split_once('→').or_else(|| part.split_once("->"))?;
        Some(OwnEdge::new(source.trim(), target.trim()))
    }))
}

fn direct(
    entry: &str,
    entity: &str,
    kind: &str,
    seed_class: &str,
    seed_nav: &str,
    graph_note: &str,
    score: u32,
) -> RequirementWitness {
    direct_with_own(
        entry, entity, kind, seed_class, seed_nav, "unset", graph_note, score,
    )
}

#[allow(clippy::too_many_arguments)]
fn direct_with_own(
    entry: &str,
    entity: &str,
    kind: &str,
    seed_class: &str,
    seed_nav: &str,
    own_pair: &str,
    graph_note: &str,
    score: u32,
) -> RequirementWitness {
    RequirementWitness {
        symbol: String::new(),
        kind: WitnessKind::DirectCapability {
            entry_id: entry.into(),
            entity: entity.into(),
            capability_id: format!("{entry}:{entity}:{kind}"),
            capability_name: format!("{entity}_{kind}"),
            kind: kind.into(),
            description: format!("{kind} {entity}"),
        },
        owner_candidate_id: format!("{entry}:{entity}"),
        lexical_score: score,
        summary: format!("{kind} {entity}"),
        entity_description: format!("{entity} desc"),
        aliases: entity.to_ascii_lowercase(),
        pool: pool_from_note(graph_note),
        seed_class: class(seed_class),
        seed_nav: nav(seed_nav),
        own_pairs: own_from_label(own_pair),
    }
}

fn corpus(mut witnesses: Vec<RequirementWitness>) -> WitnessCorpus {
    let mut symbol_to_index = HashMap::new();
    for (idx, w) in witnesses.iter_mut().enumerate() {
        w.symbol = format!("w{}", idx + 1);
        symbol_to_index.insert(w.symbol.clone(), idx);
    }
    WitnessCorpus {
        roles: CorpusRoleIndex::build(&witnesses),
        witnesses,
        bundles: vec![],
        brand_lock_catalogs: vec![],
        symbol_to_index,
    }
}

/// Same as [`corpus`] but installs pool bundles so ParentPreferred cover can resolve owners.
fn corpus_for_plans(witnesses: Vec<RequirementWitness>) -> WitnessCorpus {
    use crate::discovery_auto_seed::EntityCandidateBundle;
    let mut corpus = corpus(witnesses);
    let mut seen = BTreeSet::new();
    for w in &corpus.witnesses {
        if !seen.insert(w.owner_candidate_id.clone()) {
            continue;
        }
        let Some((entry_id, entity)) = w.owner_candidate_id.split_once(':') else {
            continue;
        };
        corpus.bundles.push(EntityCandidateBundle {
            candidate_id: w.owner_candidate_id.clone(),
            entry_id: entry_id.into(),
            entity: entity.into(),
            entity_description: format!("{entity} desc"),
            max_lexical_score: w.lexical_score,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: true,
        });
    }
    corpus
}

#[test]
fn drops_attach_when_parent_also_selected() {
    let corpus = corpus(vec![
        direct(
            "fx",
            "Issue",
            "Query",
            "primary",
            "unset",
            "relation_anchor_to=comments:Comment",
            80,
        ),
        direct(
            "fx",
            "Comment",
            "Query",
            "dependent",
            "attach",
            "relation_child_of=Issue",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1], IntentGate::Ungated);
    assert_eq!(pruned, vec![0]);
}

#[test]
fn drops_attach_label_read_when_issue_selected() {
    let corpus = corpus(vec![
        direct(
            "fx",
            "Issue",
            "Query",
            "primary",
            "unset",
            "relation_anchor_to=labels:Label",
            80,
        ),
        direct(
            "fx",
            "Label",
            "Query",
            "dependent",
            "attach",
            "relation_child_of=Issue",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1], IntentGate::Ungated);
    assert_eq!(pruned, vec![0]);
}

#[test]
fn promotes_orphan_attach_read_to_parent() {
    let corpus = corpus(vec![
        direct(
            "fx",
            "Issue",
            "Query",
            "primary",
            "unset",
            "relation_anchor_to=comments:Comment",
            80,
        ),
        direct(
            "fx",
            "Comment",
            "Query",
            "dependent",
            "attach",
            "relation_child_of=Issue",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[1], IntentGate::Ungated);
    assert_eq!(pruned, vec![0]);
}

#[test]
fn drops_ambient_when_child_direct_selected() {
    let corpus = corpus(vec![
        direct(
            "fx",
            "Repository",
            "Query",
            "ambient",
            "locate",
            "relation_anchor_to=issues:Issue",
            50,
        ),
        direct(
            "fx",
            "Issue",
            "Query",
            "primary",
            "unset",
            "relation_child_of=Repository",
            80,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1], IntentGate::Ungated);
    assert_eq!(pruned, vec![1]);
}

#[test]
fn drops_ambient_when_same_catalog_primary_without_edge() {
    let corpus = corpus(vec![
        direct("fx", "Repository", "Query", "ambient", "unset", "", 50),
        direct("fx", "Issue", "Query", "primary", "unset", "", 80),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1], IntentGate::Ungated);
    assert_eq!(pruned, vec![1]);
}

#[test]
fn prefer_lone_attach_mutate_drops_co_selected_parent() {
    let corpus = corpus(vec![
        direct(
            "fx",
            "Issue",
            "Query",
            "primary",
            "unset",
            "relation_anchor_to=comments:Comment",
            80,
        ),
        direct(
            "fx",
            "Comment",
            "Create",
            "dependent",
            "attach",
            "relation_child_of=Issue",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![1],
        "lone Create+parent → keep Create only (localized mutate)"
    );
}

#[test]
fn own_edge_xor_keeps_source_drops_target_read() {
    let corpus = corpus(vec![
        direct_with_own(
            "fx",
            "Channel",
            "Query",
            "primary",
            "unset",
            "Channel→Message",
            "relation_anchor_to=messages:Message",
            80,
        ),
        direct_with_own(
            "fx",
            "Message",
            "Query",
            "primary",
            "own",
            "Channel→Message",
            "relation_child_of=Channel",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1], IntentGate::Ungated);
    assert_eq!(pruned, vec![0], "own XOR keeps Source read only");
}

#[test]
fn own_edge_xor_keeps_target_mutate_with_source() {
    let corpus = corpus(vec![
        direct_with_own(
            "fx",
            "Channel",
            "Query",
            "primary",
            "unset",
            "Channel→Message",
            "relation_anchor_to=messages:Message",
            80,
        ),
        direct_with_own(
            "fx",
            "Message",
            "Create",
            "primary",
            "own",
            "Channel→Message",
            "relation_child_of=Channel",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1], IntentGate::Ungated);
    assert_eq!(pruned, vec![0, 1], "Target mutate survives own XOR");
}

#[test]
fn promotes_orphan_attach_per_catalog_under_federation() {
    // Federated primary in catalog A must not veto orphan attach promote in catalog B.
    let corpus = corpus(vec![
        direct("vcs", "PullRequest", "Query", "primary", "unset", "", 90),
        direct(
            "tracker",
            "Issue",
            "Query",
            "primary",
            "unset",
            "relation_anchor_to=browse_link:BrowseLink",
            80,
        ),
        direct(
            "tracker",
            "BrowseLink",
            "Query",
            "dependent",
            "attach",
            "relation_child_of=Issue",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 2], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![0, 1],
        "orphan BrowseLink promotes to Issue while keeping federated PullRequest"
    );
}

#[test]
fn drops_ambient_container_when_primary_channel_selected() {
    let corpus = corpus(vec![
        direct(
            "comms",
            "Team",
            "Query",
            "ambient",
            "locate",
            "relation_anchor_to=channels:Channel",
            50,
        ),
        direct(
            "comms",
            "Channel",
            "Query",
            "primary",
            "unset",
            "relation_child_of=Team",
            80,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![1],
        "ambient Team dropped beside Channel primary"
    );
}

#[test]
fn drops_ambient_snapshot_when_issue_primary_selected() {
    let corpus = corpus(vec![
        direct(
            "tracker",
            "SprintBoardSnapshot",
            "Query",
            "ambient",
            "unset",
            "relation_anchor_to=issues:Issue",
            60,
        ),
        direct(
            "tracker",
            "Issue",
            "Query",
            "primary",
            "unset",
            "relation_child_of=SprintBoardSnapshot",
            80,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![1],
        "ambient snapshot dropped beside Issue primary"
    );
}

#[test]
fn promotes_own_target_query_to_source() {
    let corpus = corpus(vec![
        direct_with_own(
            "comms",
            "Channel",
            "Query",
            "primary",
            "unset",
            "Channel→Message",
            "relation_anchor_to=messages:Message",
            80,
        ),
        direct_with_own(
            "comms",
            "Message",
            "Query",
            "primary",
            "own",
            "Channel→Message",
            "relation_child_of=Channel",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[1], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![0],
        "orphan own-target Query promotes to Channel"
    );
}

#[test]
fn own_target_search_stays_on_target() {
    let corpus = corpus(vec![
        direct_with_own(
            "comms",
            "Channel",
            "Query",
            "primary",
            "unset",
            "Channel→Message",
            "relation_anchor_to=messages:Message",
            80,
        ),
        direct_with_own(
            "comms",
            "Message",
            "Search",
            "primary",
            "own",
            "Channel→Message",
            "relation_child_of=Channel",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[1], IntentGate::Ungated);
    assert_eq!(pruned, vec![1], "own-target Search stays target-seated");
}

#[test]
fn own_target_create_stays_on_target() {
    let corpus = corpus(vec![
        direct_with_own(
            "comms",
            "Channel",
            "Query",
            "primary",
            "unset",
            "Channel→Message",
            "relation_anchor_to=messages:Message",
            80,
        ),
        direct_with_own(
            "comms",
            "Message",
            "Create",
            "primary",
            "own",
            "Channel→Message",
            "relation_child_of=Channel",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[1], IntentGate::Ungated);
    assert_eq!(pruned, vec![1], "own-target Create stays target-seated");
}

#[test]
fn promotes_shared_attach_create_update_to_parent() {
    let corpus = corpus(vec![
        direct("vcs", "Repository", "Query", "primary", "unset", "", 90),
        direct(
            "vcs",
            "Issue",
            "Query",
            "primary",
            "unset",
            "relation_anchor_to=comments:Comment|labels:Label",
            80,
        ),
        direct(
            "vcs",
            "IssueComment",
            "Create",
            "dependent",
            "attach",
            "relation_child_of=Issue",
            70,
        ),
        direct(
            "vcs",
            "Label",
            "Update",
            "dependent",
            "attach",
            "relation_child_of=Issue",
            65,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 2, 3], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![0, 1],
        "two attach Create/Update leaves share Issue parent → promote Issue, keep Repository"
    );
}

#[test]
fn single_attach_create_stays_localized() {
    let corpus = corpus(vec![
        direct(
            "tracker",
            "Issue",
            "Query",
            "primary",
            "unset",
            "relation_anchor_to=comments:Comment",
            80,
        ),
        direct(
            "tracker",
            "Comment",
            "Create",
            "dependent",
            "attach",
            "relation_child_of=Issue",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[1], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![1],
        "single attach Create remains localized leaf"
    );
}

#[test]
fn attach_action_with_create_does_not_batch_promote() {
    // Action leaves are excluded from shared Create/Update promotion.
    let corpus = corpus(vec![
        direct(
            "comms",
            "Channel",
            "Query",
            "primary",
            "unset",
            "relation_anchor_to=pins:Pin",
            80,
        ),
        direct(
            "comms",
            "Pin",
            "Action",
            "dependent",
            "attach",
            "relation_child_of=Channel",
            70,
        ),
        direct(
            "comms",
            "Note",
            "Create",
            "dependent",
            "attach",
            "relation_child_of=Channel",
            65,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[1, 2], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![1, 2],
        "Action + single Create do not trigger shared-mutation promote (<2 Create/Update)"
    );
}

#[test]
fn own_promote_per_catalog_under_federation() {
    let corpus = corpus(vec![
        direct("tracker", "Issue", "Query", "primary", "unset", "", 90),
        direct_with_own(
            "comms",
            "Channel",
            "Query",
            "primary",
            "unset",
            "Channel→Message",
            "relation_anchor_to=messages:Message",
            80,
        ),
        direct_with_own(
            "comms",
            "Message",
            "Query",
            "primary",
            "own",
            "Channel→Message",
            "relation_child_of=Channel",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 2], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![0, 1],
        "own-target Query promotes to Channel while keeping federated Issue"
    );
}

#[test]
fn own_query_promotes_via_pool_parent_without_own_pairs() {
    // seed_nav=own + unique pool parent, even when own_pairs unset (production gap).
    let corpus = corpus(vec![
        direct(
            "comms",
            "Channel",
            "Query",
            "primary",
            "unset",
            "relation_anchor_to=messages:Message",
            80,
        ),
        direct(
            "comms",
            "Message",
            "Query",
            "primary",
            "own",
            "relation_child_of=Channel",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[1], IntentGate::Ungated);
    assert_eq!(pruned, vec![0], "own Query + pool parent → Channel");
}

#[test]
fn prefer_lone_comment_create_over_parent() {
    let corpus = corpus(vec![
        direct(
            "tracker",
            "Task",
            "Query",
            "primary",
            "unset",
            "relation_anchor_to=comments:Comment",
            80,
        ),
        direct(
            "tracker",
            "Comment",
            "Create",
            "dependent",
            "attach",
            "relation_child_of=Task",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1], IntentGate::Ungated);
    assert_eq!(pruned, vec![1], "lone Create beats co-selected parent");
}

#[test]
fn demote_batch_creates_beside_primary() {
    let corpus = corpus(vec![
        direct("vcs", "Repository", "Query", "primary", "unset", "", 90),
        direct(
            "vcs",
            "IssueComment",
            "Create",
            "dependent",
            "attach",
            "relation_child_of=Issue",
            70,
        ),
        direct(
            "vcs",
            "Label",
            "Create",
            "dependent",
            "attach",
            "relation_child_of=Repository",
            65,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1, 2], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![0],
        "≥2 attach Creates beside primary demote to Repository"
    );
}

#[test]
fn prefer_lone_create_drops_parent_create() {
    let corpus = corpus(vec![
        direct(
            "tracker",
            "Task",
            "Create",
            "primary",
            "unset",
            "relation_anchor_to=comments:Comment",
            80,
        ),
        direct(
            "tracker",
            "Comment",
            "Create",
            "dependent",
            "attach",
            "relation_child_of=Task",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![1],
        "parent Create must not steal localized Comment Create"
    );
}

#[test]
fn prefer_lone_create_drops_parent_despite_sibling_leaf_read() {
    let corpus = corpus(vec![
        direct(
            "tracker",
            "Task",
            "Query",
            "primary",
            "unset",
            "relation_anchor_to=comments:Comment",
            80,
        ),
        direct(
            "tracker",
            "Comment",
            "Query",
            "dependent",
            "attach",
            "relation_child_of=Task",
            60,
        ),
        direct(
            "tracker",
            "Comment",
            "Create",
            "dependent",
            "attach",
            "relation_child_of=Task",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1, 2], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![2],
        "Task+Comment Query/Create → Comment Create (attach read xor + prefer-lone)"
    );
}

#[test]
fn demote_lone_ambient_to_owned_primary() {
    let corpus = corpus(vec![
        direct_with_own(
            "tracker",
            "Dashboard",
            "Query",
            "ambient",
            "unset",
            "Dashboard→Issue",
            "relation_anchor_to=items:Issue",
            90,
        ),
        direct_with_own(
            "tracker",
            "Issue",
            "Query",
            "primary",
            "own",
            "Dashboard→Issue",
            "relation_child_of=Dashboard",
            80,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![1],
        "lone ambient own-source demotes to primary child"
    );
}

#[test]
fn own_promote_skips_ambient_source() {
    let corpus = corpus(vec![
        direct(
            "tracker",
            "Dashboard",
            "Query",
            "ambient",
            "unset",
            "relation_anchor_to=items:Issue",
            90,
        ),
        direct_with_own(
            "tracker",
            "Issue",
            "Query",
            "primary",
            "own",
            "Dashboard→Issue",
            "relation_child_of=Dashboard",
            80,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[1], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![1],
        "ambient own-source must not replace primary target"
    );
}

#[test]
fn own_xor_keeps_target_when_source_ambient() {
    let corpus = corpus(vec![
        direct_with_own(
            "tracker",
            "Dashboard",
            "Query",
            "ambient",
            "unset",
            "Dashboard→Issue",
            "relation_anchor_to=items:Issue",
            90,
        ),
        direct_with_own(
            "tracker",
            "Issue",
            "Query",
            "primary",
            "own",
            "Dashboard→Issue",
            "relation_child_of=Dashboard",
            80,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![1],
        "ambient source XOR-drops; primary target remains"
    );
}

#[test]
fn prefer_lone_keeps_parent_update_with_leaf_create() {
    let corpus = corpus(vec![
        direct(
            "tracker",
            "Issue",
            "Update",
            "primary",
            "unset",
            "relation_anchor_to=comments:Comment",
            80,
        ),
        direct(
            "tracker",
            "Comment",
            "Create",
            "dependent",
            "attach",
            "relation_child_of=Issue",
            70,
        ),
    ]);
    let pruned = prune_witness_selection(&corpus, &[0, 1], IntentGate::Ungated);
    assert_eq!(
        pruned,
        vec![0, 1],
        "parent Update + leaf Create is multi-op — keep both"
    );
}

/// `adv_minimality_pr_reviewers_only` FO: co-selected dependent Create must not
/// demote the primary when leaf discovery aliases are absent from the intent.
#[test]
fn prefer_lone_skips_when_leaf_unnamed_in_intent() {
    let mut pr = direct(
        "github",
        "PullRequest",
        "Query",
        "primary",
        "unset",
        "relation_anchor_to=reviews:PullRequestReview",
        90,
    );
    pr.aliases = "pull request|pr".into();
    let mut review = direct(
        "github",
        "PullRequestReview",
        "Create",
        "dependent",
        "attach",
        "relation_child_of=PullRequest",
        70,
    );
    review.aliases = "pull request review|pr review|code review".into();
    let mut review_q = direct(
        "github",
        "PullRequestReview",
        "Query",
        "dependent",
        "attach",
        "relation_child_of=PullRequest",
        65,
    );
    review_q.aliases = review.aliases.clone();
    let corpus = corpus_for_plans(vec![pr, review, review_q]);
    let intent = "List open pull requests on the monorepo and show requested reviewers on each.";
    let pruned = prune_witness_selection(&corpus, &[0, 1, 2], IntentGate::Strict(intent));
    assert!(
        pruned.contains(&0),
        "primary PullRequest Query must survive unnamed leaf Create; got {pruned:?}"
    );
    let plans = crate::discovery_seed_witness::construct_workflow_seed_plans(&corpus, &pruned)
        .expect("plans");
    assert!(
        plans.iter().any(|p| {
            p.entities.len() == 1 && p.entities[0] == ("github".into(), "PullRequest".into())
        }),
        "ParentPreferred seating must yield PullRequest-only plan; plans={plans:?}"
    );
    assert!(
        plans.iter().all(|p| {
            !p.entities
                .iter()
                .any(|(e, ent)| e == "github" && ent == "PullRequestReview")
        }),
        "unnamed leaf Create must not seat PullRequestReview; plans={plans:?}"
    );
    // Ungated prefer_lone still demotes (explicit FO-hazard polarity for unit isolation).
    let demoted = prune_witness_selection(&corpus, &[0, 1, 2], IntentGate::Ungated);
    assert!(
        !demoted.contains(&0),
        "Ungated prefer_lone drops parent Query; got {demoted:?}"
    );
}

#[test]
fn prefer_lone_still_demotes_when_leaf_named_in_intent() {
    let mut issue = direct(
        "tracker",
        "Task",
        "Query",
        "primary",
        "unset",
        "relation_anchor_to=comments:Comment",
        80,
    );
    issue.aliases = "task|issue".into();
    let mut comment = direct(
        "tracker",
        "Comment",
        "Create",
        "dependent",
        "attach",
        "relation_child_of=Task",
        70,
    );
    comment.aliases = "comment|issue comment".into();
    let corpus = corpus(vec![issue, comment]);
    let pruned = prune_witness_selection(
        &corpus,
        &[0, 1],
        IntentGate::Strict("Add a comment on the task describing the outage"),
    );
    assert_eq!(pruned, vec![1], "named leaf Create still demotes parent");
}
