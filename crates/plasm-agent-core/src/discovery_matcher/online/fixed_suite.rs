//! Fixed offline native-pipeline evaluation. Labels enter only after admission.
use super::*;
use std::{cell::RefCell, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    family: String,
    descriptions: BTreeMap<String, String>,
    context: String,
    turns: Vec<String>,
    labels: BTreeMap<String, String>,
    expected_paths: Vec<Vec<String>>,
    denied: Vec<String>,
    exposed: Vec<String>,
    oversized: Option<String>,
    rationale: String,
}

fn root() -> PathBuf {
    PathBuf::from(crate::test_manifest_dir()).join("../../fixtures/discovery/fixed-v1")
}

fn reference(name: &str) -> CapabilityRef {
    CapabilityRef {
        catalog: "matrix".into(),
        capability: name.into(),
    }
}

fn catalogs(case: &Case, reverse: bool) -> BTreeMap<String, CGS> {
    let mut cgs =
        plasm_core::loader::load_schema_dir(&root().join("../../schemas/discovery_fixed_v1"))
            .unwrap();
    // Explicit nullary acquisition, part of the frozen fixture builder.
    cgs.capabilities
        .get_mut("source_read")
        .unwrap()
        .inputs
        .receiver = Some(plasm_core::CapabilityReceiver::None);
    for (name, description) in &case.descriptions {
        cgs.capabilities.get_mut(name.as_str()).unwrap().description = description.clone();
    }
    if reverse {
        cgs.capabilities = cgs.capabilities.into_iter().rev().collect();
    }
    if !case.context.is_empty() {
        cgs.entities
            .get_mut("Result")
            .unwrap()
            .description
            .push_str(&case.context);
    }
    BTreeMap::from([("matrix".into(), cgs)])
}

#[test]
fn frozen_oversized_controls_exceed_budget_without_competing_candidates() {
    let cases: Vec<Case> =
        serde_json::from_slice(&std::fs::read(root().join("cases.json")).unwrap()).unwrap();
    let mut measurements = Vec::new();
    for case in cases.iter().filter(|case| case.oversized.is_some()) {
        let catalogs = catalogs(case, false);
        let deployments = Default::default();
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
        let intent = IntentProvenance::from_turns(case.turns.clone()).unwrap();
        let target = reference(case.oversized.as_deref().unwrap());
        let description = &catalogs["matrix"].capabilities[target.capability.as_str()].description;
        // The authored string alone is an independent lower bound on any
        // complete-contract JSON packet. No allocator could fit this control.
        assert!(description.len() > 16_000);
        for exposed in [vec![], vec![reference("source_read")]] {
            let context = SearchContext {
                model: RELEVANCE_MODEL,
                intent: &intent,
                generation: "fixed-suite-v1",
                catalogs: &catalogs,
                deployments: &deployments,
                allowed: &allowed,
                exposed: &exposed,
                visits: &BTreeMap::new(),
            };
            let (local, members) = packet(&index, &target, &[], context).unwrap();
            let connected = request_packing::connected(vec![local]).unwrap().unwrap();
            assert!(connected.body().len() > 16_000);
            assert_eq!(members, BTreeSet::from([target.clone()]));
            let body: serde_json::Value = serde_json::from_str(connected.body()).unwrap();
            let mut without_description = body.clone();
            let key = format!("q{}", &index.labels[&target][1..]);
            without_description["state"]["question_evidence"][&key]["capabilities"]
                [&index.labels[&target]]["capability"]["operation"]["contract"] = json!("");
            let description_removed_bytes = serde_json::to_vec(&without_description).unwrap().len();
            assert_eq!(
                connected.body().len() - description_removed_bytes,
                description.len()
            );
            measurements.push(json!({"case":case.id,"exposed":exposed,
                "description_bytes":description.len(),"standalone_packet_bytes":connected.body().len(),
                "other_packet_bytes":description_removed_bytes,"body":body}));
        }
    }
    assert_eq!(measurements.len(), 12);
    if let Ok(path) = std::env::var("PLASM_OVERSIZED_DIAGNOSTIC") {
        std::fs::write(path, serde_json::to_vec_pretty(&measurements).unwrap()).unwrap();
    }
}

