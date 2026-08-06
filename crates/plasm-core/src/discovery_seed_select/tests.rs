use crate::discovery_auto_seed::EntityCandidateBundle;
use crate::discovery_auto_seed::EntityCapabilityEvidence;
use crate::discovery_intent_class::DiscoveryIntentClass;
use crate::discovery_seed_select::{
    resolve_llm_seed_selection, seeds_from_candidate_ids, validate_seed_selection,
    LlmSeedSelectionInput, SeedAlternativeSetRaw, SeedSelectionDecision, SeedSelectionRaw,
    SeedSelectionValidationError, ValidatedSeedSelection,
};
use crate::discovery_seed_symbol_map::SeedSymbolMap;

fn bundle(
    id: &str,
    eid: &str,
    ent: &str,
    cap: &str,
    kind: &str,
    relations: &str,
) -> EntityCandidateBundle {
    EntityCandidateBundle {
        candidate_id: id.into(),
        entry_id: eid.into(),
        entity: ent.into(),
        entity_description: String::new(),
        max_lexical_score: 1,
        capabilities: vec![EntityCapabilityEvidence {
            capability_id: format!("{eid}:{ent}:{cap}"),
            capability_name: cap.into(),
            kind: kind.into(),
            description: String::new(),
            reason_codes: vec![],
            lexical_score: 1,
        }],
        relation_hints: relations.into(),
        catalog_route_evidence: false,
    }
}

fn resolve(
    input: LlmSeedSelectionInput,
    map: &SeedSymbolMap,
    bundles: &[EntityCandidateBundle],
    intent_class: &DiscoveryIntentClass,
) -> Result<SeedSelectionRaw, SeedSelectionValidationError> {
    resolve_llm_seed_selection(input, map, bundles, intent_class, None, None)
}

fn ready_input(symbols: &[&str]) -> LlmSeedSelectionInput {
    LlmSeedSelectionInput {
        decision: SeedSelectionDecision::Ready,
        selected_symbols: symbols.iter().map(|s| (*s).to_string()).collect(),
        requirements: vec!["read issues".into()],
        alternative_symbol_sets: vec![],
        alternative_labels: vec![],
        uncovered_requirements: vec![],
        reasoning: "ok".into(),
    }
}

#[test]
fn ready_validates_seed_count_and_supporting() {
    let bundles = vec![bundle(
        "gmail:Thread",
        "gmail",
        "Thread",
        "thread_search",
        "Query",
        "",
    )];
    let map = SeedSymbolMap::build(&bundles, None);
    let raw = resolve(
        ready_input(&["s1"]),
        &map,
        &bundles,
        &DiscoveryIntentClass::ReadListNav,
    )
    .expect("resolve");
    assert!(validate_seed_selection(&raw, &bundles).is_ok());
}

#[test]
fn unknown_symbol_is_routing_error() {
    let bundles = vec![bundle(
        "gmail:Thread",
        "gmail",
        "Thread",
        "thread_search",
        "Query",
        "",
    )];
    let map = SeedSymbolMap::build(&bundles, None);
    let err = resolve(
        ready_input(&["s99"]),
        &map,
        &bundles,
        &DiscoveryIntentClass::ReadListNav,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        SeedSelectionValidationError::UnknownSymbol(_)
    ));
}

#[test]
fn raw_id_in_symbol_field_is_rejected() {
    let bundles = vec![bundle(
        "gmail:Thread",
        "gmail",
        "Thread",
        "thread_search",
        "Query",
        "",
    )];
    let map = SeedSymbolMap::build(&bundles, None);
    let err = resolve(
        ready_input(&["gmail:Thread"]),
        &map,
        &bundles,
        &DiscoveryIntentClass::ReadListNav,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        SeedSelectionValidationError::RawIdHallucination(_)
    ));
}

#[test]
fn ready_resolve_is_membership_only_no_semantic_rewrite() {
    let bundles = vec![
        bundle(
            "github:Issue",
            "github",
            "Issue",
            "issue_query",
            "Query",
            "labels→Label",
        ),
        bundle(
            "github:Label",
            "github",
            "Label",
            "label_query",
            "Query",
            "",
        ),
    ];
    let map = SeedSymbolMap::build(&bundles, None);
    let label_symbol = map
        .rows()
        .iter()
        .find(|row| row.entity == "Label")
        .map(|row| row.symbol.as_str())
        .expect("label row");
    let raw = resolve(
        ready_input(&[label_symbol]),
        &map,
        &bundles,
        &DiscoveryIntentClass::ReadListNav,
    )
    .expect("resolve");
    // Product resolve must not rewrite Label→Issue; that living dead path is gone.
    assert_eq!(raw.selected_ids, vec!["github:Label"]);
}

#[test]
fn localized_create_keeps_mutation_anchor() {
    let bundles = vec![
        bundle(
            "github:Repository",
            "github",
            "Repository",
            "repo_branch_create",
            "Action",
            "",
        ),
        bundle(
            "github:IssueComment",
            "github",
            "IssueComment",
            "issue_comment_create",
            "Create",
            "",
        ),
    ];
    let map = SeedSymbolMap::build(&bundles, None);
    let comment_symbol = map
        .rows()
        .iter()
        .find(|row| row.entity == "IssueComment")
        .map(|row| row.symbol.as_str())
        .expect("comment symbol");
    let raw = resolve(
        ready_input(&[comment_symbol]),
        &map,
        &bundles,
        &DiscoveryIntentClass::LocalizedMutation,
    )
    .expect("resolve");
    assert_eq!(raw.selected_ids, vec!["github:IssueComment"]);
}

