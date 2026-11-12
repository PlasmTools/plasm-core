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

#[test]
fn continuation_recall_survives_exposed_hub_growth() {
    for (count, contract_size) in [(8, 0), (8, 160), (64, 160)] {
        let mut catalogs = fixture();
        let cgs = catalogs.get_mut("matrix").unwrap();
        let consumer = cgs.capabilities["prepare"].clone();
        cgs.capabilities
            .retain(|name, _| ["source_read", "opaque"].contains(&name.as_str()));
        cgs.capabilities.get_mut("opaque").unwrap().description = format!(
            "Apply the independent routine. {}",
            "Authored independent contract. ".repeat(contract_size)
        );
        for n in 0..count {
            let mut cap = consumer.clone();
            cap.name = format!("consumer_{n:03}").into();
            cap.description = format!(
                "Prepare material for station {n}. {}",
                "Authored station contract. ".repeat(160)
            );
            cgs.capabilities.insert(cap.name.clone(), cap);
        }
        let deployments = Default::default();
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
        let intent = IntentProvenance::from_turns([
            "Apply the independent routine; preserve source material.".into(),
        ])
        .unwrap();
        let exposed = [reference("source_read")];
        let visits = Default::default();
        let context = SearchContext {
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "continuation-v1",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &exposed,
            visits: &visits,
        };
        for first in ["opaque", "consumer_000"] {
            let mut recall = vec![reference(first)];
            recall.extend(
                (0..count)
                    .map(|n| reference(&format!("consumer_{n:03}")))
                    .filter(|r| r != &reference(first)),
            );
            let plan = connected::plan_recalled(&index, context, recall).unwrap();
            assert!(
                plan.wave.iter().any(|(key, _)| key == &reference(first)),
                "current recalled root {first} starved by {count} connected consumers"
            );
            assert!(plan.issued.unwrap().body().len() <= search_budget::MAX_INPUT_BYTES);
        }
    }
}

#[test]
fn independent_outcomes_advance_after_retention_or_rejection() {
    for count in [8, 64] {
        for retain_first in [false, true] {
            let mut catalogs = fixture();
            let cgs = catalogs.get_mut("matrix").unwrap();
            let consumer = cgs.capabilities["prepare"].clone();
            let independent = cgs.capabilities["opaque"].clone();
            cgs.capabilities
                .retain(|name, _| name.as_str() == "source_read");
            for name in ["outcome_a", "outcome_b"] {
                let mut cap = independent.clone();
                cap.name = name.into();
                cap.description = format!(
                    "Perform independent {name}. {}",
                    "Authored independent contract. ".repeat(160)
                );
                cgs.capabilities.insert(cap.name.clone(), cap);
            }
            for n in 0..count {
                let mut cap = consumer.clone();
                cap.name = format!("consumer_{n:03}").into();
                cap.description = format!(
                    "Prepare material for station {n}. {}",
                    "Authored station contract. ".repeat(160)
                );
                cgs.capabilities.insert(cap.name.clone(), cap);
            }
            let deployments = Default::default();
            let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
            let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
            let intent = IntentProvenance::from_turns([
                "Perform both independent outcomes; preserve original material.".into(),
            ])
            .unwrap();
            let mut exposed = vec![reference("source_read")];
            let mut visits = BTreeMap::new();
            let mut seen = BTreeSet::new();
            for _ in 0..2 {
                let mut recalled = vec![reference("outcome_a"), reference("outcome_b")];
                recalled.extend((0..count).map(|n| reference(&format!("consumer_{n:03}"))));
                let plan = connected::plan_recalled(
                    &index,
                    SearchContext {
                        model: RELEVANCE_MODEL,
                        intent: &intent,
                        generation: "independent-v1",
                        catalogs: &catalogs,
                        deployments: &deployments,
                        allowed: &allowed,
                        exposed: &exposed,
                        visits: &visits,
                    },
                    recalled,
                )
                .unwrap();
                assert!(plan.issued.unwrap().body().len() <= search_budget::MAX_INPUT_BYTES);
                for (key, _) in plan.wave {
                    seen.insert(key.clone());
                    *visits.entry(key.clone()).or_insert(0) += 1;
                    if retain_first && key == reference("outcome_a") {
                        exposed.push(key);
                    }
                }
            }
            assert!(
                seen.contains(&reference("outcome_a")) && seen.contains(&reference("outcome_b")),
                "independent outcome starved: consumers={count}, retain_first={retain_first}"
            );
        }
    }
}

