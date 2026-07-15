use super::*;
use crate::discovery_auto_seed::EntityCapabilityEvidence;
use crate::discovery_candidate_graph::TypedCandidateGraph;
use crate::discovery_seed_witness::MAX_WITNESS_CATALOGS_UNBRANDED;

fn cap(id: &str, name: &str, kind: &str, score: u32) -> EntityCapabilityEvidence {
    EntityCapabilityEvidence {
        capability_id: id.into(),
        capability_name: name.into(),
        kind: kind.into(),
        description: format!("{kind} {name}"),
        reason_codes: vec![],
        lexical_score: score,
    }
}

fn bundle(
    id: &str,
    catalog: &str,
    entity: &str,
    caps: Vec<EntityCapabilityEvidence>,
    score: u32,
) -> crate::discovery_auto_seed::EntityCandidateBundle {
    crate::discovery_auto_seed::EntityCandidateBundle {
        candidate_id: id.into(),
        entry_id: catalog.into(),
        entity: entity.into(),
        entity_description: format!("{entity} desc"),
        max_lexical_score: score,
        capabilities: caps,
        relation_hints: String::new(),
        catalog_route_evidence: true,
    }
}

fn empty_graph(
    bundles: &[crate::discovery_auto_seed::EntityCandidateBundle],
) -> TypedCandidateGraph {
    TypedCandidateGraph::build(bundles, &indexmap::IndexMap::new())
}

#[test]
fn single_capability_witness_yields_one_seed_plan() {
    let message = bundle(
        "gmail:Message",
        "gmail",
        "Message",
        vec![cap("gmail:Message:Search", "Search", "Search", 90)],
        90,
    );
    let thread = bundle(
        "gmail:Thread",
        "gmail",
        "Thread",
        vec![cap("gmail:Thread:Search", "Search", "Search", 80)],
        80,
    );
    let bundles = vec![message, thread];
    let graph = empty_graph(&bundles);
    let corpus = build_witness_corpus(&bundles, &[], &graph, None).expect("corpus");
    assert!(corpus.witnesses.len() >= 2);

    let msg_w = corpus
        .witnesses
        .iter()
        .position(|w| {
            matches!(&w.kind, WitnessKind::DirectCapability { entity, .. } if entity == "Message")
        })
        .expect("message witness");

    let plans = construct_minimal_plans(&corpus, &[msg_w]).expect("plans");
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].candidate_ids, vec!["gmail:Message".to_string()]);
    assert!(verify_plan(&corpus, &plans[0], &[msg_w]));
}

#[test]
fn parent_does_not_cover_child_mutate_witness() {
    let issue = bundle(
        "github:Issue",
        "github",
        "Issue",
        vec![cap("github:Issue:Update", "Update", "Update", 50)],
        50,
    );
    let comment = bundle(
        "github:IssueComment",
        "github",
        "IssueComment",
        vec![cap("github:IssueComment:Create", "Create", "Create", 70)],
        70,
    );
    let bundles = vec![issue, comment];
    let graph = TypedCandidateGraph::build(&bundles, &indexmap::IndexMap::new());
    let corpus = build_witness_corpus(&bundles, &[], &graph, None).expect("corpus");
    let comment_w = corpus
        .witnesses
        .iter()
        .position(|w| {
            matches!(
                &w.kind,
                WitnessKind::DirectCapability { entity, kind, .. }
                    if entity == "IssueComment" && kind == "Create"
            )
        })
        .expect("comment create");
    let plans = construct_minimal_plans(&corpus, &[comment_w]).expect("plans");
    assert_eq!(plans.len(), 1);
    assert_eq!(
        plans[0].candidate_ids,
        vec!["github:IssueComment".to_string()]
    );
}

#[test]
fn two_witnesses_same_entity_stay_minimal() {
    let pr = bundle(
        "github:PullRequest",
        "github",
        "PullRequest",
        vec![
            cap("github:PullRequest:Create", "Create", "Create", 100),
            cap("github:PullRequest:Update", "Update", "Update", 40),
        ],
        100,
    );
    let bundles = vec![pr];
    let graph = TypedCandidateGraph::build(&bundles, &indexmap::IndexMap::new());
    let corpus = build_witness_corpus(&bundles, &[], &graph, None).expect("corpus");
    let idxs: Vec<_> = (0..corpus.witnesses.len()).collect();
    let plans = construct_minimal_plans(&corpus, &idxs).expect("plans");
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].candidate_ids.len(), 1);
}

