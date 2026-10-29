//! Frozen starvation benchmark, native selection with fixture-only judgments.
use super::*;
use std::{cell::RefCell, io::Write, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Outcome {
    id: String,
    alternatives: Vec<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    topology: String,
    profile: String,
    count: usize,
    request: String,
    labels: BTreeMap<String, String>,
    outcomes: Vec<Outcome>,
    noise_label: String,
    provider: String,
}
fn root() -> PathBuf {
    PathBuf::from(crate::test_manifest_dir()).join("../../fixtures/discovery/starvation-v2.1")
}
fn key(text: &str) -> CapabilityRef {
    let (catalog, capability) = text.split_once('/').unwrap();
    CapabilityRef {
        catalog: catalog.into(),
        capability: capability.into(),
    }
}
fn name(r: &CapabilityRef) -> String {
    format!("{}/{}", r.catalog, r.capability)
}
fn checkpoint(value: serde_json::Value) {
    let path = std::env::var("PLASM_STARVATION_TRACE").unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{value}").unwrap();
}
fn load(schema: &str) -> CGS {
    plasm_core::loader::load_schema_dir(
        &root()
            .join("../../schemas/discovery_starvation_v2_1")
            .join(schema),
    )
    .unwrap()
}

fn fixture(case: &Case, reverse: bool) -> (BTreeMap<String, CGS>, DeploymentBindings) {
    let mut catalogs = BTreeMap::new();
    let mut deployments = DeploymentBindings::default();
    if case.topology == "local_alternatives" {
        let mut cgs = load("local");
        cgs.bind_registry_entry_id("matrix");
        catalogs.insert("matrix".into(), cgs);
    } else {
        let mut materials = load("cross");
        materials.bind_registry_entry_id("materials");
        materials
            .capabilities
            .retain(|n, _| ["source_a", "source_b"].contains(&n.as_str()));
        materials.prerequisites.requirements.clear();
        let mut engine = load("cross");
        engine.bind_registry_entry_id("engine");
        engine
            .capabilities
            .retain(|n, _| !["source_a", "source_b"].contains(&n.as_str()));
        engine.prerequisites.providers.clear();
        engine.entities.get_mut("Source").unwrap().primary_read = None;
        catalogs.insert("materials".into(), materials);
        catalogs.insert("engine".into(), engine);
        deployments
            .bindings
            .push(plasm_core::prerequisites::DeploymentBinding {
                consumer: key("engine/prepare"),
                requirement: "feed".into(),
                provider_catalog: "materials".into(),
                provider: case.provider.clone(),
            });
    }
    for cgs in catalogs.values_mut() {
        for (id, cap) in &mut cgs.capabilities {
            if ["source_a", "source_b", "prepared_read"].contains(&id.as_str()) {
                cap.inputs.receiver = Some(plasm_core::CapabilityReceiver::None);
            }
        }
    }
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root().join("noise.json")).unwrap()).unwrap();
    let mut noise = load("noise");
    noise.bind_registry_entry_id("noise");
    let template = noise.capabilities["archive"].clone();
    noise.capabilities.clear();
    for i in 0..case.count {
        let mut cap = template.clone();
        cap.name = format!("distractor_{i:05}").into();
        cap.description = profile[&case.profile]
            .as_str()
            .unwrap()
            .repeat(profile["repeat"].as_u64().unwrap() as usize);
        noise.capabilities.insert(cap.name.clone(), cap);
    }
    if case.count > 0 {
        catalogs.insert("noise".into(), noise);
    }
    if reverse {
        for cgs in catalogs.values_mut() {
            cgs.capabilities = std::mem::take(&mut cgs.capabilities)
                .into_iter()
                .rev()
                .collect();
        }
    }
    (catalogs, deployments)
}