#[test]
fn trial_packet_size_matches_dispatch_encoding_and_stable_labels() {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent =
        IntentProvenance::from_turns(["Inspect calibration; preserve originals.".into()]).unwrap();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "size-parity",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
        visits: &Default::default(),
    };
    let mut local = Vec::new();
    assert_eq!(request_packing::connected_size(&[]).unwrap(), 0);
    for (i, key) in index.candidates.keys().enumerate() {
        assert_eq!(index.labels[key], format!("c{i}"));
        let (scope, _, _) = packet_scope(&index, key, &[], context).unwrap();
        let direct = request_packing::PreparedLocal::for_scope(
            context.model,
            context.intent,
            &index.labels[key],
            &scope,
        )
        .unwrap();
        let encoded = packet(&index, key, &[], context).unwrap().0;
        assert_eq!(
            request_packing::connected_size(&[&direct]).unwrap(),
            request_packing::connected(vec![encoded])
                .unwrap()
                .unwrap()
                .body()
                .len()
        );
        local.push(packet(&index, key, &[], context).unwrap().0);
        for packets in [
            local.clone(),
            local.iter().rev().cloned().collect(),
            local.iter().step_by(2).cloned().collect(),
        ] {
            let prepared = packets
                .iter()
                .map(request_packing::PreparedLocal::new)
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(
                request_packing::connected_size(&prepared.iter().collect::<Vec<_>>()).unwrap(),
                request_packing::connected(packets)
                    .unwrap()
                    .unwrap()
                    .body()
                    .len()
            );
        }
    }
    // Serialization boundaries: escaped keys/values, multi-digit reference IDs,
    // same-name definitions with different contents, and distinct current fields.
    let template = local[0].clone();
    let mut variants = Vec::new();
    for n in 0..105 {
        let mut batch = template.clone();
        let mut body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
        let question = body["questions"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .clone();
        body["questions"] = json!({format!("q{n}"): question});
        body["state"]["outcome"] = json!("Observe \"α\"; preserve originals.\n");
        body["state"]["discovery_need"] = json!("Inspect β");
        body["state"]["types"]["values"]
            .as_object_mut()
            .unwrap()
            .insert(
                "quoted\"\\\n☄".into(),
                json!({"description":format!("case {n}: \"α\"\\\n") }),
            );
        body["state"]["bindings"] = json!([{ "abstract_binding": n }]);
        batch.body = serde_json::to_string(&body).unwrap();
        variants.push(batch);
    }
    for packets in [variants.clone(), variants.iter().rev().cloned().collect()] {
        let prepared = packets
            .iter()
            .map(request_packing::PreparedLocal::new)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            request_packing::connected_size(&prepared.iter().collect::<Vec<_>>()).unwrap(),
            request_packing::connected(packets)
                .unwrap()
                .unwrap()
                .body()
                .len()
        );
    }
    let prepared = request_packing::PreparedLocal::new(&template).unwrap();
    assert!(matches!(
        request_packing::connected_size(&[&prepared, &prepared]),
        Err(DiscoveryMatcherError::DuplicateIssuedQuestion)
    ));
    let different = request_packing::PreparedLocal::new(&variants[0]).unwrap();
    assert!(matches!(
        request_packing::connected_size(&[&prepared, &different]),
        Err(DiscoveryMatcherError::InvalidConnectedContext)
    ));
}