#[test]
fn graph_note_lists_siblings_in_pool() {
    let message = bundle(
        "gmail:Message",
        "gmail",
        "Message",
        vec![cap("gmail:Message:Search", "Search", "Search", 90)],
        90,
    );
    let thread = bundle(
        "gmail:Thread",
        "gmail",
        "Thread",
        vec![cap("gmail:Thread:Search", "Search", "Search", 80)],
        80,
    );
    let bundles = vec![message, thread];
    let graph = empty_graph(&bundles);
    let corpus = build_witness_corpus(&bundles, &[], &graph, None).expect("corpus");
    let msg = corpus
        .witnesses
        .iter()
        .find(|w| {
            matches!(&w.kind, WitnessKind::DirectCapability { entity, .. } if entity == "Message")
        })
        .expect("message");
    assert!(
        msg.pool
            .render_graph_note()
            .contains("siblings_in_pool=Thread"),
        "graph_note={}",
        msg.pool.render_graph_note()
    );
}

#[test]
fn resolve_accepts_unique_capability_name_alias() {
    let message = bundle(
        "gmail:Message",
        "gmail",
        "Message",
        vec![cap("gmail:Message:Search", "message_search", "Search", 90)],
        90,
    );
    let bundles = vec![message];
    let graph = empty_graph(&bundles);
    let corpus = build_witness_corpus(&bundles, &[], &graph, None).expect("corpus");
    let idxs = corpus
        .resolve_symbols(&["message_search".into()])
        .expect("alias");
    assert_eq!(idxs.len(), 1);
}

#[test]
fn uncoverable_witness_errors() {
    let issue = bundle(
        "github:Issue",
        "github",
        "Issue",
        vec![cap("github:Issue:Get", "Get", "Get", 10)],
        10,
    );
    let bundles = vec![issue];
    let graph = TypedCandidateGraph::build(&bundles, &indexmap::IndexMap::new());
    let corpus = build_witness_corpus(&bundles, &[], &graph, None).expect("corpus");
    let err = construct_minimal_plans(&corpus, &[]).unwrap_err();
    assert!(matches!(err, PlanConstructError::EmptyWitnesses));
}

#[test]
fn federation_two_catalogs_need_two_seeds() {
    let task = bundle(
        "clickup:Task",
        "clickup",
        "Task",
        vec![cap("clickup:Task:Create", "Create", "Create", 60)],
        60,
    );
    let issue = bundle(
        "github:Issue",
        "github",
        "Issue",
        vec![cap("github:Issue:Create", "Create", "Create", 60)],
        60,
    );
    let bundles = vec![task, issue];
    let graph = TypedCandidateGraph::build(&bundles, &indexmap::IndexMap::new());
    let corpus = build_witness_corpus(&bundles, &[], &graph, None).expect("corpus");
    let idxs: Vec<_> = (0..corpus.witnesses.len()).collect();
    let plans = construct_minimal_plans(&corpus, &idxs).expect("plans");
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].candidate_ids.len(), 2);
}

#[test]
fn missing_named_catalogs_detected() {
    let task = bundle(
        "clickup:Task",
        "clickup",
        "Task",
        vec![cap("clickup:Task:Create", "Create", "Create", 60)],
        60,
    );
    let issue = bundle(
        "github:Issue",
        "github",
        "Issue",
        vec![cap("github:Issue:Create", "Create", "Create", 60)],
        60,
    );
    let bundles = vec![task, issue];
    let graph = empty_graph(&bundles);
    let corpus = build_witness_corpus(&bundles, &[], &graph, None).expect("corpus");
    let clickup_only: Vec<_> = corpus
        .witnesses
        .iter()
        .enumerate()
        .filter(|(_, w)| {
            matches!(&w.kind, WitnessKind::DirectCapability { entry_id, .. } if entry_id == "clickup")
        })
        .map(|(i, _)| i)
        .collect();
    let missing = missing_named_catalog_coverage(
        &["github".into(), "clickup".into()],
        &corpus,
        &clickup_only,
    );
    assert_eq!(missing, vec!["github".to_string()]);
}