fn choice_response(
    request: &IssuedBatch,
    index: &CatalogueSearch,
    case: &Case,
    mode: &str,
) -> String {
    let mut answers = BTreeMap::new();
    for (question_id, candidate_id) in &request.bindings {
        let candidate = index
            .candidates
            .values()
            .find(|c| &c.id == candidate_id)
            .unwrap();
        let choice = match mode {
            "reject" => "unrelated",
            "uncertain" => "uncertain",
            "labels" => case.labels[&candidate.reference.capability].as_str(),
            _ => panic!("unknown fixture judgment mode"),
        };
        answers.insert(
            question_id,
            json!({"type":"choice", "choice":choice,"confidence":1.0,
            "probabilities":{"relevant":if choice=="relevant" {1.0} else {0.0},
            "uncertain":if choice=="uncertain" {1.0} else {0.0},
            "unrelated":if choice=="unrelated" {1.0} else {0.0}}}),
        );
    }
    // Deliberately omit usage: fixture answers did not consume provider tokens.
    // Match the real decoder envelope; the callback and report identify this as synthetic.
    json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","fixture_response":true,"answers":answers})
        .to_string()
}

async fn run_case(case: &Case, reverse: bool, mode: &str) -> serde_json::Value {
    let catalogs = catalogs(case, reverse);
    let deployments = Default::default();
    let mut allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    allowed.capabilities.insert(
        "matrix".into(),
        catalogs["matrix"]
            .capabilities
            .keys()
            .filter(|name| !case.denied.iter().any(|denied| denied == name.as_str()))
            .map(|name| name.as_str().to_owned())
            .collect(),
    );
    let start = std::time::Instant::now();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let canonical = evidence::CatalogEvidence::from_catalogs(
        &RetrievalReceipt {
            generation: "fixed-suite-v1".into(),
            candidates: index.candidates.values().cloned().collect(),
            lexical_count: index.candidates.len(),
            vector_count: 0,
            lexical_truncated: false,
            vector_truncated: false,
        },
        &catalogs,
        &deployments,
        &|key| allowed.permits(key),
    )
    .unwrap();
    let indexing_us = start.elapsed().as_micros();
    let mut exposed: BTreeSet<_> = case.exposed.iter().map(|name| reference(name)).collect();
    let mut visits = BTreeMap::<CapabilityRef, usize>::new();
    let mut turns = Vec::new();
    let mut timings = Vec::new();
    for step in 1..=4 {
        let intent = IntentProvenance::from_turns(case.turns.clone()).unwrap();
        let need = case.turns.last().unwrap();
        let rank = index.ranking.rank(need);
        let previous = exposed.clone();
        let existing: Vec<_> = exposed.iter().cloned().collect();
        let context = SearchContext {
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "fixed-suite-v1",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &existing,
            visits: &visits,
        };
        let frontier = connected::root_order(&index, context);
        let packets = RefCell::new(Vec::<serde_json::Value>::new());
        let start = std::time::Instant::now();
        let selected = select_indexed(context, &index, |request| {
            assert!(request.body().len() <= 16_000);
            let body: serde_json::Value = serde_json::from_str(request.body()).unwrap();
            assert_eq!(request.body(), serde_json::to_string(&body).unwrap());
            for (question_id, local) in body["state"]["question_evidence"].as_object().unwrap() {
                let target_id = &request.bindings[question_id];
                let target = index.candidates.values().find(|c| &c.id==target_id).unwrap();
                let mut found_target = false;
                for card in local["capabilities"].as_object().unwrap().values() {
                    let key: CapabilityRef = serde_json::from_value(card["capability"]["reference"].clone()).unwrap();
                    assert!(allowed.permits(&key));
                    assert_eq!(card["capability"]["operation"]["contract"], catalogs[&key.catalog].capabilities[key.capability.as_str()].description);
                    if key == target.reference {
                        found_target = true;
                        assert_eq!(card["declaration"], serde_json::to_value(&canonical.capabilities[&key].declaration).unwrap());
                    }
                }
                assert!(found_target);
                for id in local["definition_refs"].as_array().unwrap() {
                    assert!(!body["state"]["definitions"][id.as_str().unwrap()].is_null());
                }
                for id in local["binding_refs"].as_array().unwrap() {
                    assert!(!body["state"]["binding_witnesses"][id.as_str().unwrap()].is_null());
                }
            }
            for question in &request.questions {
                assert!(allowed.permits(&question.reference));
                assert_eq!(serde_json::to_value(&question.operation).unwrap(),
                    serde_json::to_value(&canonical.capabilities[&question.reference].operation).unwrap());
            }
            let response = choice_response(&request, &index, case, mode);
            packets.borrow_mut().push(json!({"body":serde_json::from_str::<serde_json::Value>(request.body()).unwrap(),
                "bytes":request.body().len(),"sha256":plasm_core::catalog_discovery::content_hash(request.body().as_bytes())}));
            async move { Ok(response) }
        }, |_| async { Ok(()) }).await;
        timings.push(start.elapsed().as_micros());
        let (retrieval, matches, receipt) = match selected {
            Ok(value) => value,
            Err(error) => {
                return json!({"case":case.id,"family":case.family,"mode":mode,
                "error":error.to_string(),"stage":"native_pipeline_fault","turns":turns})
            }
        };
        assert!(receipt.input_bytes <= 16_000 && receipt.requests <= 1);
        assert_eq!(receipt.requests, packets.borrow().len());
        let mut decisions = BTreeMap::new();
        for candidate in &retrieval.candidates {
            let answer = matches
                .matches
                .iter()
                .find(|m| m.capability_id == candidate.id)
                .unwrap();
            assert!(
                !previous.contains(&candidate.reference),
                "already exposed capability rejudged"
            );
            *visits.entry(candidate.reference.clone()).or_default() += 1;
            decisions.insert(candidate.reference.clone(), answer.choice.clone());
            if answer.choice != MatchChoice::Unrelated {
                exposed.insert(candidate.reference.clone());
            }
        }
        for edge in required(&exposed, &catalogs, &deployments, &allowed).unwrap() {
            exposed.insert(edge.source);
        }
        assert!(previous.is_subset(&exposed));
        assert!(exposed.iter().all(|r| allowed.permits(r)));
        let status: Vec<_> = case
            .labels
            .iter()
            .map(|(name, label)| {
                let key = reference(name);
                let deferred = receipt.deferred.iter().find(|d| d.reference == key);
                let stage = if !allowed.permits(&key) {
                    "unauthorized"
                } else if !index.candidates.contains_key(&key) {
                    "not_indexed"
                } else if exposed.contains(&key) {
                    "retained"
                } else if decisions.get(&key) == Some(&MatchChoice::Unrelated) {
                    "judgment_rejected"
                } else if deferred.is_some() {
                    "allocation_deferred"
                } else if !frontier.contains(&key) {
                    "ranking_unretrieved"
                } else {
                    "allocation_unvisited"
                };
                json!({"capability":key,"expected_contribution":label,"stage":stage,
                "rank":rank.iter().position(|r| r==&key).map(|n|n+1),
                "frontier_rank":frontier.iter().position(|r|r==&key).map(|n|n+1),
                "deferred":deferred,"judgment":decisions.get(&key)})
            })
            .collect();
        let paths: Vec<_> = case
            .expected_paths
            .iter()
            .map(|path| {
                json!({"members":path,
            "complete":path.iter().all(|name|exposed.contains(&reference(name)))})
            })
            .collect();
        turns.push(
            json!({"search":step,"status":status,"paths":paths,"receipt":receipt,
            "exposed":exposed,"packets":packets.into_inner()}),
        );
    }
    json!({"case":case.id,"family":case.family,"mode":mode,"turns":turns,
        "timing":{"indexing_us":indexing_us,"selection_us":timings},
        "oversized":case.oversized,"label_rationale":case.rationale})
}