#[test]
fn oversized_optional_route_does_not_block_affordable_direct_root() {
    let mut catalogs = fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    cgs.capabilities
        .retain(|name, _| ["source_read", "prepare", "opaque"].contains(&name.as_str()));
    let template = cgs.capabilities.swap_remove("opaque").unwrap();
    for name in ["a", "b"] {
        let mut cap = template.clone();
        cap.name = name.into();
        cap.description = "Inspect calibration telemetry precisely.".into();
        cgs.capabilities.insert(name.into(), cap);
    }
    cgs.capabilities.get_mut("source_read").unwrap().description = "Read calibration.".into();
    cgs.capabilities.get_mut("prepare").unwrap().description =
        "Inspect calibration telemetry precisely. ".repeat(900);
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent =
        IntentProvenance::from_turns(["Inspect calibration telemetry precisely".into()]).unwrap();
    let exposed = [reference("prepare")];
    let context = SearchContext {
        visits: &Default::default(),
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "route-not-capability",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &exposed,
    };
    assert!(
        packet(&index, &reference("source_read"), &[], context)
            .unwrap()
            .0
            .body()
            .len()
            < search_budget::MAX_INPUT_BYTES
    );
    let plan = connected::plan(&index, context).unwrap();
    assert!(
        plan.wave
            .iter()
            .any(|(key, path)| key == &reference("source_read") && path.is_empty()),
        "an oversized optional ancestry must not exclude an affordable direct judgment"
    );
    assert!(!plan.deferred.contains_key(&reference("source_read")));
    assert!(plan.issued.unwrap().body().len() <= search_budget::MAX_INPUT_BYTES);
}

#[tokio::test]
async fn current_discovery_need_is_not_drowned_by_original_context() {
    let mut catalogs = fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let template = cgs.capabilities["opaque"].clone();
    cgs.capabilities.clear();
    for (name, description) in [
        ("old", "Inspect calibration."),
        ("overlap", "Inspect calibration with optional fluorescence."),
        ("target", "Measure fluorescence."),
    ] {
        let mut cap = template.clone();
        cap.name = name.into();
        cap.description = format!("{description}{}", " Authored detail.".repeat(560));
        cgs.capabilities.insert(name.into(), cap);
    }
    let intent = IntentProvenance::from_turns([
        "Inspect calibration; preserve originals. If measurements fail, report uncertainty.".into(),
        "Measure fluorescence; preserve originals. If measurements fail, report uncertainty."
            .into(),
    ])
    .unwrap();
    let (retrieval, _, receipt) = select(
        SearchContext {
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "need-isolation",
            catalogs: &catalogs,
            deployments: &Default::default(),
            allowed: &DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()])),
            exposed: &[],
            visits: &Default::default(),
        },
        |request| async move {
            assert!(request.body().contains("preserve originals"));
            assert!(request.body().contains("If measurements fail"));
            let body: serde_json::Value = serde_json::from_str(request.body()).unwrap();
            assert!(body["state"].get("intent_history").is_none());
            Ok(answer(&request, "uncertain"))
        },
    )
    .await
    .unwrap();
    assert!(retrieval
        .candidates
        .iter()
        .any(|c| c.reference == reference("target")));
    assert!(receipt.input_bytes <= search_budget::MAX_INPUT_BYTES);
}