#[test]
fn brand_lock_drops_unlocked_catalog_witnesses() {
    let github = bundle(
        "github:Issue",
        "github",
        "Issue",
        vec![cap("github:Issue:Query", "Query", "Query", 90)],
        90,
    );
    let teams = bundle(
        "microsoft-teams:ChatMessage",
        "microsoft-teams",
        "ChatMessage",
        vec![cap("microsoft-teams:ChatMessage:Get", "Get", "Get", 95)],
        95,
    );
    let bundles = vec![github, teams];
    let graph = empty_graph(&bundles);
    let corpus = build_witness_corpus(&bundles, &["github".into()], &graph, None).expect("corpus");
    assert!(corpus.witnesses.iter().all(|w| {
        matches!(&w.kind, WitnessKind::DirectCapability { entry_id, .. } if entry_id == "github")
            || matches!(&w.kind, WitnessKind::RelationHop { entry_id, .. } if entry_id == "github")
    }));
    assert!(
        !corpus
            .witnesses
            .iter()
            .any(|w| witness_catalog_of(w) == "microsoft-teams"),
        "teams must not appear under github brand lock"
    );
}

#[test]
fn unbranded_keeps_only_top_catalogs_by_lexical_score() {
    let mut bundles = vec![
        bundle(
            "github:Issue",
            "github",
            "Issue",
            vec![cap("github:Issue:Query", "Query", "Query", 100)],
            100,
        ),
        bundle(
            "gmail:Message",
            "gmail",
            "Message",
            vec![cap("gmail:Message:Search", "Search", "Search", 90)],
            90,
        ),
        bundle(
            "slack:Channel",
            "slack",
            "Channel",
            vec![cap("slack:Channel:Query", "Query", "Query", 80)],
            80,
        ),
        bundle(
            "microsoft-teams:ChatMessage",
            "microsoft-teams",
            "ChatMessage",
            vec![cap("microsoft-teams:ChatMessage:Get", "Get", "Get", 10)],
            10,
        ),
        bundle(
            "reddit:Post",
            "reddit",
            "Post",
            vec![cap("reddit:Post:Query", "Query", "Query", 5)],
            5,
        ),
    ];
    // Extra low-score noise vendors beyond the top-3 soft catalog cut.
    bundles.push(bundle(
        "spotify:Track",
        "spotify",
        "Track",
        vec![cap("spotify:Track:Search", "Search", "Search", 1)],
        1,
    ));
    let graph = empty_graph(&bundles);
    let corpus = build_witness_corpus(&bundles, &[], &graph, None).expect("corpus");
    let catalogs: std::collections::BTreeSet<&str> =
        corpus.witnesses.iter().map(witness_catalog_of).collect();
    assert!(
        catalogs.len() <= MAX_WITNESS_CATALOGS_UNBRANDED,
        "catalogs={catalogs:?}"
    );
    assert!(catalogs.contains("github"));
    assert!(catalogs.contains("gmail"));
    assert!(catalogs.contains("slack"));
    assert!(!catalogs.contains("microsoft-teams"));
    assert!(!catalogs.contains("reddit"));
    assert!(!catalogs.contains("spotify"));
}

#[test]
fn shortlist_prefers_higher_lexical_score_within_cap() {
    let mut bundles = Vec::new();
    // One brand-locked catalog with many low-score caps would previously crowd out others.
    for i in 0..12 {
        bundles.push(bundle(
            &format!("github:E{i}"),
            "github",
            &format!("E{i}"),
            vec![cap(
                &format!("github:E{i}:Query"),
                "Query",
                "Query",
                10 + i as u32,
            )],
            10 + i as u32,
        ));
    }
    bundles.push(bundle(
        "gmail:Message",
        "gmail",
        "Message",
        vec![cap("gmail:Message:Search", "Search", "Search", 200)],
        200,
    ));
    let graph = empty_graph(&bundles);
    let corpus = build_witness_corpus(&bundles, &["github".into(), "gmail".into()], &graph, None)
        .expect("corpus");
    assert!(corpus.witnesses.len() <= MAX_WITNESSES);
    assert_eq!(corpus.witnesses[0].lexical_score, 200);
    assert!(matches!(
        &corpus.witnesses[0].kind,
        WitnessKind::DirectCapability { entity, .. } if entity == "Message"
    ));
}

