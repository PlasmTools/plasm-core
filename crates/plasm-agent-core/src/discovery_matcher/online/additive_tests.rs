use super::{
    tests::{answer, fixture},
    *,
};

fn reference(name: &str) -> CapabilityRef {
    CapabilityRef {
        catalog: "matrix".into(),
        capability: name.into(),
    }
}

#[tokio::test]
async fn oversized_acquisition_paths_never_defer_existing_declarations() {
    let names: Vec<_> = fixture()["matrix"].capabilities.keys().cloned().collect();
    for oversized in &names {
        let mut catalogs = fixture();
        catalogs
            .get_mut("matrix")
            .unwrap()
            .capabilities
            .get_mut(oversized)
            .unwrap()
            .description
            .push_str(&" extensive authored contract evidence".repeat(1000));
        for existing in &names {
            let exposed = [reference(existing.as_str())];
            let (_, _, receipt) = select(
                SearchContext {
                    model: RELEVANCE_MODEL,
                    intent: &IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap(),
                    generation: "oversized-existing-path",
                    catalogs: &catalogs,
                    deployments: &Default::default(),
                    allowed: &DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()])),
                    exposed: &exposed,
                },
                |request| async move { Ok(answer(&request, "uncertain")) },
            )
            .await
            .unwrap();
            assert!(
                receipt
                    .deferred
                    .iter()
                    .all(|item| !exposed.contains(&item.reference)),
                "existing={existing}, oversized={oversized}: {:?}",
                receipt.deferred
            );
            assert!(receipt.input_bytes <= search_budget::MAX_INPUT_BYTES);
            assert!(receipt.requests <= 1);
            assert!(exposed.iter().all(|key| receipt.exposed.contains(key)));
            if existing != oversized {
                assert!(receipt.unexplored.contains(&reference(oversized)));
            }
        }
    }
}

#[test]
fn exposed_intermediate_preserves_the_unseen_creator_path() {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent = IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap();
    let exposed = [reference("prepared_read")];
    let order = index.ranking.rank(intent.original(), intent.current());
    assert!(!order.iter().take(2).any(|key| exposed.contains(key)));
    let plan = connected::plan(
        &index,
        SearchContext {
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "exposed-intermediate",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &exposed,
        },
    )
    .unwrap();
    assert!(!plan.wave.iter().any(|(key, _)| exposed.contains(key)));
    let (_, route) = plan
        .wave
        .iter()
        .find(|(key, _)| *key == reference("prepare"))
        .expect("unseen creator must remain a connected candidate");
    assert!(route
        .iter()
        .any(|edge| edge.consumer == reference("process")
            && edge.source == reference("prepared_read")));
    assert!(route
        .iter()
        .any(|edge| edge.consumer == reference("prepared_read")
            && edge.source == reference("prepare")
            && edge.relationship == Relationship::MayCreateResource));
    assert!(plan.issued.unwrap().body().len() <= search_budget::MAX_INPUT_BYTES);
}

#[tokio::test]
async fn new_inspection_gets_the_question_budget_when_processing_is_exposed() {
    let mut catalogs = fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let mut inspection = cgs.capabilities["source_read"].clone();
    inspection.name = "inspection".into();
    inspection.description = "Inspect diagnostic metadata without processing resources.".into();
    let exposed: Vec<_> = cgs
        .capabilities
        .keys()
        .map(|name| reference(name.as_str()))
        .collect();
    cgs.capabilities.insert(inspection.name.clone(), inspection);
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let intent = IntentProvenance::from_turns([
        "Produce spectrum".into(),
        "Cancel processing; inspect diagnostic metadata".into(),
    ])
    .unwrap();
    let (retrieval, matching, receipt) = select(
        SearchContext {
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "inspection-extension",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &exposed,
        },
        |request| {
            assert_eq!(request.bindings.len(), 1);
            async move { Ok(answer(&request, "uncertain")) }
        },
    )
    .await
    .unwrap();
    assert_eq!(matching.selected(&retrieval), vec![reference("inspection")]);
    assert_eq!(receipt.requests, 1);
    assert!(receipt.input_bytes <= search_budget::MAX_INPUT_BYTES);
}

