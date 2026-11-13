//! Explicit recall tests: no lexical fixture may manufacture the candidate set.
use super::*;

#[path = "continuation_diagnostic.rs"]
mod continuation_diagnostic;

#[test]
fn shared_routing_fixture_uses_current_native_policy() {
    let packet: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../fixtures/discovery/partial-routing.json"
    ))
    .unwrap();
    assert_eq!(packet["routing"]["search"]["policy"], SEARCH_POLICY);
}

#[test]
fn unfinished_and_uncertain_operations_are_distinct_from_no_match() {
    let reference = CapabilityRef {
        catalog: "matrix".into(),
        capability: "read".into(),
    };
    let mut receipt = SearchReceipt::default();
    assert!(!receipt.has_incomplete_operation_judgments());
    receipt.deferred.push(DeferredCandidate {
        reference: reference.clone(),
        reason: DeferredReason::OperationPending,
    });
    assert!(receipt.has_pending_operation_judgments());
    assert!(receipt.has_incomplete_operation_judgments());
    receipt.deferred[0] = DeferredCandidate {
        reference,
        reason: DeferredReason::OperationUncertain,
    };
    assert!(!receipt.has_pending_operation_judgments());
    assert!(receipt.has_incomplete_operation_judgments());
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

#[test]
fn operation_packet_uses_the_remaining_turn_budget() {
    use super::super::operations::{MatrixScope, OperationMatrix, OperationSet, RequestIdentity};
    let catalogs = tests::fixture();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let request = "Read the source, process the result, and inspect the output.";
    let intent = IntentProvenance::from_turns([request.into()]).unwrap();
    let operations = OperationSet::from_request(request).unwrap();
    let authorized = index.candidates.keys().cloned().collect();
    let scope = MatrixScope::new(
        "session",
        "frozen",
        &authorized,
        RequestIdentity::from_request(request),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(scope, request, operations, &authorized, vec![]).unwrap();
    let visits = BTreeMap::new();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "frozen",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
        visits: &visits,
    };
    let recall = recall(&index, &["process", "source_read"]);
    let (_, full) = prepare_operation_wave(
        context,
        &index,
        &recall,
        &mut matrix,
        &BTreeMap::new(),
        search_budget::MAX_INPUT_BYTES,
        &mut connected::PacketCache::default(),
    )
    .unwrap();
    let full = full.unwrap();
    let full_questions = serde_json::from_str::<serde_json::Value>(full.body()).unwrap()
        ["questions"]
        .as_object()
        .unwrap()
        .len();
    assert!(full_questions > 1);
    let remaining = full.body().len() - 1;
    let (_, reduced) = prepare_operation_wave(
        context,
        &index,
        &recall,
        &mut matrix,
        &BTreeMap::new(),
        remaining,
        &mut connected::PacketCache::default(),
    )
    .unwrap();
    let reduced = reduced.expect("a smaller valid packet fits the remaining allowance");
    assert!(reduced.body().len() <= remaining);
    let reduced_questions = serde_json::from_str::<serde_json::Value>(reduced.body()).unwrap()
        ["questions"]
        .as_object()
        .unwrap()
        .len();
    assert!(reduced_questions < full_questions);
    let (plan, none) = prepare_operation_wave(
        context,
        &index,
        &recall,
        &mut matrix,
        &BTreeMap::new(),
        0,
        &mut connected::PacketCache::default(),
    )
    .unwrap();
    assert!(none.is_none());
    assert!(plan
        .deferred
        .values()
        .any(|reason| matches!(reason, DeferredReason::InputBytes)));
}

#[test]
fn reused_wave_evidence_preserves_planning_after_visits_change() {
    let catalogs = tests::fixture();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent = IntentProvenance::from_turns(["Read and inspect the sample.".into()]).unwrap();
    let visits = BTreeMap::new();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "frozen",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
        visits: &visits,
    };
    let roots: Vec<_> = ["process", "source_read", "opaque"]
        .into_iter()
        .map(|name| CapabilityRef {
            catalog: "matrix".into(),
            capability: name.into(),
        })
        .collect();
    let mut cache = connected::PacketCache::default();
    let first = connected::plan_recalled_with_deferred(
        &index,
        context,
        roots.clone(),
        &BTreeMap::new(),
        &mut cache,
    )
    .unwrap();
    let mut visits = BTreeMap::new();
    for (reference, _) in first.wave.iter().take(2) {
        visits.insert(reference.clone(), 1);
    }
    let context = SearchContext {
        visits: &visits,
        ..context
    };
    let reused = connected::plan_recalled_with_deferred(
        &index,
        context,
        roots.clone(),
        &BTreeMap::new(),
        &mut cache,
    )
    .unwrap();
    let fresh = connected::plan_recalled_with_deferred(
        &index,
        context,
        roots,
        &BTreeMap::new(),
        &mut connected::PacketCache::default(),
    )
    .unwrap();
    assert_eq!(reused.wave, fresh.wave);
    assert_eq!(
        reused.issued.as_ref().map(|x| x.body()),
        fresh.issued.as_ref().map(|x| x.body())
    );
    assert_eq!(
        format!("{:?}", reused.deferred),
        format!("{:?}", fresh.deferred)
    );
}