#[test]
fn parent_get_observations_are_searchable_without_leaking_to_peer_operations() {
    let mut catalogs = fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    cgs.entities.get_mut("Source").unwrap().relations.insert("parts".into(),
        serde_json::from_value(serde_json::json!({
            "name":"parts", "description":"Fluorescent specimens contained in the enclosure.",
            "target_resource":"Prepared", "cardinality":"many",
            "materialize":{"kind":"from_parent_get", "path":[{"key":"parts"},{"wildcard":true}], "collection_coverage":"complete"}
        })).unwrap());
    let mut peer = cgs.capabilities["opaque"].clone();
    peer.name = "unobserving_peer".into();
    peer.domain = "Source".into();
    cgs.capabilities.insert("unobserving_peer".into(), peer);
    let documents = plasm_core::catalog_discovery::capability_documents(cgs).unwrap();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &Default::default(), &allowed).unwrap();
    assert_eq!(
        index.ranking.rank("fluorescent specimens"),
        vec![reference("source_read")]
    );
    for candidate in index.candidates.values() {
        assert_eq!(
            &candidate.document,
            documents
                .iter()
                .find(|d| d.capability == candidate.reference.capability)
                .unwrap()
        );
    }
}

/// Characterizes starvation, not desired behaviour: an individually oversized
/// contract cannot be recovered by a better rank or an affirmative classifier.
#[tokio::test]
async fn admission_diagnostic_oversized_target_never_reaches_judgment() {
    for oversized in [false, true] {
        let mut catalogs = fixture();
        let cgs = catalogs.get_mut("matrix").unwrap();
        cgs.capabilities.retain(|name, _| name.as_str() == "opaque");
        let cap = cgs.capabilities.get_mut("opaque").unwrap();
        cap.description = "Inspect calibration telemetry.".into();
        if oversized {
            cap.description
                .push_str(&" Authored calibration details.".repeat(700));
        }
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        for need in [
            "Inspect calibration telemetry",
            "Read calibration telemetry now",
        ] {
            let intent =
                IntentProvenance::from_turns(["Inspect calibration telemetry".into(), need.into()])
                    .unwrap();
            let calls = std::sync::atomic::AtomicUsize::new(0);
            let (_, _, receipt) = select(
                SearchContext {
                    visits: &Default::default(),
                    model: RELEVANCE_MODEL,
                    intent: &intent,
                    generation: "admission-diagnostic",
                    catalogs: &catalogs,
                    deployments: &Default::default(),
                    allowed: &allowed,
                    exposed: &[],
                },
                |request| {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    async move { Ok(answer(&request, "relevant")) }
                },
            )
            .await
            .unwrap();
            assert_eq!(
                calls.load(std::sync::atomic::Ordering::SeqCst),
                usize::from(!oversized)
            );
            assert_eq!(
                receipt.considered.contains(&reference("opaque")),
                !oversized
            );
            assert_eq!(
                receipt
                    .deferred
                    .iter()
                    .any(|d| d.reference == reference("opaque")
                        && matches!(d.reason, DeferredReason::InputBytes)),
                oversized
            );
            assert!(receipt.input_bytes <= search_budget::MAX_INPUT_BYTES);
        }
    }
}

#[test]
fn admission_diagnostic_parent_get_relation_survives_in_referenced_definition() {
    let mut catalogs = fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    cgs.entities.get_mut("Source").unwrap().relations.insert("parts".into(),
        serde_json::from_value(serde_json::json!({
            "name":"parts", "description":"Contained calibrated specimens.",
            "target_resource":"Prepared", "cardinality":"many",
            "materialize":{"kind":"from_parent_get", "path":[{"key":"parts"},{"wildcard":true}], "collection_coverage":"complete"}
        })).unwrap());
    cgs.capabilities
        .get_mut("source_read")
        .unwrap()
        .output_schema = None;
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = Default::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent =
        IntentProvenance::from_turns(["Inspect contained calibrated specimens".into()]).unwrap();
    let (issued, _) = packet(
        &index,
        &reference("source_read"),
        &[],
        SearchContext {
            visits: &Default::default(),
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "relation-diagnostic",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &[],
        },
    )
    .unwrap();
    // The direct relation-use summary omits this implicit use, but the full
    // question-local type definition still supplies the materialization.
    assert!(catalogs["matrix"].entities["Source"]
        .relations
        .contains_key("parts"));
    assert!(issued.body().contains("Contained calibrated specimens."));
    assert!(issued.body().contains("from_parent_get"));
    assert!(plasm_core::catalog_discovery::capability_relation_uses(
        &catalogs["matrix"],
        &catalogs["matrix"].capabilities["source_read"]
    )
    .unwrap()
    .is_empty());
}

