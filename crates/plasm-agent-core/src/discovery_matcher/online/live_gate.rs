//! Opt-in native discovery gate. Fixture labels never enter live selection.
use super::*;
use std::{cell::RefCell, io::Write, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Phase {
    request: String,
    labels: BTreeMap<String, String>,
    paths: Vec<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bridge {
    consumer: String,
    requirement: String,
    provider_catalog: String,
    provider: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    catalogs: Vec<String>,
    phases: Vec<Phase>,
    binding: Option<Bridge>,
}
fn key(text: &str) -> CapabilityRef {
    let (catalog, capability) = text.split_once('/').unwrap();
    CapabilityRef {
        catalog: catalog.into(),
        capability: capability.into(),
    }
}
fn name(key: &CapabilityRef) -> String {
    format!("{}/{}", key.catalog, key.capability)
}
fn emit(path: &std::path::Path, value: serde_json::Value) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{value}").unwrap();
}

#[tokio::test]
#[ignore = "explicit frozen live-v2 runner only; oracle mode makes no provider calls"]
async fn native_live_discovery_gate_v2() {
    let root = PathBuf::from(crate::test_manifest_dir()).join("../../fixtures");
    let cases: Vec<Case> = serde_json::from_slice(
        &std::fs::read(root.join("discovery/live-v2.2/cases.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(cases.len(), 8);
    let mode = std::env::var("PLASM_LIVE_GATE_MODE").unwrap();
    assert!(matches!(mode.as_str(), "oracle" | "live"));
    let output = PathBuf::from(std::env::var("PLASM_LIVE_GATE_TRACE").unwrap());
    assert!(!output.exists(), "never overwrite a gate trace");
    let credential = if mode == "live" {
        std::env::var("OPENROUTER_API_KEY").unwrap()
    } else {
        String::new()
    };
    let client = reqwest::Client::new();
    let mut logical = 0usize;
    for repeat in 0..2 {
        let order: Vec<_> = if repeat == 0 {
            cases.iter().collect()
        } else {
            cases.iter().rev().collect()
        };
        for case in order {
            let mut catalogs = BTreeMap::new();
            for catalog in &case.catalogs {
                let mut cgs = plasm_core::loader::load_schema_dir(
                    &root
                        .join("schemas/discovery_live_v2_2")
                        .join(&case.id)
                        .join(catalog),
                )
                .unwrap();
                cgs.bind_registry_entry_id(catalog);
                // Authored observation capabilities are explicit nullary reads.
                for cap in cgs.capabilities.values_mut() {
                    if cap.inputs.arguments.is_none() {
                        cap.inputs.receiver = Some(plasm_core::CapabilityReceiver::None);
                    }
                }
                catalogs.insert(catalog.clone(), cgs);
            }
            let mut deployments = DeploymentBindings::default();
            if let Some(b) = &case.binding {
                deployments
                    .bindings
                    .push(plasm_core::prerequisites::DeploymentBinding {
                        consumer: key(&b.consumer),
                        requirement: b.requirement.clone(),
                        provider_catalog: b.provider_catalog.clone(),
                        provider: b.provider.clone(),
                    });
            }
            let allowed = DiscoveryAuthorization::catalogs(catalogs.keys().cloned().collect());
            let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
            let mut exposed = BTreeSet::new();
            let mut visits = BTreeMap::new();
            for step in 0..4 {
                let phase_index = (step / 2).min(case.phases.len() - 1);
                let phase = &case.phases[phase_index];
                let intent = IntentProvenance::from_turns(
                    case.phases[..=phase_index]
                        .iter()
                        .map(|p| p.request.clone()),
                )
                .unwrap();
                let existing: Vec<_> = exposed.iter().cloned().collect();
                let context = SearchContext {
                    model: RELEVANCE_MODEL,
                    intent: &intent,
                    generation: "live-v2",
                    catalogs: &catalogs,
                    deployments: &deployments,
                    allowed: &allowed,
                    exposed: &existing,
                    visits: &visits,
                };
                // Label-based cost checks are diagnostic only, never allocator input.
                if mode == "oracle" {
                    for (n, label) in &phase.labels {
                        if label == "unrelated" {
                            continue;
                        }
                        let (local, _) = packet(&index, &key(n), &[], context).unwrap();
                        let body = request_packing::connected(vec![local]).unwrap().unwrap();
                        assert!(
                            body.body().len() <= 16_000,
                            "positive cannot fit alone: {} {}",
                            case.id,
                            n
                        );
                    }
                }
                let before = exposed.clone();
                let calls = RefCell::new(0usize);
                let started = std::time::Instant::now();
                let result=select_indexed(context,&index,|request| {
                    *calls.borrow_mut()+=1;
                    assert!(*calls.borrow()<=3);
                    assert!(request.body().len()<=16_000);
                    let body:serde_json::Value=serde_json::from_str(request.body()).unwrap();
                    assert_eq!(request.body(),serde_json::to_string(&body).unwrap());
                    for (qid,local) in body["state"]["question_evidence"].as_object().unwrap() {
                        let target=&index.candidates.values().find(|c|c.id==request.bindings[qid]).unwrap().reference;
                        let mut found=false;
                        for card in local["capabilities"].as_object().unwrap().values() {
                            let r:CapabilityRef=serde_json::from_value(card["capability"]["reference"].clone()).unwrap();
                            assert!(allowed.permits(&r));
                            assert_eq!(card["capability"]["operation"]["contract"],catalogs[&r.catalog].capabilities[r.capability.as_str()].description);
                            if &r==target {
                                let (_,canonical)=index.evidence("live-v2",&BTreeSet::from([r.clone()]),&catalogs,&deployments,&allowed).unwrap();
                                assert_eq!(card["declaration"],serde_json::to_value(&canonical.capabilities[&r].declaration).unwrap());
                                found=true;
                            }
                        }
                        assert!(found);
                    }
                    let bindings:BTreeMap<_,_>=request.bindings.iter().map(|(q,id)| {
                        let c=index.candidates.values().find(|c| &c.id==id).unwrap();
                        (q.clone(),name(&c.reference))
                    }).collect();
                    emit(&output,json!({"kind":"request","case":case.id,"repeat":repeat,"step":step+1,
                        "attempt":*calls.borrow(),"bindings":bindings,"body":body,"bytes":request.body().len(),
                        "sha256":content_hash(request.body().as_bytes())}));
                    let mut answers=BTreeMap::new();
                    if mode=="oracle" {for (q,n) in &bindings {
                        let label=&phase.labels[n];
                        answers.insert(q,json!({"type":"choice","choice":label,"confidence":1.0,
                            "probabilities":{"relevant":if label=="relevant" {1.0}else{0.0},
                            "uncertain":if label=="uncertain" {1.0}else{0.0},"unrelated":if label=="unrelated" {1.0}else{0.0}}}));
                    }}
                    let oracle=json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","answers":answers}).to_string();
                    let client=&client;let credential=&credential;let output=&output;let mode=&mode;
                    async move {
                        if mode=="oracle" {return Ok(oracle);}
                        let begin=std::time::Instant::now();
                        let result=plasm_runtime::provider_transport::request_provider(&plasm_runtime::provider_transport::ReqwestProviderTransport::new(client.clone(), RELEVANCE_ENDPOINT, credential.clone()),request.body(),
                            plasm_runtime::provider_transport::ProviderRetryPolicy::discovery_attempt(),
                            |attempt| {emit(output,json!({"kind":"transport","case":case.id,"repeat":repeat,"step":step+1,
                                "call_seconds":begin.elapsed().as_secs_f64(),"record":attempt}));Ok(())}).await;
                        result.map_err(|e|Box::new(e) as DecisionProviderError)
                    }
                }, |_|async {Ok(())}).await;
                let (retrieval, matches, receipt) = match result {
                    Ok(v) => v,
                    Err(e) => {
                        emit(
                            &output,
                            json!({"kind":"fault","case":case.id,"repeat":repeat,"step":step+1,"error":e.to_string()}),
                        );
                        panic!("unrecovered discovery fault");
                    }
                };
                if *calls.borrow() > 0 {
                    logical += 1;
                }
                assert!(logical <= 64 && receipt.requests <= 3 && receipt.input_bytes <= 48_000);
                let mut decisions = BTreeMap::new();
                for candidate in &retrieval.candidates {
                    assert!(!before.contains(&candidate.reference));
                    let answer = matches
                        .matches
                        .iter()
                        .find(|m| m.capability_id == candidate.id)
                        .unwrap();
                    decisions.insert(name(&candidate.reference), answer.choice.clone());
                    *visits.entry(candidate.reference.clone()).or_default() += 1;
                    if answer.choice != MatchChoice::Unrelated {
                        exposed.insert(candidate.reference.clone());
                    }
                }
                for edge in required(&exposed, &catalogs, &deployments, &allowed).unwrap() {
                    exposed.insert(edge.source);
                }
                assert!(before.is_subset(&exposed));
                assert!(exposed.iter().all(|r| allowed.permits(r)));
                let paths:Vec<_>=phase.paths.iter().map(|p|json!({"members":p,"complete":p.iter().all(|n|exposed.contains(&key(n)))})).collect();
                emit(
                    &output,
                    json!({"kind":"turn","case":case.id,"repeat":repeat,"step":step+1,"phase":phase_index,
                    "decisions":decisions,"exposed":exposed,"paths":paths,"receipt":receipt,"seconds":started.elapsed().as_secs_f64()}),
                );
            }
        }
    }
    emit(
        &output,
        json!({"kind":"complete","mode":mode,"logical_requests":logical}),
    );
}
