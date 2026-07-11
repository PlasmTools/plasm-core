use super::{
    build_seed_bundle_index_tables, resolve_seed_coverage_assessment, validate_seed_selection,
    SeedBundleIndexTables, SeedSelectionDecision, SeedSelectionRaw, SeedSelectionValidationError,
};
use crate::discovery_auto_seed::{EntityCandidateBundle, EntityCapabilityEvidence};
use crate::discovery_seed_bundle::CandidateSeedBundle;

fn bundle(id: &str, eid: &str, ent: &str, cap: &str) -> EntityCandidateBundle {
        bundle_with_hints(id, eid, ent, cap, "Query", "")
    }

    fn bundle_with_hints(
        id: &str,
        eid: &str,
        ent: &str,
        cap: &str,
        kind: &str,
        relation_hints: &str,
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
            relation_hints: relation_hints.into(),
            catalog_route_evidence: false,
        }
    }

    fn seed_bundle(catalog: &str, ids: &[&str]) -> CandidateSeedBundle {
        CandidateSeedBundle {
            candidate_ids: ids.iter().map(|id| (*id).to_string()).collect(),
            catalogs: vec![catalog.into()],
            max_lexical_score: 10,
            total_lexical_score: ids.len() as u64 * 10,
        }
    }

    fn seed_bundle_scored(catalog: &str, ids: &[&str], score: u32) -> CandidateSeedBundle {
        CandidateSeedBundle {
            candidate_ids: ids.iter().map(|id| (*id).to_string()).collect(),
            catalogs: vec![catalog.into()],
            max_lexical_score: score,
            total_lexical_score: ids.len() as u64 * u64::from(score),
        }
    }

    fn coverage_assessment(
        requirements: Vec<(&str, Vec<i64>)>,
        coverage: Vec<(i64, Vec<i64>)>,
        tables: &SeedBundleIndexTables,
    ) -> SeedSelectionRaw {
        let reqs = requirements
            .into_iter()
            .enumerate()
            .map(|(index, (text, deps))| (index as i64, text.to_string(), deps))
            .collect();
        resolve_seed_coverage_assessment(reqs, coverage, String::new(), tables, "")
            .expect("resolve coverage")
    }

    #[test]
    fn ready_validates_seed_count_and_supporting() {
        let bundles = vec![bundle("gmail:Thread", "gmail", "Thread", "thread_search")];
        let raw = SeedSelectionRaw {
            decision: SeedSelectionDecision::Ready,
            requirements: vec!["find thread".into()],
            selected_ids: vec!["gmail:Thread".into()],
            supporting_capability_ids: vec!["gmail:Thread:thread_search".into()],
            alternative_sets: vec![],
            uncovered_requirements: vec![],
            reasoning: "ok".into(),
        };
        assert!(validate_seed_selection(&raw, &bundles).is_ok());
    }

    #[test]
    fn invented_bundle_index_is_routing_error() {
        let bundles = vec![bundle("gmail:Thread", "gmail", "Thread", "thread_search")];
        let tables =
            build_seed_bundle_index_tables(&[seed_bundle("gmail", &["gmail:Thread"])], &bundles)
                .expect("build tables");
        let raw = resolve_seed_coverage_assessment(
            vec![(0, "find thread".into(), vec![])],
            vec![(0, vec![99])],
            String::new(),
            &tables,
            "",
        );
        assert!(matches!(
            raw,
            Err(SeedSelectionValidationError::UnknownBundleIndex(99))
        ));
    }

    #[test]
    fn bundle_roots_and_supporting_capabilities_are_resolved_together() {
        let bundles = vec![
            bundle("gmail:Thread", "gmail", "Thread", "thread_search"),
            bundle("gmail:Message", "gmail", "Message", "message_list"),
        ];
        let seed_bundles = [seed_bundle("gmail", &["gmail:Thread", "gmail:Message"])];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = coverage_assessment(vec![("find mail", vec![])], vec![(0, vec![0])], &tables);
        assert_eq!(
            raw.selected_ids,
            vec!["gmail:Message", "gmail:Thread"]
        );
        assert_eq!(
            raw.supporting_capability_ids,
            vec!["gmail:Message:message_list", "gmail:Thread:thread_search"]
        );
    }

    #[test]
    fn provider_distinct_minimal_complete_bundles_clarify() {
        let bundles = vec![
            bundle("gmail:Message", "gmail", "Message", "message_search"),
            bundle("outlook:Message", "outlook", "Message", "message_search"),
        ];
        let seed_bundles = [
            seed_bundle("gmail", &["gmail:Message"]),
            seed_bundle("outlook", &["outlook:Message"]),
        ];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = coverage_assessment(vec![("find mail", vec![])], vec![(0, vec![0, 1])], &tables);
        assert_eq!(raw.decision, SeedSelectionDecision::Clarify);
        assert_eq!(raw.alternative_sets.len(), 2);
    }

    #[test]
    fn requirement_dependency_cycle_is_routing_error() {
        let bundles = vec![bundle("gmail:Thread", "gmail", "Thread", "thread_search")];
        let tables =
            build_seed_bundle_index_tables(&[seed_bundle("gmail", &["gmail:Thread"])], &bundles)
                .expect("build tables");
        let raw = resolve_seed_coverage_assessment(
            vec![(0, "read".into(), vec![1]), (1, "write".into(), vec![0])],
            vec![(0, vec![0]), (1, vec![0])],
            String::new(),
            &tables,
            "",
        );
        assert!(matches!(
            raw,
            Err(SeedSelectionValidationError::RequirementDependencyCycle)
        ));
    }

    #[test]
    fn empty_support_set_yields_hard_miss() {
        let bundles = vec![bundle("gmail:Thread", "gmail", "Thread", "thread_search")];
        let tables =
            build_seed_bundle_index_tables(&[seed_bundle("gmail", &["gmail:Thread"])], &bundles)
                .expect("build tables");
        let raw = coverage_assessment(
            vec![("fine-tune model", vec![])],
            vec![(0, vec![])],
            &tables,
        );
        assert_eq!(raw.decision, SeedSelectionDecision::HardMiss);
        assert_eq!(raw.uncovered_requirements, vec!["fine-tune model"]);
    }

    #[test]
    fn intersecting_coverage_selects_smallest_complete_bundle() {
        let bundles = vec![
            bundle("github:Issue", "github", "Issue", "issue_list"),
            bundle("github:IssueTriage", "github", "IssueTriage", "triage"),
        ];
        let seed_bundles = [
            seed_bundle("github", &["github:Issue"]),
            seed_bundle("github", &["github:IssueTriage"]),
        ];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = coverage_assessment(
            vec![("list issues", vec![]), ("show labels", vec![0])],
            vec![(0, vec![0, 1]), (1, vec![0, 1])],
            &tables,
        );
        assert_eq!(raw.decision, SeedSelectionDecision::Ready);
        assert_eq!(raw.selected_ids, vec!["github:Issue"]);
    }

    #[test]
    fn cross_provider_requirement_split_clarifies_instead_of_hard_miss() {
        let bundles = vec![
            bundle("github:PullRequest", "github", "PullRequest", "pr_create"),
            bundle("gitlab:MergeRequest", "gitlab", "MergeRequest", "mr_create"),
        ];
        let seed_bundles = [
            seed_bundle_scored("github", &["github:PullRequest"], 10),
            seed_bundle_scored("gitlab", &["gitlab:MergeRequest"], 10),
        ];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = coverage_assessment(
            vec![
                ("open merge request", vec![]),
                ("request review", vec![]),
            ],
            vec![(0, vec![1]), (1, vec![0])],
            &tables,
        );
        assert_eq!(raw.decision, SeedSelectionDecision::Clarify);
        assert_eq!(raw.alternative_sets.len(), 2);
    }

    #[test]
    fn partial_empty_with_multi_provider_support_clarifies() {
        let bundles = vec![
            bundle("gmail:Message", "gmail", "Message", "message_search"),
            bundle("outlook:Message", "outlook", "Message", "message_search"),
        ];
        let seed_bundles = [
            seed_bundle("gmail", &["gmail:Message"]),
            seed_bundle("outlook", &["outlook:Message"]),
        ];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = coverage_assessment(
            vec![
                ("list community posts", vec![]),
                ("drop spam bots", vec![]),
            ],
            vec![(0, vec![0, 1]), (1, vec![])],
            &tables,
        );
        assert_eq!(raw.decision, SeedSelectionDecision::Clarify);
        assert_eq!(raw.alternative_sets.len(), 2);
        assert_eq!(raw.uncovered_requirements, vec!["drop spam bots"]);
    }

    #[test]
    fn catalog_anchored_cross_provider_requirements_federate_ready() {
        let bundles = vec![
            bundle("gitlab:MergeRequest", "gitlab", "MergeRequest", "mr_list"),
            bundle("jira:Issue", "jira", "Issue", "issue_list"),
        ];
        let seed_bundles = [
            seed_bundle("gitlab", &["gitlab:MergeRequest"]),
            seed_bundle("jira", &["jira:Issue"]),
        ];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = resolve_seed_coverage_assessment(
            vec![
                (
                    0,
                    "Link GitLab merge requests for the release".into(),
                    vec![],
                ),
                (1, "Track Jira release-note issues".into(), vec![]),
            ],
            vec![(0, vec![0]), (1, vec![1])],
            String::new(),
            &tables,
            "",
        )
        .expect("resolve coverage");
        assert_eq!(raw.decision, SeedSelectionDecision::Ready);
        assert_eq!(
            raw.selected_ids,
            vec!["gitlab:MergeRequest", "jira:Issue"]
        );
    }

    #[test]
    fn unbranded_single_provider_ready_expands_to_clarify() {
        let bundles = vec![
            bundle("gmail:Message", "gmail", "Message", "message_search"),
            bundle("outlook:Message", "outlook", "Message", "message_search"),
        ];
        let seed_bundles = [
            seed_bundle("gmail", &["gmail:Message"]),
            seed_bundle("outlook", &["outlook:Message"]),
        ];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = resolve_seed_coverage_assessment(
            vec![(0, "find finance invoices".into(), vec![])],
            vec![(0, vec![0])],
            String::new(),
            &tables,
            "Find invoices forwarded from finance that mention NET-45",
        )
        .expect("resolve coverage");
        assert_eq!(raw.decision, SeedSelectionDecision::Clarify);
        assert_eq!(raw.alternative_sets.len(), 2);
    }

    #[test]
    fn model_listed_multi_provider_intersection_clarifies() {
        let bundles = vec![
            bundle("gmail:Message", "gmail", "Message", "message_search"),
            bundle("outlook:Message", "outlook", "Message", "message_search"),
        ];
        let seed_bundles = [
            seed_bundle_scored("gmail", &["gmail:Message"], 10),
            seed_bundle_scored("outlook", &["outlook:Message"], 9),
        ];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = coverage_assessment(
            vec![("list finance invoices", vec![])],
            vec![(0, vec![0, 1])],
            &tables,
        );
        assert_eq!(raw.decision, SeedSelectionDecision::Clarify);
        assert_eq!(raw.alternative_sets.len(), 2);
    }

    #[test]
    fn read_only_ready_blocked_for_finetune_host_intent() {
        let bundles = vec![bundle("jira:Issue", "jira", "Issue", "issue_list")];
        let tables =
            build_seed_bundle_index_tables(&[seed_bundle("jira", &["jira:Issue"])], &bundles)
                .expect("build tables");
        let raw = resolve_seed_coverage_assessment(
            vec![(0, "read closed Jira issues".into(), vec![])],
            vec![(0, vec![0])],
            String::new(),
            &tables,
            "Fine-tune an ML model using closed Jira issues as training data",
        )
        .expect("resolve coverage");
        assert_eq!(raw.decision, SeedSelectionDecision::HardMiss);
        assert!(raw.selected_ids.is_empty());
    }

    #[test]
    fn cross_provider_create_does_not_swap_issue_to_comment() {
        let bundles = vec![
            bundle("github:Issue", "github", "Issue", "issue_query"),
            bundle(
                "github:IssueComment",
                "github",
                "IssueComment",
                "issue_comment_create",
            ),
            bundle("clickup:Task", "clickup", "Task", "task_create"),
        ];
        let seed_bundles = [
            seed_bundle("github", &["github:Issue"]),
            seed_bundle("github", &["github:IssueComment"]),
            seed_bundle("clickup", &["clickup:Task"]),
        ];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = resolve_seed_coverage_assessment(
            vec![
                (0, "Read/list open GitHub issues".into(), vec![]),
                (1, "Create ClickUp tasks".into(), vec![0]),
            ],
            vec![(0, vec![0]), (1, vec![2])],
            String::new(),
            &tables,
            "Sync open GitHub issues with ClickUp tasks for the launch checklist.",
        )
        .expect("resolve coverage");
        assert_eq!(raw.decision, SeedSelectionDecision::Ready);
        assert_eq!(raw.selected_ids.len(), 2);
        assert!(raw.selected_ids.contains(&"github:Issue".to_string()));
        assert!(raw.selected_ids.contains(&"clickup:Task".to_string()));
    }

    #[test]
    fn federated_merge_caps_one_root_per_catalog() {
        let bundles = vec![
            bundle("gitlab:MergeRequest", "gitlab", "MergeRequest", "mr_list"),
            bundle(
                "gitlab:MergeRequestNote",
                "gitlab",
                "MergeRequestNote",
                "note_list",
            ),
            bundle("jira:Issue", "jira", "Issue", "issue_list"),
        ];
        let seed_bundles = [
            seed_bundle(
                "gitlab",
                &["gitlab:MergeRequest", "gitlab:MergeRequestNote"],
            ),
            seed_bundle("jira", &["jira:Issue"]),
        ];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = resolve_seed_coverage_assessment(
            vec![
                (
                    0,
                    "Link GitLab merge requests for the release".into(),
                    vec![],
                ),
                (1, "Track Jira release-note issues".into(), vec![]),
            ],
            vec![(0, vec![0]), (1, vec![1])],
            String::new(),
            &tables,
            "",
        )
        .expect("resolve coverage");
        assert_eq!(raw.decision, SeedSelectionDecision::Ready);
        assert_eq!(
            raw.selected_ids,
            vec!["gitlab:MergeRequest", "jira:Issue"]
        );
    }

    #[test]
    fn non_context_singleton_replaces_issue_triage_context() {
        let bundles = vec![
            bundle_with_hints(
                "github:Issue",
                "github",
                "Issue",
                "issue_list",
                "Query",
                "labels→Label",
            ),
            bundle_with_hints(
                "github:IssueTriageContext",
                "github",
                "IssueTriageContext",
                "triage",
                "Query",
                "labels→Label",
            ),
        ];
        let seed_bundles = [
            seed_bundle("github", &["github:Issue"]),
            seed_bundle("github", &["github:IssueTriageContext"]),
        ];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = coverage_assessment(
            vec![("list issues with labels", vec![])],
            vec![(0, vec![1])],
            &tables,
        );
        assert_eq!(raw.decision, SeedSelectionDecision::Ready);
        assert_eq!(raw.selected_ids, vec!["github:Issue"]);
    }

    #[test]
    fn relation_nav_prefers_singleton_issue_over_issue_label_combo() {
        let bundles = vec![
            bundle_with_hints(
                "github:Issue",
                "github",
                "Issue",
                "issue_list",
                "Query",
                "labels→Label",
            ),
            bundle("github:Label", "github", "Label", "label_list"),
        ];
        let seed_bundles = [
            seed_bundle("github", &["github:Issue"]),
            seed_bundle("github", &["github:Issue", "github:Label"]),
        ];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = coverage_assessment(
            vec![("list issues with labels", vec![])],
            vec![(0, vec![0, 1])],
            &tables,
        );
        assert_eq!(raw.decision, SeedSelectionDecision::Ready);
        assert_eq!(raw.selected_ids, vec!["github:Issue"]);
    }

    #[test]
    fn share_intent_prefers_page_over_database() {
        let bundles = vec![
            bundle_with_hints(
                "notion:Page",
                "notion",
                "Page",
                "page_share",
                "Action",
                "",
            ),
            bundle("notion:Database", "notion", "Database", "db_list"),
        ];
        let seed_bundles = [
            seed_bundle("notion", &["notion:Database"]),
            seed_bundle("notion", &["notion:Page"]),
        ];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = resolve_seed_coverage_assessment(
            vec![(0, "grant read-only access to the brief".into(), vec![])],
            vec![(0, vec![0])],
            String::new(),
            &tables,
            "Grant read-only access to the strategy brief page",
        )
        .expect("resolve coverage");
        assert_eq!(raw.decision, SeedSelectionDecision::Ready);
        assert_eq!(raw.selected_ids, vec!["notion:Page"]);
    }

    #[test]
    fn mutation_intent_prefers_issue_over_sprint_snapshot() {
        let bundles = vec![
            bundle_with_hints(
                "jira:Issue",
                "jira",
                "Issue",
                "issue_transition",
                "Action",
                "",
            ),
            bundle(
                "jira:SprintBoardSnapshot",
                "jira",
                "SprintBoardSnapshot",
                "sprint_get",
            ),
        ];
        let seed_bundles = [
            seed_bundle("jira", &["jira:SprintBoardSnapshot"]),
            seed_bundle("jira", &["jira:Issue"]),
        ];
        let tables = build_seed_bundle_index_tables(&seed_bundles, &bundles).expect("build tables");
        let raw = resolve_seed_coverage_assessment(
            vec![(0, "transition issue to Done".into(), vec![])],
            vec![(0, vec![0])],
            String::new(),
            &tables,
            "Move the blocker ticket to Done in the current sprint",
        )
        .expect("resolve coverage");
        assert_eq!(raw.decision, SeedSelectionDecision::Ready);
    assert_eq!(raw.selected_ids, vec!["jira:Issue"]);
}