#[tokio::test]
async fn completed_judgments_advance_to_an_affordable_deferred_target() {
    let mut catalogs = fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let template = cgs.capabilities["opaque"].clone();
    cgs.capabilities.clear();
    for name in ["a_candidate", "b_candidate", "c_target"] {
        let mut cap = template.clone();
        cap.name = name.into();
        cap.description = format!(
            "Inspect calibration.{}",
            " Authored specimen detail.".repeat(400)
        );
        cgs.capabilities.insert(name.into(), cap);
    }
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent = IntentProvenance::from_turns(["Inspect calibration".into()]).unwrap();
    let context = SearchContext {
        visits: &Default::default(),
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "crowding-diagnostic",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
    };
    let (target, _) = packet(&index, &reference("c_target"), &[], context).unwrap();
    assert!(target.body().len() <= search_budget::MAX_INPUT_BYTES);
    let mut visits = BTreeMap::new();
    let mut targets = BTreeSet::new();
    for turn in 0..3 {
        let refinement = IntentProvenance::from_turns([
            "Inspect calibration".into(),
            format!("Inspect calibration attempt {turn}"),
        ])
        .unwrap();
        let (retrieval, matching, receipt) = select(
            SearchContext {
                visits: &visits,
                intent: &refinement,
                ..context
            },
            |request| async move { Ok(answer(&request, "unrelated")) },
        )
        .await
        .unwrap();
        for candidate in retrieval
            .candidates
            .iter()
            .filter(|c| matching.matches.iter().any(|m| m.capability_id == c.id))
        {
            assert!(
                targets.insert(candidate.reference.clone()),
                "a rejected root must not repeat while matching unvisited roots fit"
            );
            *visits.entry(candidate.reference.clone()).or_default() += 1;
        }
        assert!(receipt.input_bytes <= search_budget::MAX_INPUT_BYTES);
    }
    assert!(targets.contains(&reference("c_target")));
}

#[test]
fn oversized_ancestry_is_not_silently_discarded() {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let original = "Inspect calibration telemetry".to_owned();
    let initial = IntentProvenance::from_turns([original.clone()]).unwrap();
    let accumulated =
        IntentProvenance::from_turns(std::iter::once(original).chain((0..32).map(|n| {
            format!(
                "Refinement {n}: {}",
                "Inspect calibration telemetry without changing unrelated resources. ".repeat(12)
            )
        })))
        .unwrap();
    let size = |intent: &IntentProvenance| {
        let local = packet(
            &index,
            &reference("source_read"),
            &[],
            SearchContext {
                visits: &Default::default(),
                model: RELEVANCE_MODEL,
                intent,
                generation: "ancestry-diagnostic",
                catalogs: &catalogs,
                deployments: &deployments,
                allowed: &allowed,
                exposed: &[],
            },
        )
        .unwrap()
        .0;
        request_packing::connected(vec![local])
            .unwrap()
            .unwrap()
            .body()
            .len()
    };
    assert!(size(&initial) <= search_budget::MAX_INPUT_BYTES);
    assert!(size(&accumulated) > search_budget::MAX_INPUT_BYTES);
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
                    visits: &Default::default(),
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
    let order = index.ranking.rank(intent.current());
    assert!(!order.iter().take(2).any(|key| exposed.contains(key)));
    let plan = connected::plan(
        &index,
        SearchContext {
            visits: &Default::default(),
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
            visits: &Default::default(),
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
                    visits: &Default::default(),
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
            visits: &Default::default(),
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
            visits: &Default::default(),
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
                visits: &Default::default(),
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