#[tokio::test]
#[ignore = "fixed offline discovery benchmark; run through fixtures/discovery/fixed-v1/run.py"]
async fn fixed_native_discovery_suite_v1() {
    let cases: Vec<Case> =
        serde_json::from_slice(&std::fs::read(root().join("cases.json")).unwrap()).unwrap();
    assert_eq!(cases.len(), 48);
    let destination = PathBuf::from(
        std::env::var("PLASM_FIXED_DISCOVERY_REPORT").expect("report output path required"),
    );
    assert!(!destination.exists(), "never overwrite prior results");
    let mut results = Vec::new();
    for case in &cases {
        let a = run_case(case, false, "labels").await;
        let b = run_case(case, true, "labels").await;
        assert_eq!(a["turns"], b["turns"], "{} insertion permutation", case.id);
        println!(
            "fixed-suite case={} fault={}",
            case.id,
            a.get("error").unwrap_or(&serde_json::Value::Null)
        );
        results.push(a);
    }
    let control = cases.iter().find(|c| c.family == "composition").unwrap();
    let reject = run_case(control, false, "reject").await;
    let uncertain = run_case(control, false, "uncertain").await;
    std::fs::write(
        &destination,
        serde_json::to_vec_pretty(&json!({"version":1,"policy":SEARCH_POLICY,
        "cases":results,"sensitivity":{"reject":reject,"uncertain":uncertain},
        "provider_calls":0,"provider_usage":null}))
        .unwrap(),
    )
    .unwrap();
    assert!(reject["turns"][0]["status"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["stage"] == "judgment_rejected"));
    assert!(uncertain["turns"][0]["status"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["judgment"] == "uncertain" && s["stage"] == "retained"));
}
