use super::*;

fn mixed() -> (RetrievalReceipt, CatalogEvidence) {
    let (mut catalogs, retrieval) = super::reference_index::matrix();
    catalogs
        .get_mut("matrix")
        .unwrap()
        .capabilities
        .get_mut("opaque")
        .unwrap()
        .description = "λ".repeat(witnesses::MAX_BYTES);
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    (retrieval, evidence)
}
fn intent() -> IntentProvenance {
    IntentProvenance::from_turns(["Prepare material while preserving originals.".into()]).unwrap()
}

#[test]
fn budget_outcomes_are_recomputed_when_evidence_changes() {
    let (retrieval, mut evidence) = mixed();
    assert_eq!(
        prepare_selection(RELEVANCE_MODEL, &intent(), &retrieval, &evidence)
            .unwrap()
            .budget_retained()
            .len(),
        1
    );
    evidence
        .capabilities
        .get_mut(&CapabilityRef {
            catalog: "matrix".into(),
            capability: "opaque".into(),
        })
        .unwrap()
        .operation
        .contract = "Performs an unspecified configured change.".into();
    let fresh = prepare_selection(RELEVANCE_MODEL, &intent(), &retrieval, &evidence).unwrap();
    assert!(fresh.budget_retained().is_empty());
    assert_eq!(
        fresh
            .batches()
            .iter()
            .map(|b| b.bindings.len())
            .sum::<usize>(),
        retrieval.candidates.len()
    );
}

#[tokio::test]
async fn retained_consumer_still_requires_its_authorized_prerequisite_closure() {
    let (mut catalogs, retrieval, deployments) = super::compact::prerequisite_fixture();
    catalogs
        .get_mut("matrix")
        .unwrap()
        .capabilities
        .get_mut("read")
        .unwrap()
        .description = "λ".repeat(witnesses::MAX_BYTES);
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &deployments, &|_| true).unwrap();
    let receipt = super::super::judge_candidates(
        RELEVANCE_MODEL,
        &intent(),
        &retrieval,
        &evidence,
        |_| async { unreachable!("oversized consumer must not dispatch") },
    )
    .await
    .unwrap();
    let references = catalogs.iter().map(|(id, cgs)| (id.clone(), cgs)).collect();
    let selected = receipt.selected(&retrieval);
    let closure = plasm_core::prerequisites::prerequisite_closure(
        &references,
        &deployments,
        &selected,
        &BTreeSet::from(["matrix".into()]),
    )
    .unwrap();
    assert!(!closure.edges.is_empty());
    let denied = &closure.edges[0].provider;
    assert!(matches!(
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &deployments, &|r| r != denied),
        Err(DiscoveryMatcherError::UnauthorizedWitness(_))
    ));
}

#[tokio::test]
async fn only_oversized_candidates_are_retained_without_dispatch_or_fake_judgment() {
    let (retrieval, evidence) = mixed();
    let mut calls = 0;
    let receipt = super::super::judge_candidates(
        RELEVANCE_MODEL,
        &intent(),
        &retrieval,
        &evidence,
        |batch| {
            calls += 1;
            assert!(batch.body().len() <= witnesses::MAX_BYTES);
            assert!(batch
                .questions
                .iter()
                .all(|q| q.reference.capability != "opaque"));
            let raw = response(&batch, &vec![MatchChoice::Unrelated; batch.bindings.len()]);
            async move { Ok(raw) }
        },
    )
    .await
    .unwrap();
    assert_eq!(
        calls,
        prepare_selection(RELEVANCE_MODEL, &intent(), &retrieval, &evidence)
            .unwrap()
            .batches()
            .len()
    );
    assert_eq!(receipt.matches.len(), retrieval.candidates.len());
    assert_eq!(
        receipt
            .selected(&retrieval)
            .iter()
            .map(|r| r.capability.as_str())
            .collect::<Vec<_>>(),
        vec!["opaque"]
    );
    assert_eq!(receipt.budget_retained.len(), 1);
    let budget = &receipt.budget_retained[0];
    assert_eq!(budget.reference.capability, "opaque");
    assert!(budget.bytes > budget.byte_limit);
    assert_eq!(budget.byte_limit, 16_000);
    assert_eq!(budget.capability_limit, 8);
    let wire = serde_json::to_value(&receipt).unwrap();
    assert_eq!(
        serde_json::from_value::<CapabilityMatchReceipt>(wire.clone()).unwrap(),
        receipt
    );
    assert!(wire["budget_retained"][0].get("probabilities").is_none());
}