#[tokio::test]
async fn additions_do_not_rejudge_exposed_capabilities_across_intent_changes() {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    for names in [
        vec![],
        vec!["process"],
        vec!["source_read"],
        vec!["prepare", "prepared_read"],
    ] {
        let exposed: Vec<_> = names.into_iter().map(reference).collect();
        let ids: BTreeSet<_> = exposed
            .iter()
            .map(|r| index.candidates[r].id.clone())
            .collect();
        for need in [
            "spectrum",
            "Inspect prepared resources",
            "Cancel processing; inspect resources only",
        ] {
            let intent =
                IntentProvenance::from_turns(["Produce spectrum".into(), need.into()]).unwrap();
            let (retrieval, matching, receipt) = select_indexed(
                SearchContext {
                    model: RELEVANCE_MODEL,
                    intent: &intent,
                    generation: "additive",
                    catalogs: &catalogs,
                    deployments: &deployments,
                    allowed: &allowed,
                    exposed: &exposed,
                },
                &index,
                |request| {
                    assert!(request.bindings.values().all(|id| !ids.contains(id)));
                    async move { Ok(answer(&request, "uncertain")) }
                },
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
            let additions = matching.selected(&retrieval);
            assert!(additions.iter().all(|r| !exposed.contains(r)));
            assert!(receipt.requests <= 1);
            assert_eq!(receipt.exposed, exposed.iter().cloned().collect());
            assert!(receipt.unexplored.iter().all(|r| !exposed.contains(r)));
            assert!(receipt.input_bytes <= search_budget::MAX_INPUT_BYTES);
            // Union is the caller's exposure operation; no relevance decision retracts E.
            let surface: BTreeSet<_> = exposed.iter().cloned().chain(additions).collect();
            assert!(exposed.iter().all(|r| surface.contains(r)));
        }
    }
}

#[tokio::test]
async fn fully_exposed_catalogue_needs_no_provider_request() {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let exposed: Vec<_> = index.candidates.keys().cloned().collect();
    let intent = IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap();
    let (retrieval, matching, receipt) = select_indexed(
        SearchContext {
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "all-exposed",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &exposed,
        },
        &index,
        |_| async { panic!("existing declarations must not be rejudged") },
        |_| async { Ok(()) },
    )
    .await
    .unwrap();
    assert!(matching.selected(&retrieval).is_empty());
    assert_eq!(receipt.requests, 0);
    assert_eq!(receipt.input_bytes, 0);
    assert_eq!(receipt.exposed, exposed.into_iter().collect());
    assert!(receipt.unexplored.is_empty());
}

#[test]
fn exposed_direct_suppliers_remain_contract_evidence_not_questions() {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent = IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap();
    let mut checked = 0;
    for edge in index.graph.edges() {
        let exposed = [edge.source.clone()];
        let context = SearchContext {
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "local-evidence",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &exposed,
        };
        let (issued, members) = packet(&index, &edge.consumer, &[], context).unwrap();
        assert!(members.contains(&edge.source));
        assert!(issued.body().contains(edge.source.capability.as_str()));
        assert_eq!(issued.bindings.len(), 1);
        assert!(issued
            .bindings
            .values()
            .all(|id| *id == index.candidates[&edge.consumer].id));
        checked += 1;
    }
    assert!(checked > 0);
}

#[tokio::test]
async fn invalid_existing_surface_is_rejected_before_dispatch() {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent = IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap();
    for exposed in [
        reference("absent"),
        CapabilityRef {
            catalog: "unauthorized".into(),
            capability: "process".into(),
        },
    ] {
        let error = select_indexed(
            SearchContext {
                model: RELEVANCE_MODEL,
                intent: &intent,
                generation: "invalid-exposure",
                catalogs: &catalogs,
                deployments: &deployments,
                allowed: &allowed,
                exposed: &[exposed],
            },
            &index,
            |_| async { panic!("invalid exposure must not dispatch") },
            |_| async { Ok(()) },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            DiscoveryMatcherError::InvalidWitness { .. }
        ));
    }
}