fn witness_catalog_of(w: &RequirementWitness) -> &str {
    match &w.kind {
        WitnessKind::DirectCapability { entry_id, .. }
        | WitnessKind::RelationHop { entry_id, .. } => entry_id.as_str(),
    }
}

#[test]
fn corpus_stamps_attach_on_label_and_prune_drops_label_read() {
    use crate::discovery_intent_class::DiscoveryIntentClass;
    use crate::discovery_seed_catalog::CatalogWorkflowContext;
    use crate::identity::{CapabilityName, EntityFieldName, EntityName, RelationName};
    use crate::schema::{
        CapabilityKind, CapabilityMapping, CapabilitySchema, CapabilityTemplateJson, Cardinality,
        DiscoveryEntityHints, DiscoveryRelationHints, DiscoverySeedClass, DiscoverySeedNav,
        EntityDef, RelationSchema, CGS,
    };
    use indexmap::IndexMap;
    use std::collections::HashMap;
    use std::sync::Arc;

    let mut relations = IndexMap::new();
    relations.insert(
        RelationName::from("labels"),
        RelationSchema {
            name: RelationName::from("labels"),
            description: String::new(),
            target_resource: EntityName::from("Label"),
            cardinality: Cardinality::Many,
            materialize: None,
            discovery: Some(DiscoveryRelationHints {
                qualifier_terms: vec![],
                traversal_weight: None,
                seed_nav: Some(DiscoverySeedNav::Attach),
            }),
        },
    );

    let mut entities = IndexMap::new();
    entities.insert(
        EntityName::from("Issue"),
        EntityDef {
            name: EntityName::from("Issue"),
            description: "Issue".into(),
            id_field: EntityFieldName::from("id"),
            id_format: None,
            id_from: None,
            fields: IndexMap::new(),
            relations,
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: Some(DiscoveryEntityHints {
                names: vec!["issue".into()],
                qualifier_names: vec![],
                seed_class: Some(DiscoverySeedClass::Primary),
            }),
        },
    );
    entities.insert(
        EntityName::from("Label"),
        EntityDef {
            name: EntityName::from("Label"),
            description: "Label".into(),
            id_field: EntityFieldName::from("id"),
            id_format: None,
            id_from: None,
            fields: IndexMap::new(),
            relations: IndexMap::new(),
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: Some(DiscoveryEntityHints {
                names: vec!["label".into()],
                qualifier_names: vec![],
                seed_class: Some(DiscoverySeedClass::Dependent),
            }),
        },
    );

    let mut capabilities = IndexMap::new();
    for (id, domain, name) in [
        ("issue_query", "Issue", "issue_query"),
        ("label_query", "Label", "label_query"),
    ] {
        capabilities.insert(
            CapabilityName::from(id),
            CapabilitySchema {
                name: CapabilityName::from(name),
                description: format!("Query {domain}"),
                kind: CapabilityKind::Query,
                domain: EntityName::from(domain),
                mapping: CapabilityMapping {
                    template: CapabilityTemplateJson(serde_json::json!({ "method": "GET" })),
                },
                input_schema: None,
                output_schema: None,
                provides: vec![],
                sanitizes: vec![],
                deterministic: None,
                scope_aggregate_key_policy: Default::default(),
                preflight: None,
                discovery: None,
                identity_key: None,
            },
        );
    }

    let mut cgs = CGS::new();
    cgs.entities = entities;
    cgs.capabilities = capabilities;
    let cgs = Arc::new(cgs);
    let mut catalog_refs = HashMap::new();
    catalog_refs.insert("fx".to_string(), cgs.as_ref());
    let ctx = CatalogWorkflowContext::build(
        &catalog_refs,
        "triage open bugs and summarize label coverage",
        &DiscoveryIntentClass::default(),
        &["fx".into()],
    );

    let bundles = vec![
        crate::discovery_auto_seed::EntityCandidateBundle {
            candidate_id: "fx:Issue".into(),
            entry_id: "fx".into(),
            entity: "Issue".into(),
            entity_description: "Issue".into(),
            max_lexical_score: 80,
            capabilities: vec![cap("fx:Issue:Query", "issue_query", "Query", 80)],
            relation_hints: "labels→Label".into(),
            catalog_route_evidence: true,
        },
        crate::discovery_auto_seed::EntityCandidateBundle {
            candidate_id: "fx:Label".into(),
            entry_id: "fx".into(),
            entity: "Label".into(),
            entity_description: "Label".into(),
            max_lexical_score: 70,
            capabilities: vec![cap("fx:Label:Query", "label_query", "Query", 70)],
            relation_hints: String::new(),
            catalog_route_evidence: true,
        },
    ];
    let mut graph_catalogs = IndexMap::new();
    graph_catalogs.insert("fx".to_string(), Arc::clone(&cgs));
    let graph = TypedCandidateGraph::build(&bundles, &graph_catalogs);
    let corpus =
        build_witness_corpus(&bundles, &["fx".into()], &graph, Some(&ctx)).expect("corpus");

    let label = corpus
        .witnesses
        .iter()
        .find(|w| {
            matches!(&w.kind, WitnessKind::DirectCapability { entity, .. } if entity == "Label")
        })
        .expect("label witness");
    assert!(
        label.seed_nav.is_attach(),
        "graph_note={}",
        label.pool.render_graph_note()
    );
    assert!(label.seed_class.is_dependent());
    assert!(
        label.pool.parents.contains("Issue"),
        "graph_note={}",
        label.pool.render_graph_note()
    );

    let issue_idx = corpus
        .witnesses
        .iter()
        .position(|w| {
            matches!(&w.kind, WitnessKind::DirectCapability { entity, .. } if entity == "Issue")
        })
        .unwrap();
    let label_idx = corpus
        .witnesses
        .iter()
        .position(|w| {
            matches!(&w.kind, WitnessKind::DirectCapability { entity, .. } if entity == "Label")
        })
        .unwrap();
    let pruned = prune_witness_selection(&corpus, &[issue_idx, label_idx], IntentGate::Ungated);
    assert_eq!(pruned, vec![issue_idx]);
}