#[test]
fn deferred_recall_precedes_unrelated_graph_nominations_at_the_backlog_bound() {
    let catalogs = tests::fixture();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let target = CapabilityRef {
        catalog: "matrix".into(),
        capability: "opaque".into(),
    };
    let mut deferred: BTreeMap<_, _> = (0..150)
        .map(|i| {
            (
                CapabilityRef {
                    catalog: "matrix".into(),
                    capability: format!("a_routine_{i:03}"),
                },
                DeferredReason::InputBytes,
            )
        })
        .collect();
    deferred.insert(target.clone(), DeferredReason::InputBytes);

    let ordered = ordered_deferred(deferred, &recall(&index, &["opaque"]));
    assert_eq!(ordered.len(), 151);
    assert_eq!(ordered[0].reference, target);
    assert!(
        ordered
            .iter()
            .take(crate::discovery_store::CANDIDATE_LIMIT)
            .any(|item| item.reference == target)
    );
}

#[test]
fn connected_wave_preserves_recall_priority_for_operation_scheduling() {
    let catalogs = tests::fixture();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent = IntentProvenance::from_turns(["Inspect the sample".into()]).unwrap();
    let visits = BTreeMap::new();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "frozen",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
        visits: &visits,
    };
    let roots: Vec<_> = ["source_read", "opaque"]
        .into_iter()
        .map(|capability| CapabilityRef {
            catalog: "matrix".into(),
            capability: capability.into(),
        })
        .collect();
    let plan =
        connected::plan_recalled_with_deferred(&index, context, roots.clone(), &BTreeMap::new(), &mut connected::PacketCache::default())
            .unwrap();
    let source = plan
        .wave
        .iter()
        .position(|(key, _)| key == &roots[0])
        .unwrap();
    let opaque = plan
        .wave
        .iter()
        .position(|(key, _)| key == &roots[1])
        .unwrap();
    assert!(source < opaque, "recall rank must survive graph assembly");
}

#[test]
fn overdue_deferrals_use_current_recall_order_instead_of_unbounded_age() {
    let catalogs = tests::fixture();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent = IntentProvenance::from_turns(["Inspect the sample".into()]).unwrap();
    let visits = BTreeMap::new();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "frozen",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
        visits: &visits,
    };
    let roots: Vec<_> = ["process", "source_read", "opaque"]
        .into_iter()
        .map(|capability| CapabilityRef {
            catalog: "matrix".into(),
            capability: capability.into(),
        })
        .collect();
    let deferred = BTreeMap::from([(roots[1].clone(), 3), (roots[2].clone(), 99)]);
    let order = connected::order_recalled(&index, context, roots.clone(), &deferred);
    assert_eq!(order, roots);
}