async fn run(case: &Case, reverse: bool) -> serde_json::Value {
    checkpoint(json!({"case":case.id,"reverse":reverse,"phase":"fixture"}));
    let (catalogs, deployments) = fixture(case, reverse);
    let allowed = DiscoveryAuthorization::catalogs(catalogs.keys().cloned().collect());
    checkpoint(json!({"case":case.id,"reverse":reverse,"phase":"index"}));
    let started = std::time::Instant::now();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let index_us = started.elapsed().as_micros();
    let edges: Vec<_> = index.graph.edges().cloned().collect();
    assert!(
        edges
            .iter()
            .all(|e| e.source.catalog != "noise" && e.consumer.catalog != "noise"),
        "same-name noise invented a binding"
    );
    if case.topology == "local_alternatives" {
        for supplier in ["matrix/source_a", "matrix/source_b"] {
            assert!(edges.iter().any(|e| e.source == key(supplier)
                && e.consumer == key("matrix/prepare")
                && e.relationship == Relationship::SuppliesInput));
        }
    }
    let all_targets: BTreeSet<_> = case.labels.keys().map(|s| key(s)).collect();
    let closure = required_closure(&all_targets, &catalogs, &deployments, &allowed).unwrap();
    if case.topology != "local_alternatives" {
        let supplier = if case.provider == "primary" {
            "materials/source_a"
        } else {
            "materials/source_b"
        };
        assert!(closure
            .edges
            .iter()
            .any(|e| e.consumer == key("engine/prepare") && e.provider == key(supplier)));
    }
    checkpoint(
        json!({"case":case.id,"reverse":reverse,"phase":"indexed","index_us":index_us,"edges":edges,"required":closure.edges}),
    );
    let intent = IntentProvenance::from_turns([case.request.clone()]).unwrap();
    let mut exposed = BTreeSet::new();
    let mut visits = BTreeMap::new();
    let mut turns = Vec::new();
    let mut timings = Vec::new();
    for step in 1..=4 {
        let rank = index.ranking.rank(&case.request);
        let existing: Vec<_> = exposed.iter().cloned().collect();
        let context = SearchContext {
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "starvation-v2",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &existing,
            visits: &visits,
        };
        let frontier = connected::root_order(&index, context);
        checkpoint(
            json!({"case":case.id,"reverse":reverse,"search":step,"phase":"selection",
            "target_ranks":all_targets.iter().map(|r|json!({"reference":r,"rank":rank.iter().position(|x|x==r).map(|n|n+1),
                "frontier_rank":frontier.iter().position(|x|x==r).map(|n|n+1)})).collect::<Vec<_>>()}),
        );
        let packets = RefCell::new(Vec::new());
        let now = std::time::Instant::now();
        let result=select_indexed(context,&index,|request| {
            assert!(request.body().len()<=16_000);
            let body:serde_json::Value=serde_json::from_str(request.body()).unwrap();
            assert_eq!(request.body(), serde_json::to_string(&body).unwrap());
            let mut answers=BTreeMap::new();
            for (qid,id) in &request.bindings {
                let candidate=index.candidates.values().find(|c|&c.id==id).unwrap();
                let choice=if candidate.reference.catalog=="noise" {case.noise_label.as_str()} else {case.labels[&name(&candidate.reference)].as_str()};
                let target=&candidate.reference;
                let (_,canonical)=index.evidence("starvation-v2",&BTreeSet::from([target.clone()]),&catalogs,&deployments,&allowed).unwrap();
                let local=&body["state"]["question_evidence"][qid];
                let card=local["capabilities"].as_object().unwrap().values().find(|card|card["capability"]["reference"]==serde_json::to_value(target).unwrap()).unwrap();
                assert_eq!(card["declaration"],serde_json::to_value(&canonical.capabilities[target].declaration).unwrap());
                assert_eq!(card["capability"]["operation"]["contract"],catalogs[&target.catalog].capabilities[target.capability.as_str()].description);
                for card in local["capabilities"].as_object().unwrap().values() {
                    let r:CapabilityRef=serde_json::from_value(card["capability"]["reference"].clone()).unwrap(); assert!(allowed.permits(&r));
                }
                for id in local["definition_refs"].as_array().unwrap() {assert!(!body["state"]["definitions"][id.as_str().unwrap()].is_null());}
                for id in local["binding_refs"].as_array().unwrap() {assert!(!body["state"]["binding_witnesses"][id.as_str().unwrap()].is_null());}
                answers.insert(qid.clone(),json!({"type":"choice","choice":choice,"confidence":1.0,
                    "probabilities":{"relevant":if choice=="relevant" {1.0}else{0.0},"uncertain":0.0,
                    "unrelated":if choice=="unrelated" {1.0}else{0.0}}}));
            }
            packets.borrow_mut().push(body);
            let response=json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","fixture_response":true,"answers":answers}).to_string();
            async move {Ok(response)}
        }, |_|async {Ok(())}).await;
        timings.push(now.elapsed().as_micros());
        let (retrieval, matches, receipt) = match result {
            Ok(v) => v,
            Err(e) => return json!({"case":case.id,"error":e.to_string(),"turns":turns}),
        };
        assert!(receipt.requests <= 1 && receipt.input_bytes <= 16_000);
        assert_eq!(receipt.requests, packets.borrow().len());
        // Independent direct-cost diagnostic, never passed to the allocator.
        let direct: BTreeMap<_, _> = all_targets
            .iter()
            .map(|r| {
                (
                    name(r),
                    match packet(&index, r, &[], context) {
                        Ok((p, _)) => json!({"bytes":p.body().len()}),
                        Err(e) => json!({"error":e.to_string()}),
                    },
                )
            })
            .collect();
        let before = exposed.clone();
        let mut decisions = BTreeMap::new();
        for candidate in &retrieval.candidates {
            assert!(!before.contains(&candidate.reference));
            let answer = matches
                .matches
                .iter()
                .find(|m| m.capability_id == candidate.id)
                .unwrap();
            *visits.entry(candidate.reference.clone()).or_insert(0usize) += 1;
            decisions.insert(candidate.reference.clone(), answer.choice.clone());
            if answer.choice != MatchChoice::Unrelated {
                exposed.insert(candidate.reference.clone());
            }
        }
        for edge in required(&exposed, &catalogs, &deployments, &allowed).unwrap() {
            exposed.insert(edge.source);
        }
        assert!(before.is_subset(&exposed));
        assert!(exposed.iter().all(|r| r.catalog != "noise"));
        let statuses:Vec<_>=all_targets.iter().map(|r| {
            let deferred=receipt.deferred.iter().find(|d|&d.reference==r);
            let stage=if exposed.contains(r){"retained"}else if decisions.get(r)==Some(&MatchChoice::Unrelated){"judgment_rejected"}
                else if deferred.is_some(){"allocation_deferred"}else if !frontier.contains(r) && !edges.iter().any(|e|&e.source==r){"ranking_unretrieved"}
                else {"allocation_unvisited"};
            json!({"reference":r,"rank":rank.iter().position(|x|x==r).map(|n|n+1),
                "frontier_rank":frontier.iter().position(|x|x==r).map(|n|n+1),"stage":stage,"deferred":deferred,"direct":direct[&name(r)]})
        }).collect();
        let outcomes:Vec<_>=case.outcomes.iter().map(|outcome|json!({"id":outcome.id,
            "complete":outcome.alternatives.iter().any(|path|path.iter().all(|s|exposed.contains(&key(s)))),
            "alternatives":outcome.alternatives.iter().map(|path|json!({"members":path,"complete":path.iter().all(|s|exposed.contains(&key(s)))})).collect::<Vec<_>>()})).collect();
        let turn = json!({"search":step,"statuses":statuses,"outcomes":outcomes,"receipt":receipt,"exposed":exposed,"packets":packets.into_inner()});
        checkpoint(
            json!({"case":case.id,"reverse":reverse,"phase":"completed_search","turn":turn}),
        );
        turns.push(turn);
    }
    json!({"case":case.id,"topology":case.topology,"profile":case.profile,"count":case.count,"turns":turns,
        "edges":edges,"required":closure.edges,"timings":{"index_us":index_us,"selection_us":timings}})
}

