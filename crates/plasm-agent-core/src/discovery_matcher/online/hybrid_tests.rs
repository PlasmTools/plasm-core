//! Explicit recall tests: no lexical fixture may manufacture the candidate set.
use super::*;

#[test]
fn shared_routing_fixture_uses_current_native_policy() {
    let packet: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../fixtures/discovery/partial-routing.json"
    ))
    .unwrap();
    assert_eq!(packet["routing"]["search"]["policy"], SEARCH_POLICY);
}

fn recall(index: &CatalogueSearch, names: &[&str]) -> RetrievalReceipt {
    RetrievalReceipt {
        generation: "frozen".into(),
        candidates: names
            .iter()
            .map(|name| {
                let mut c = index.candidates[&CapabilityRef {
                    catalog: "matrix".into(),
                    capability: (*name).into(),
                }]
                    .clone();
                c.admissions = BTreeSet::from(["vector".into()]);
                c
            })
            .collect(),
        lexical_count: 0,
        vector_count: names.len(),
        lexical_truncated: false,
        vector_truncated: false,
    }
}

#[tokio::test]
async fn hybrid_recall_advances_rejected_roots_without_lexical_overlap() {
    let mut catalogs = tests::fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let cap = cgs.capabilities["source_read"].clone();
    cgs.capabilities.clear();
    for name in ["a", "b", "c", "d"] {
        let mut cap = cap.clone();
        cap.name = name.into();
        // Force bounded allocation rather than fitting every independent root.
        cap.description = "authored purpose ".repeat(300);
        cgs.capabilities.insert(name.into(), cap);
    }
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let input = recall(&index, &["d", "c", "b", "a"]);
    let intent = IntentProvenance::from_turns(["xylovariance".into()]).unwrap();
    let mut visits = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for turn in 0..4 {
        let (result, matching, search) = select_recalled_indexed(
            SearchContext {
                model: RELEVANCE_MODEL,
                intent: &intent,
                generation: "frozen",
                catalogs: &catalogs,
                deployments: &deployments,
                allowed: &allowed,
                exposed: &[],
                visits: &visits,
            },
            &index,
            &input,
            |batch| async move { Ok(tests::answer(&batch, "unrelated")) },
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
        if turn == 0 {
            assert!(result
                .candidates
                .iter()
                .any(|c| c.reference.capability == "d"));
            assert!(result.candidates.len() < 4);
        }
        assert_eq!(result.lexical_count, 0);
        assert_eq!(result.vector_count, result.candidates.len());
        assert_eq!(matching.matches.len(), result.candidates.len());
        assert_eq!(search.requests, 1);
        assert!(search.input_bytes <= 16000);
        for c in result.candidates {
            assert!(
                seen.insert(c.reference.clone()),
                "completed rejection must advance the frontier"
            );
            *visits.entry(c.reference).or_default() += 1;
        }
        if seen.len() == 4 {
            break;
        }
    }
    assert_eq!(seen.len(), 4);
}

#[tokio::test]
async fn hybrid_recall_checks_pinned_documents_before_judgment() {
    let catalogs = tests::fixture();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent = IntentProvenance::from_turns(["xylovariance".into()]).unwrap();
    for defect in 0..4 {
        let mut input = recall(&index, &["opaque"]);
        match defect {
            0 => input.generation = "stale".into(),
            1 => input.candidates[0].reference.catalog = "denied".into(),
            2 => input.candidates.push(input.candidates[0].clone()),
            _ => {
                input.candidates[0]
                    .document
                    .text
                    .push_str(" forged purpose");
                input.candidates[0].document.text_hash =
                    plasm_core::catalog_discovery::content_hash(
                        input.candidates[0].document.text.as_bytes(),
                    );
            }
        }
        let error = select_recalled_indexed(
            SearchContext {
                model: RELEVANCE_MODEL,
                intent: &intent,
                generation: "frozen",
                catalogs: &catalogs,
                deployments: &deployments,
                allowed: &allowed,
                exposed: &[],
                visits: &BTreeMap::new(),
            },
            &index,
            &input,
            |_| async { panic!("invalid recall must never reach the decision provider") },
            |_| async { Ok(()) },
        )
        .await
        .unwrap_err();
        assert!(match defect {
            0 => matches!(error, DiscoveryMatcherError::RetrievalGenerationMismatch),
            1 | 2 => matches!(error, DiscoveryMatcherError::Retrieval(_)),
            _ => matches!(error, DiscoveryMatcherError::InvalidWitness { .. }),
        });
    }
}

#[tokio::test]
async fn hybrid_recall_keeps_typed_suppliers_distinct_from_vector_admission() {
    let catalogs = tests::fixture();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let input = recall(&index, &["process"]);
    let intent = IntentProvenance::from_turns(["xylovariance".into()]).unwrap();
    let (result, _, _) = select_recalled_indexed(
        SearchContext {
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "frozen",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &[],
            visits: &BTreeMap::new(),
        },
        &index,
        &input,
        |batch| async move { Ok(tests::answer(&batch, "uncertain")) },
        |_| async { Ok(()) },
    )
    .await
    .unwrap();
    assert!(result
        .candidates
        .iter()
        .any(|c| c.reference.capability == "prepared_read"));
    assert_eq!(result.vector_count, 1);
    assert_eq!(result.lexical_count, 0);
    for candidate in result.candidates {
        assert!(candidate
            .admissions
            .contains(if candidate.reference.capability == "process" {
                "vector"
            } else {
                "typed_relationship"
            }));
    }
}