#[test]
fn corpus_stamps_own_pair_on_both_ends_of_own_edge() {
    use crate::discovery_intent_class::DiscoveryIntentClass;
    use crate::discovery_seed_catalog::CatalogWorkflowContext;
    use crate::identity::{CapabilityName, EntityFieldName, EntityName, RelationName};
    use crate::schema::{
        CapabilityKind, CapabilityMapping, CapabilitySchema, CapabilityTemplateJson, Cardinality,
        DiscoveryEntityHints, DiscoveryRelationHints, DiscoverySeedClass, DiscoverySeedNav,
        EntityDef, RelationSchema, CGS,
    };
    use indexmap::IndexMap;
    use std::collections::HashMap;
    use std::sync::Arc;

    let mut relations = IndexMap::new();
    relations.insert(
        RelationName::from("messages"),
        RelationSchema {
            name: RelationName::from("messages"),
            description: String::new(),
            target_resource: EntityName::from("Message"),
            cardinality: Cardinality::Many,
            materialize: None,
            discovery: Some(DiscoveryRelationHints {
                qualifier_terms: vec![],
                traversal_weight: None,
                seed_nav: Some(DiscoverySeedNav::Own),
            }),
        },
    );

    let mut entities = IndexMap::new();
    entities.insert(
        EntityName::from("Thread"),
        EntityDef {
            name: EntityName::from("Thread"),
            description: "Thread".into(),
            id_field: EntityFieldName::from("id"),
            id_format: None,
            id_from: None,
            fields: IndexMap::new(),
            relations,
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: Some(DiscoveryEntityHints {
                names: vec!["thread".into()],
                qualifier_names: vec![],
                seed_class: Some(DiscoverySeedClass::Primary),
            }),
        },
    );
    entities.insert(
        EntityName::from("Message"),
        EntityDef {
            name: EntityName::from("Message"),
            description: "Message".into(),
            id_field: EntityFieldName::from("id"),
            id_format: None,
            id_from: None,
            fields: IndexMap::new(),
            relations: IndexMap::new(),
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: Some(DiscoveryEntityHints {
                names: vec!["message".into()],
                qualifier_names: vec![],
                seed_class: Some(DiscoverySeedClass::Primary),
            }),
        },
    );

    let mut capabilities = IndexMap::new();
    for (id, domain, name) in [
        ("thread_query", "Thread", "thread_query"),
        ("message_query", "Message", "message_query"),
    ] {
        capabilities.insert(
            CapabilityName::from(id),
            CapabilitySchema {
                name: CapabilityName::from(name),
                description: format!("Query {domain}"),
                kind: CapabilityKind::Query,
                domain: EntityName::from(domain),
                mapping: CapabilityMapping {
                    template: CapabilityTemplateJson(serde_json::json!({ "method": "GET" })),
                },
                input_schema: None,
                output_schema: None,
                provides: vec![],
                sanitizes: vec![],
                deterministic: None,
                scope_aggregate_key_policy: Default::default(),
                preflight: None,
                discovery: None,
                identity_key: None,
            },
        );
    }

    let mut cgs = CGS::new();
    cgs.entities = entities;
    cgs.capabilities = capabilities;
    let cgs = Arc::new(cgs);
    let mut catalog_refs = HashMap::new();
    catalog_refs.insert("fx".to_string(), cgs.as_ref());
    let ctx = CatalogWorkflowContext::build(
        &catalog_refs,
        "list messages in a thread",
        &DiscoveryIntentClass::default(),
        &["fx".into()],
    );

    let bundles = vec![
        crate::discovery_auto_seed::EntityCandidateBundle {
            candidate_id: "fx:Thread".into(),
            entry_id: "fx".into(),
            entity: "Thread".into(),
            entity_description: "Thread".into(),
            max_lexical_score: 80,
            capabilities: vec![cap("fx:Thread:Query", "thread_query", "Query", 80)],
            relation_hints: "messages→Message".into(),
            catalog_route_evidence: true,
        },
        crate::discovery_auto_seed::EntityCandidateBundle {
            candidate_id: "fx:Message".into(),
            entry_id: "fx".into(),
            entity: "Message".into(),
            entity_description: "Message".into(),
            max_lexical_score: 70,
            capabilities: vec![cap("fx:Message:Query", "message_query", "Query", 70)],
            relation_hints: String::new(),
            catalog_route_evidence: true,
        },
    ];
    let mut graph_catalogs = IndexMap::new();
    graph_catalogs.insert("fx".to_string(), Arc::clone(&cgs));
    let graph = TypedCandidateGraph::build(&bundles, &graph_catalogs);
    let corpus =
        build_witness_corpus(&bundles, &["fx".into()], &graph, Some(&ctx)).expect("corpus");

    let thread = corpus
        .witnesses
        .iter()
        .find(|w| {
            matches!(&w.kind, WitnessKind::DirectCapability { entity, .. } if entity == "Thread")
        })
        .expect("thread witness");
    let message = corpus
        .witnesses
        .iter()
        .find(|w| {
            matches!(&w.kind, WitnessKind::DirectCapability { entity, .. } if entity == "Message")
        })
        .expect("message witness");
    // Governing seed_nav is incoming-edge only; Source stays unset, Target gets own.
    assert_eq!(thread.seed_nav, SeedNavStamp::Unset);
    assert!(message.seed_nav.is_own());
    assert_eq!(thread.own_pairs.render(), "Thread→Message");
    assert_eq!(message.own_pairs.render(), "Thread→Message");
    assert_eq!(thread.own_pairs.end_role("Thread"), OwnEnd::Source);
    assert_eq!(message.own_pairs.end_role("Message"), OwnEnd::Target);
}