#[tokio::test]
async fn affirmative_contribution_does_not_end_bounded_candidate_assessment() {
    use super::super::operations::{MatrixScope, OperationMatrix, OperationSet, RequestIdentity};
    let mut catalogs = tests::fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let source = cgs.capabilities["source_read"].clone();
    let names: Vec<_> = (0..8).map(|i| format!("read_alternative_{i}")).collect();
    for name in &names {
        let mut candidate = source.clone();
        candidate.name = name.clone().into();
        candidate.description = format!("Read alternative {name}.");
        cgs.capabilities.insert(name.clone().into(), candidate);
    }
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let request = "Read the source.";
    let intent = IntentProvenance::from_turns([request.into()]).unwrap();
    let operations = OperationSet::from_excerpts(request, &[request.into()]).unwrap();
    let authorized = index.candidates.keys().cloned().collect();
    let scope = MatrixScope::new(
        "session",
        "frozen",
        &authorized,
        RequestIdentity::from_request(request),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(scope, request, operations, &authorized, vec![]).unwrap();
    let visits = BTreeMap::new();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "frozen",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
        visits: &visits,
    };
    let refs: Vec<_> = names.iter().map(String::as_str).collect();
    let (_, _, search) = select_operations_indexed(context, &index, &recall(&index, &refs), &mut matrix, &BTreeMap::new(), |batch| async move {
        let wire: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
        let mut answers: serde_json::Map<String, serde_json::Value> = wire["questions"].as_object().unwrap().keys().map(|key| (key.clone(), serde_json::json!({"type":"choice","choice":"relevant","confidence":1.0,"probabilities":{"relevant":1.0,"unrelated":0.0,"uncertain":0.0}}))).collect();
        complete_contextual_probabilities(&wire, &mut answers);
        Ok(serde_json::json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","answers":answers}).to_string())
    }, |_| async { Ok(()) }).await.unwrap();
    assert!(
        search.requests >= 2,
        "an affirmative judgment must not stop assessment of other recalled candidates"
    );
    assert!(search.requests <= 3);
}

#[tokio::test]
async fn one_discovery_call_advances_multiple_bounded_operation_waves() {
    use super::super::operations::{MatrixScope, OperationMatrix, OperationSet, RequestIdentity};
    let catalogs = tests::fixture();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let request = "Read the source, process the result, and inspect the output.";
    let intent = IntentProvenance::from_turns([request.into()]).unwrap();
    let operations = OperationSet::from_request(request).unwrap();
    let authorized = index.candidates.keys().cloned().collect();
    let scope = MatrixScope::new(
        "session",
        "frozen",
        &authorized,
        RequestIdentity::from_request(request),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(scope, request, operations, &authorized, vec![]).unwrap();
    let visits = BTreeMap::new();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "frozen",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
        visits: &visits,
    };
    let (_, _, search) = select_operations_indexed(context, &index, &recall(&index, &["process", "source_read"]), &mut matrix, &BTreeMap::new(), |batch| async move {
        let wire: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
        let mut answers: serde_json::Map<String, serde_json::Value> = wire["questions"].as_object().unwrap().iter().map(|(key, question)| {
            let relevant = question["instructions"].as_str().unwrap().contains("operation `o1`");
            (key.clone(), serde_json::json!({"type":"choice","choice":if relevant {"relevant"} else {"unrelated"},"confidence":1.0,"probabilities":{"relevant":if relevant {1.0} else {0.0},"unrelated":if relevant {0.0} else {1.0},"uncertain":0.0}}))
        }).collect();
        complete_contextual_probabilities(&wire, &mut answers);
        Ok(serde_json::json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","answers":answers}).to_string())
    }, |_| async { Ok(()) }).await.unwrap();
    assert!(
        search.requests >= 2,
        "one call should advance more than one bounded packet"
    );
    assert!(search.requests <= 3);
    assert!(search.input_bytes <= 48_000);

    let operations = OperationSet::from_request(request).unwrap();
    let scope = MatrixScope::new(
        "session",
        "frozen",
        &authorized,
        RequestIdentity::from_request(request),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(scope, request, operations, &authorized, vec![]).unwrap();
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let (_, matching, search) = select_operations_indexed(context, &index, &recall(&index, &["process", "source_read"]), &mut matrix, &BTreeMap::new(), |batch| {
        let attempt = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        async move {
            if attempt > 0 {
                return Err(Box::new(std::io::Error::other("synthetic provider failure")) as DecisionProviderError);
            }
            let wire: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
            let mut answers: serde_json::Map<String, serde_json::Value> = wire["questions"].as_object().unwrap().iter().map(|(key, question)| {
                let relevant = question["instructions"].as_str().unwrap().contains("operation `o1`");
                (key.clone(), serde_json::json!({"type":"choice","choice":if relevant {"relevant"} else {"unrelated"},"confidence":1.0,"probabilities":{"relevant":if relevant {1.0} else {0.0},"unrelated":if relevant {0.0} else {1.0},"uncertain":0.0}}))
            }).collect();
            complete_contextual_probabilities(&wire, &mut answers);
        Ok(serde_json::json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","answers":answers}).to_string())
        }
    }, |_| async { Ok(()) }).await.unwrap();
    assert!(
        matching
            .matches
            .iter()
            .any(|item| item.choice == MatchChoice::Relevant)
    );
    assert!(search.has_unavailable_provider());
}

#[tokio::test]
async fn connected_planner_schedules_operation_pairs_and_keeps_the_rest_pending() {
    use super::super::operations::{MatrixScope, OperationMatrix, OperationSet, RequestIdentity};
    let catalogs = tests::fixture();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let request = "Read the source, process the result, and inspect the output.";
    let intent = IntentProvenance::from_turns([request.into()]).unwrap();
    let operations = OperationSet::from_request(request).unwrap();
    let authorized = index
        .candidates
        .keys()
        .filter(|reference| allowed.permits(reference))
        .cloned()
        .collect();
    let scope = MatrixScope::new(
        "session",
        "frozen",
        &authorized,
        RequestIdentity::from_request(request),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(scope, request, operations, &authorized, vec![]).unwrap();
    let visits = BTreeMap::new();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "frozen",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
        visits: &visits,
    };
    let (plan, batch) = prepare_operation_wave(
        context,
        &index,
        &recall(&index, &["process", "source_read"]),
        &mut matrix,
        &BTreeMap::new(),
        search_budget::MAX_INPUT_BYTES,
        &mut connected::PacketCache::default(),
    )
    .unwrap();
    let batch = batch.unwrap();
    let wire: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
    assert!(
        wire["state"]["question_evidence"]
            .as_object()
            .unwrap()
            .len()
            <= request_packing::MAX_QUESTIONS
    );
    let mut answers: serde_json::Map<String, serde_json::Value> = wire["questions"].as_object().unwrap().keys().map(|key| (key.clone(), serde_json::json!({"type":"choice","choice":"unrelated","confidence":1.0,"probabilities":{"relevant":0.0,"unrelated":1.0,"uncertain":0.0}}))).collect();
    complete_contextual_probabilities(&wire, &mut answers);
    let response =
        serde_json::json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","answers":answers})
            .to_string();
    let judgments = batch.decode(&response).unwrap();
    matrix
        .record_packet(&batch.issued_pairs(), &judgments)
        .unwrap();
    assert!(matrix.directly_supported().is_empty());
    assert!(!matrix.uncovered_operations().collect::<Vec<_>>().is_empty());
    assert!(!plan.wave.is_empty());
}

#[tokio::test]
async fn supplemental_wave_gets_its_own_packet_under_ordinary_pressure() {
    let mut catalogs = tests::fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let template = cgs.capabilities["source_read"].clone();
    cgs.capabilities.clear();
    for name in ["a", "b", "guide"] {
        let mut cap = template.clone();
        cap.name = name.into();
        cap.description = "authored purpose ".repeat(300);
        cgs.capabilities.insert(name.into(), cap);
    }
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent = IntentProvenance::from_turns(["inspect the requested record".into()]).unwrap();
    let reference = |name: &str| CapabilityRef {
        catalog: "matrix".into(),
        capability: name.into(),
    };
    let visits = BTreeMap::from([(reference("a"), 1), (reference("b"), 1)]);
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "frozen",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
        visits: &visits,
    };
    let ordinary = recall(&index, &["a", "b"]);
    let supplemental = recall(&index, &["guide"]);
    let judge = |batch| async move { Ok(tests::answer(&batch, "unrelated")) };
    let (baseline, _, ordinary_search) =
        select_recalled_indexed(context, &index, &ordinary, judge, |_| async { Ok(()) })
            .await
            .unwrap();
    let (with_guidance, _, search) =
        select_recalled_indexed(context, &index, &supplemental, judge, |_| async { Ok(()) })
            .await
            .unwrap();
    let baseline: BTreeSet<_> = baseline
        .candidates
        .iter()
        .map(|candidate| &candidate.reference)
        .collect();
    let guided: BTreeSet<_> = with_guidance
        .candidates
        .iter()
        .map(|candidate| &candidate.reference)
        .collect();
    assert!(!baseline.is_empty());
    assert!(guided.contains(&reference("guide")));
    assert_eq!(ordinary_search.requests, 1);
    assert_eq!(search.requests, 1);
    assert!(search.input_bytes <= 16_000);
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
        cap.description = "authored purpose ".repeat(650);
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
            assert!(
                result
                    .candidates
                    .iter()
                    .any(|c| c.reference.capability == "d")
            );
            assert!(result.candidates.len() < 4);
        }
        assert_eq!(result.lexical_count, 0);
        assert_eq!(result.vector_count, result.candidates.len());
        assert_eq!(matching.matches.len(), result.candidates.len());
        assert_eq!(search.requests, 1);
        assert!(search.input_bytes <= search_budget::MAX_INPUT_BYTES);
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
    assert!(
        result
            .candidates
            .iter()
            .any(|c| c.reference.capability == "prepared_read")
    );
    assert_eq!(result.vector_count, 1);
    assert_eq!(result.lexical_count, 0);
    for candidate in result.candidates {
        assert!(
            candidate
                .admissions
                .contains(if candidate.reference.capability == "process" {
                    "vector"
                } else {
                    "typed_relationship"
                })
        );
    }
}

fn complete_contextual_probabilities(
    wire: &serde_json::Value,
    answers: &mut serde_json::Map<String, serde_json::Value>,
) {
    for (key, answer) in answers.iter_mut() {
        if wire["questions"][key]["criteria"]
            .get("contextual")
            .is_some()
        {
            answer["probabilities"]["contextual"] = serde_json::json!(0.0);
        }
    }
}