#[tokio::test]
#[ignore = "frozen native starvation benchmark; use starvation-v2/run.py"]
async fn fixed_native_starvation_suite_v2() {
    let cases: Vec<Case> =
        serde_json::from_slice(&std::fs::read(root().join("cases.json")).unwrap()).unwrap();
    let id = std::env::var("PLASM_STARVATION_CASE").unwrap();
    let case = cases.iter().find(|c| c.id == id).unwrap();
    let a = run(case, false).await;
    let b = run(case, true).await;
    let output = PathBuf::from(std::env::var("PLASM_STARVATION_REPORT").unwrap());
    assert!(!output.exists());
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&json!({"forward":a,"reverse":b,"provider_calls":0})).unwrap(),
    )
    .unwrap();
    assert_eq!(
        a["turns"], b["turns"],
        "permutation changed native receipts"
    );
    assert!(
        a.get("error").is_none() && b.get("error").is_none(),
        "native pipeline fault: {a}"
    );
}

#[test]
fn retained_producers_nominate_missing_consumers_before_unconnected_roots() {
    let cases: Vec<Case> =
        serde_json::from_slice(&std::fs::read(root().join("cases.json")).unwrap()).unwrap();
    let case = cases
        .iter()
        .find(|c| c.id == "local_alternatives/near_topic/100")
        .unwrap();
    let (catalogs, deployments) = fixture(case, false);
    let allowed = DiscoveryAuthorization::catalogs(catalogs.keys().cloned().collect());
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent = IntentProvenance::from_turns([case.request.clone()]).unwrap();
    for (producer, consumer) in [("prepare", "prepared_read"), ("prepared_read", "process")] {
        let exposed = [key(&format!("matrix/{producer}"))];
        let visits = BTreeMap::from([(exposed[0].clone(), 1)]);
        let plan = connected::plan(
            &index,
            SearchContext {
                model: RELEVANCE_MODEL,
                intent: &intent,
                generation: "producer-frontier",
                catalogs: &catalogs,
                deployments: &deployments,
                allowed: &allowed,
                exposed: &exposed,
                visits: &visits,
            },
        )
        .unwrap();
        assert!(
            plan.wave
                .iter()
                .any(|(r, _)| r == &key(&format!("matrix/{consumer}"))),
            "retained {producer} did not nominate {consumer}"
        );
        assert!(plan.issued.unwrap().body().len() <= search_budget::MAX_INPUT_BYTES);
    }
}