#[tokio::test]
async fn count_overflow_retains_complete_candidate_without_any_provider_call() {
    let (retrieval, evidence) = super::bounded_scopes::matrix(16, 0);
    let receipt = super::super::judge_candidates(
        RELEVANCE_MODEL,
        &intent(),
        &retrieval,
        &evidence,
        |_| async { unreachable!("count overflow must not dispatch") },
    )
    .await
    .unwrap();
    assert_eq!(receipt.matches[0].choice, MatchChoice::Uncertain);
    assert_eq!(receipt.budget_retained[0].capabilities, 17);
    assert_eq!(
        receipt.selected(&retrieval),
        vec![retrieval.candidates[0].reference.clone()]
    );
    assert_eq!(
        evidence.capabilities.len(),
        17,
        "no catalogue evidence was pruned"
    );
}

#[tokio::test]
async fn oversized_scope_does_not_hide_invalid_evidence_before_dispatch() {
    let (retrieval, mut evidence) = mixed();
    let opaque = CapabilityRef {
        catalog: "matrix".into(),
        capability: "opaque".into(),
    };
    evidence.bindings.push(witnesses::Binding::InputSource {
        from: witnesses::Endpoint {
            capability: opaque.clone(),
            slot: witnesses::Slot::Field {
                field: "nonexistent".into(),
            },
        },
        to: witnesses::Endpoint {
            capability: CapabilityRef {
                catalog: "matrix".into(),
                capability: "prepare".into(),
            },
            slot: witnesses::Slot::Input(plasm_core::prerequisites::InputSourceTarget::Argument {
                input: plasm_core::prerequisites::InputPath {
                    lane: plasm_core::prerequisites::InputLane::Arguments,
                    path: vec!["resource".into()],
                },
            }),
        },
        collect: false,
    });
    let result = super::super::judge_candidates(
        RELEVANCE_MODEL,
        &intent(),
        &retrieval,
        &evidence,
        |_| async { unreachable!("invalid evidence must abort the whole preparation") },
    )
    .await;
    assert!(matches!(
        result,
        Err(DiscoveryMatcherError::InvalidWitness { .. })
    ));
}

#[tokio::test]
async fn provider_fault_still_aborts_with_budget_retention_pending() {
    let (retrieval, evidence) = mixed();
    let result = super::super::judge_candidates(
        RELEVANCE_MODEL,
        &intent(),
        &retrieval,
        &evidence,
        |_| async {
            Err(Box::new(std::io::Error::other("provider failed")) as DecisionProviderError)
        },
    )
    .await;
    assert!(matches!(result, Err(DiscoveryMatcherError::Provider(_))));
}

#[test]
fn sequential_cached_path_preserves_budget_receipts_and_requires_all_answers() {
    let (retrieval, evidence) = mixed();
    let prepared = prepare_selection(RELEVANCE_MODEL, &intent(), &retrieval, &evidence).unwrap();
    let expected = prepared.budget_retained().to_vec();
    assert!(matches!(
        SelectionPages::from_prepared(
            prepare_selection(RELEVANCE_MODEL, &intent(), &retrieval, &evidence).unwrap()
        )
        .finish(&retrieval),
        Err(DiscoveryMatcherError::UnprocessedPages)
    ));
    let mut pages = SelectionPages::from_prepared(prepared);
    while let Some(batch) = pages.current() {
        let raw = response(batch, &vec![MatchChoice::Uncertain; batch.bindings.len()]);
        pages.accept(&raw).unwrap();
    }
    let receipt = pages.finish(&retrieval).unwrap();
    assert_eq!(receipt.budget_retained, expected);
    assert_eq!(
        receipt.selected(&retrieval).len(),
        retrieval.candidates.len()
    );
    assert_eq!(
        receipt
            .matches
            .iter()
            .filter(|m| m.choice == MatchChoice::Uncertain)
            .count(),
        retrieval.candidates.len()
    );
}