#[test]
fn clarify_requires_two_alternative_symbol_sets() {
    let bundles = vec![
        bundle(
            "github:Issue",
            "github",
            "Issue",
            "issue_query",
            "Query",
            "",
        ),
        bundle(
            "gitlab:Issue",
            "gitlab",
            "Issue",
            "issue_query",
            "Query",
            "",
        ),
    ];
    let map = SeedSymbolMap::build(&bundles, None);
    let raw = resolve(
        LlmSeedSelectionInput {
            decision: SeedSelectionDecision::Clarify,
            selected_symbols: vec![],
            requirements: vec!["list issues".into()],
            alternative_symbol_sets: vec![vec!["s1".into()], vec!["s2".into()]],
            alternative_labels: vec!["github".into(), "gitlab".into()],
            uncovered_requirements: vec![],
            reasoning: "ambiguous".into(),
        },
        &map,
        &bundles,
        &DiscoveryIntentClass::ReadListNav,
    )
    .expect("resolve");
    assert_eq!(raw.decision, SeedSelectionDecision::Clarify);
    assert_eq!(raw.alternative_sets.len(), 2);
}

#[test]
fn brand_lock_rejects_provider_clarify() {
    use super::validation::{
        classify_clarify, validate_seed_selection_with_brand_lock, ClarifyKind,
    };
    let bundles = vec![
        bundle(
            "github:Issue",
            "github",
            "Issue",
            "issue_query",
            "Query",
            "",
        ),
        bundle(
            "linear:Issue",
            "linear",
            "Issue",
            "issue_query",
            "Query",
            "",
        ),
    ];
    let raw = SeedSelectionRaw {
        decision: SeedSelectionDecision::Clarify,
        requirements: vec!["issues".into()],
        selected_ids: vec![],
        supporting_capability_ids: vec![],
        teaching_satellites: vec![],
        alternative_sets: vec![
            SeedAlternativeSetRaw {
                candidate_ids: vec!["github:Issue".into()],
                label: "github".into(),
            },
            SeedAlternativeSetRaw {
                candidate_ids: vec!["linear:Issue".into()],
                label: "linear".into(),
            },
        ],
        uncovered_requirements: vec![],
        reasoning: "which tracker".into(),
    };
    assert_eq!(
        classify_clarify(&raw.alternative_sets),
        ClarifyKind::ProviderDisambiguation
    );
    let err = validate_seed_selection_with_brand_lock(
        &raw,
        &bundles,
        Some(&["github".into(), "linear".into()]),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        SeedSelectionValidationError::ClarifyUnderBrandLock(_)
    ));
}

#[test]
fn brand_lock_allows_entity_clarify_within_one_catalog() {
    use super::validation::{
        classify_clarify, validate_seed_selection_with_brand_lock, ClarifyKind,
    };
    let bundles = vec![
        bundle(
            "github:Issue",
            "github",
            "Issue",
            "issue_query",
            "Query",
            "",
        ),
        bundle(
            "github:PullRequest",
            "github",
            "PullRequest",
            "pr_query",
            "Query",
            "",
        ),
    ];
    let raw = SeedSelectionRaw {
        decision: SeedSelectionDecision::Clarify,
        requirements: vec!["open work".into()],
        selected_ids: vec![],
        supporting_capability_ids: vec![],
        teaching_satellites: vec![],
        alternative_sets: vec![
            SeedAlternativeSetRaw {
                candidate_ids: vec!["github:Issue".into()],
                label: "issues".into(),
            },
            SeedAlternativeSetRaw {
                candidate_ids: vec!["github:PullRequest".into()],
                label: "prs".into(),
            },
        ],
        uncovered_requirements: vec![],
        reasoning: "issue vs pr".into(),
    };
    assert_eq!(
        classify_clarify(&raw.alternative_sets),
        ClarifyKind::EntityDisambiguation
    );
    let ok = validate_seed_selection_with_brand_lock(
        &raw,
        &bundles,
        Some(&["github".into()]),
    )
    .expect("entity clarify under brand lock");
    assert!(matches!(ok, ValidatedSeedSelection::Abstain(_)));
}

#[test]
fn hard_miss_requires_uncovered_requirements() {
    let bundles = vec![bundle(
        "github:Issue",
        "github",
        "Issue",
        "issue_query",
        "Query",
        "",
    )];
    let map = SeedSymbolMap::build(&bundles, None);
    let err = resolve(
        LlmSeedSelectionInput {
            decision: SeedSelectionDecision::HardMiss,
            selected_symbols: vec![],
            requirements: vec![],
            alternative_symbol_sets: vec![],
            alternative_labels: vec![],
            uncovered_requirements: vec![],
            reasoning: "out of catalog".into(),
        },
        &map,
        &bundles,
        &DiscoveryIntentClass::HostCapabilityMiss {
            summary: "fine-tune".into(),
        },
    )
    .unwrap_err();
    assert!(matches!(
        err,
        SeedSelectionValidationError::HardMissMissingUncovered
    ));
}

#[test]
fn validated_ready_maps_to_seeds() {
    let bundles = vec![bundle(
        "gmail:Message",
        "gmail",
        "Message",
        "message_get",
        "Get",
        "",
    )];
    let map = SeedSymbolMap::build(&bundles, None);
    let raw = resolve(
        ready_input(&["s1"]),
        &map,
        &bundles,
        &DiscoveryIntentClass::ReadListNav,
    )
    .expect("resolve");
    let ValidatedSeedSelection::Ready(ready) =
        validate_seed_selection(&raw, &bundles).expect("validate")
    else {
        panic!("expected ready");
    };
    let seeds = seeds_from_candidate_ids(&bundles, &ready.selected_ids);
    assert_eq!(seeds, vec![("gmail".into(), "Message".into())]);
}
