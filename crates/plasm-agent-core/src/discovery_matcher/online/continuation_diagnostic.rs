//! Development diagnostic: real request keys, recall admission and packet renderer.
//! Frozen recall and fixture judgments isolate mechanics; no semantic retrieval claim.
use super::*;

#[tokio::test]
async fn frozen_continuation_diagnostic() {
    let requests = [
        "Inspect specimens without changing them.",
        "Observe specimens while preserving their state.",
        "Read specimen information without modifications.",
        "Obtain specimen observations; leave specimens unchanged.",
        "Show observations of the specimens without making changes.",
        "Find read-only information about the specimens.",
    ];
    let mut rows = Vec::new();
    for oversized in [false, true] {
        for changed in [false, true] {
            for rotating in [false, true] {
                for shared in [false, true] {
                    let mut catalogs = tests::fixture();
                    let cgs = catalogs.get_mut("matrix").unwrap();
                    let prototype = cgs.capabilities["source_read"].clone();
                    cgs.capabilities.clear();
                    for name in ["a", "b", "c", "d"] {
                        let mut cap = prototype.clone();
                        cap.name = name.into();
                        cap.description = "Read the authored specimen observations. "
                            .repeat(if oversized && name == "d" { 1000 } else { 120 });
                        cgs.capabilities.insert(name.into(), cap);
                    }
                    let allowed =
                        DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
                    let deployments = DeploymentBindings::default();
                    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
                    let input = recall(&index, &["a", "b", "c", "d"]);
                    let mut frontiers = BTreeMap::<String, BTreeMap<CapabilityRef, usize>>::new();
                    let mut seen = BTreeSet::new();
                    let mut turns = Vec::new();
                    let mut history = Vec::new();
                    for turn in 0..6 {
                        let request = if changed && turn >= 3 {
                            "Inspect the newly available specimen observations."
                        } else {
                            requests[if rotating { turn } else { 0 }]
                        };
                        history.push(request.into());
                        let intent = IntentProvenance::from_turns(history.clone()).unwrap();
                        let key = if shared {
                            "experimental-session".into()
                        } else {
                            crate::discovery_service::diagnostic_frontier_key(request, &allowed)
                        };
                        let visits = frontiers.entry(key).or_default();
                        // The changed-intent control makes a formerly negative candidate useful.
                        // Labels affect only the fixture response, never recall or allocation.
                        let target = if changed && turn >= 3 { "a" } else { "d" };
                        let (retrieval, matching, receipt) = select_recalled_indexed(
                            SearchContext { model: RELEVANCE_MODEL, intent: &intent,
                                generation: "frozen", catalogs: &catalogs, deployments: &deployments,
                                allowed: &allowed, exposed: &[], visits },
                            &index, &input,
                            |batch| {
                                let mut answer: serde_json::Value = serde_json::from_str(&tests::answer(&batch, "unrelated")).unwrap();
                                for question in &batch.questions {
                                    if question.reference.capability == target {
                                        let id = batch.bindings.iter().find(|(_, id)| **id == index.candidates[&question.reference].id).unwrap().0;
                                        answer["answers"][id] = json!({"type":"choice","choice":"relevant","confidence":1.0,
                                            "probabilities":{"relevant":1.0,"uncertain":0.0,"unrelated":0.0}});
                                    }
                                }
                                async move { Ok(answer.to_string()) }
                            }, |_| async { Ok(()) },
                        ).await.unwrap();
                        assert!(receipt.requests <= 1 && receipt.input_bytes <= 16000);
                        let mut repeated = 0;
                        let mut admitted = Vec::new();
                        for c in &retrieval.candidates {
                            repeated += usize::from(!seen.insert(c.reference.clone()));
                            *visits.entry(c.reference.clone()).or_default() += 1;
                            admitted.push(c.reference.capability.clone());
                        }
                        let target_relevant = retrieval.candidates.iter().any(|c| {
                            c.reference.capability == target
                                && matching.matches.iter().any(|m| {
                                    m.capability_id == c.id && m.choice == MatchChoice::Relevant
                                })
                        });
                        turns.push(json!({"turn":turn+1,"target":target,"target_relevant":target_relevant,
                            "admitted":admitted,"repeated":repeated,"unique":seen.len(),"bytes":receipt.input_bytes,
                            "deferred":receipt.deferred}));
                    }
                    rows.push(
                        json!({"oversized":oversized,"changed":changed,"rotating":rotating,
                        "shared_exploration":shared,"turns":turns}),
                    );
                }
            }
        }
    }
    println!(
        "CONTINUATION_DIAGNOSTIC {}",
        serde_json::to_string(&rows).unwrap()
    );
}
