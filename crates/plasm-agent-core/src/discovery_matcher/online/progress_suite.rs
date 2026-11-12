//! Frozen development measurements through native selection and teaching.
use super::*;
use plasm_core::{
    capability_exposure::selected_capability_surface,
    prompt_render::python::{prepare_python_teaching_wave, PythonTeachingState},
    symbol_tuning::TeachingExposureSession,
};
use std::{cell::RefCell, path::PathBuf, sync::Arc};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    version: usize,
    searches: usize,
    distractors: usize,
    description_clause: String,
    cases: Vec<Case>,
    exposure_schedules: Vec<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    repeat: usize,
    needs: Vec<String>,
    labels: BTreeMap<String, String>,
    expected: Vec<String>,
    denied: Vec<String>,
    reject: bool,
    positive: bool,
}
fn root() -> PathBuf {
    PathBuf::from(crate::test_manifest_dir()).join("../../fixtures/discovery/progress-v3")
}

/// Diagnostic exporter only: caller-specified targets are not retrieval evidence.
/// Never used as a scored fixture or a provider request.
#[test]
#[ignore = "offline standalone-byte diagnostic; explicit input/output paths required"]
fn export_standalone_packet_diagnostic() {
    #[derive(Deserialize)]
    struct Input {
        catalogs: BTreeMap<String, PathBuf>,
        intent: IntentProvenance,
        allowed: DiscoveryAuthorization,
        exposed: Vec<CapabilityRef>,
        targets: Vec<CapabilityRef>,
        #[serde(default)]
        deployments: DeploymentBindings,
    }
    let input: Input = serde_json::from_slice(
        &std::fs::read(std::env::var("PLASM_STANDALONE_INPUT").unwrap()).unwrap(),
    )
    .unwrap();
    let catalogs: BTreeMap<String, CGS> = input
        .catalogs
        .iter()
        .map(|(name, path)| {
            (
                name.clone(),
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap(),
            )
        })
        .collect();
    let index = CatalogueSearch::new(&catalogs, &input.deployments, &input.allowed).unwrap();
    let visits = BTreeMap::new();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &input.intent,
        generation: "standalone-diagnostic",
        catalogs: &catalogs,
        deployments: &input.deployments,
        allowed: &input.allowed,
        exposed: &input.exposed,
        visits: &visits,
    };
    let mut packets = Vec::new();
    for target in &input.targets {
        let (local, members) = packet(&index, target, &[], context).unwrap();
        let connected = request_packing::connected(vec![local]).unwrap().unwrap();
        packets.push(
            json!({"target":target,"members":members,"bytes":connected.body().len(),
            "body":serde_json::from_str::<serde_json::Value>(connected.body()).unwrap()}),
        );
    }
    let output = PathBuf::from(std::env::var("PLASM_STANDALONE_OUTPUT").unwrap());
    assert!(!output.exists());
    std::fs::write(output, serde_json::to_vec_pretty(&packets).unwrap()).unwrap();
}
fn key(name: &str) -> CapabilityRef {
    CapabilityRef {
        catalog: "matrix".into(),
        capability: name.into(),
    }
}
fn schema() -> CGS {
    let mut cgs =
        plasm_core::loader::load_schema_dir(&root().join("../../schemas/discovery_progress_v3"))
            .unwrap();
    cgs.capabilities
        .get_mut("source_read")
        .unwrap()
        .inputs
        .receiver = Some(plasm_core::CapabilityReceiver::None);
    cgs
}
fn apply_wave(session: &mut Option<TeachingExposureSession>, cgs: &CGS, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let delta = selected_capability_surface(cgs, "matrix", names).unwrap();
    let entities: Vec<_> = delta
        .required
        .entities
        .iter()
        .map(|e| e.entity.as_str())
        .collect();
    let entities: Vec<String> = entities.into_iter().map(str::to_owned).collect();
    let refs: Vec<_> = entities.iter().map(String::as_str).collect();
    if let Some(s) = session {
        s.expose_surface(&[cgs], Arc::new(cgs.clone()), "matrix", &refs, delta);
    } else {
        *session = Some(TeachingExposureSession::new_with_intent_delta(
            cgs, "matrix", &refs, delta,
        ));
    }
}
fn relation_visible(session: &TeachingExposureSession) -> bool {
    session
        .exposed_relation_symbol_rows()
        .iter()
        .any(|r| r.entity == "Source" && r.wire == "observations")
}
fn exposure_controls(fixture: &Fixture) -> Vec<serde_json::Value> {
    let cgs = schema();
    fixture
        .exposure_schedules
        .iter()
        .enumerate()
        .map(|(i, first)| {
            let mut session = None;
            apply_wave(&mut session, &cgs, first);
            let second: Vec<_> = ["source_read", "prepare"]
                .into_iter()
                .filter(|n| !first.iter().any(|x| x == n))
                .map(str::to_owned)
                .collect();
            apply_wave(&mut session, &cgs, &second);
            let session = session.unwrap();
            assert!(!session
                .surface
                .capabilities
                .iter()
                .any(|c| c.capability.as_str() == "process"));
            let teaching =
                prepare_python_teaching_wave(&session, &PythonTeachingState::default()).unwrap();
            json!({"id":format!("schedule_{i}"),"first":first,"second":second,
            "relation_visible":relation_visible(&session),"declarations":teaching.declarations,
            "capabilities":session.surface.capabilities,"slots":session.surface.slots})
        })
        .collect()
}
async fn run_case(fixture: &Fixture, case: &Case, reverse: bool) -> serde_json::Value {
    let mut cgs = schema();
    cgs.capabilities.get_mut("process").unwrap().description =
        fixture.description_clause.repeat(case.repeat);
    for i in 0..fixture.distractors {
        let name = format!("audit_{i:03}");
        let mut cap = cgs.capabilities["opaque"].clone();
        cap.name = name.clone().into();
        cap.description = "Read the audit settings for processing. This does not process or prepare source material.".into();
        cgs.capabilities.insert(name.into(), cap);
    }
    if reverse {
        cgs.capabilities = cgs.capabilities.into_iter().rev().collect();
    }
    let catalogs = BTreeMap::from([("matrix".into(), cgs)]);
    let cgs = &catalogs["matrix"];
    let deployments = DeploymentBindings::default();
    let mut allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    allowed.capabilities.insert(
        "matrix".into(),
        cgs.capabilities
            .keys()
            .filter(|n| !case.denied.iter().any(|d| d == n.as_str()))
            .map(|n| n.as_str().to_owned())
            .collect(),
    );
    let start = std::time::Instant::now();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let indexing_us = start.elapsed().as_micros();
    let mut visits = BTreeMap::new();
    let mut exposed = BTreeSet::new();
    let mut history = Vec::new();
    let mut session = None;
    let mut turns = Vec::new();
    let mut elapsed = Vec::new();
    for turn in 0..fixture.searches {
        history.push(case.needs[turn % case.needs.len()].clone());
        let intent = IntentProvenance::from_turns(history.clone()).unwrap();
        let existing: Vec<_> = exposed.iter().cloned().collect();
        let context = SearchContext {
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "progress-v3",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &existing,
            visits: &visits,
        };
        let rank = index.ranking.rank(history.last().unwrap());
        let frontier = connected::root_order(&index, context);
        let mut standalone = BTreeMap::new();
        for name in &case.expected {
            let target = key(name);
            if index.candidates.contains_key(&target) {
                let (local, _) = packet(&index, &target, &[], context).unwrap();
                let request = request_packing::connected(vec![local]).unwrap().unwrap();
                let bytes = request.body().len();
                if !reverse {
                    if let Ok(directory) = std::env::var("PLASM_PROGRESS_STANDALONE") {
                        let directory = PathBuf::from(directory);
                        std::fs::create_dir_all(&directory).unwrap();
                        let path = directory.join(format!("{}-{}-{name}.json", case.id, turn + 1));
                        assert!(!path.exists(), "preserve earlier diagnostic packets");
                        std::fs::write(path, request.body()).unwrap();
                    }
                }
                standalone.insert(name.clone(), bytes);
            }
        }
        let packets = RefCell::new(Vec::new());
        let start = std::time::Instant::now();
        let (retrieval, matches, receipt) = select_indexed(context, &index, |request| {
            assert!(request.body().len() <= 16_000);
            let body: serde_json::Value = serde_json::from_str(request.body()).unwrap();
            for question in &request.questions {
                assert!(allowed.permits(&question.reference));
                let card = body["state"]["question_evidence"].as_object().unwrap().values().flat_map(|q| q["capabilities"].as_object().unwrap().values()).find(|c| c["capability"]["reference"] == serde_json::to_value(&question.reference).unwrap()).unwrap();
                assert_eq!(card["capability"]["operation"]["contract"], cgs.capabilities[question.reference.capability.as_str()].description);
            }
            let answers: BTreeMap<_,_> = request.bindings.iter().map(|(qid, cid)| {
                let c = index.candidates.values().find(|c| &c.id == cid).unwrap();
                let choice = if case.reject { "unrelated" } else { case.labels.get(&c.reference.capability).map(String::as_str).unwrap_or("unrelated") };
                (qid.clone(),json!({"type":"choice","choice":choice,"confidence":1.0,"probabilities":{"relevant":if choice=="relevant" {1.0} else {0.0},"uncertain":if choice=="uncertain" {1.0} else {0.0},"unrelated":if choice=="unrelated" {1.0} else {0.0}}}))
            }).collect();
            packets.borrow_mut().push(json!({"body":body,"bytes":request.body().len(),"sha256":plasm_core::catalog_discovery::content_hash(request.body().as_bytes())}));
            let response=json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","fixture_response":true,"answers":answers}).to_string();
            async move { Ok(response) }
        }, |_| async {Ok(())}).await.unwrap();
        elapsed.push(start.elapsed().as_micros());
        assert!(receipt.requests <= 1 && receipt.input_bytes <= 16_000);
        let previous = exposed.clone();
        let mut decisions = BTreeMap::new();
        let mut repeats = 0;
        for candidate in &retrieval.candidates {
            let answer = matches
                .matches
                .iter()
                .find(|m| m.capability_id == candidate.id)
                .unwrap();
            assert!(!exposed.contains(&candidate.reference));
            if answer.choice == MatchChoice::Unrelated && visits.contains_key(&candidate.reference)
            {
                repeats += 1;
            }
            *visits.entry(candidate.reference.clone()).or_insert(0usize) += 1;
            decisions.insert(candidate.reference.clone(), answer.choice.clone());
            if answer.choice != MatchChoice::Unrelated {
                exposed.insert(candidate.reference.clone());
            }
        }
        for edge in required(&exposed, &catalogs, &deployments, &allowed).unwrap() {
            exposed.insert(edge.source);
        }
        assert!(previous.is_subset(&exposed) && exposed.iter().all(|r| allowed.permits(r)));
        let delta: Vec<_> = exposed
            .difference(&previous)
            .map(|r| r.capability.clone())
            .collect();
        apply_wave(&mut session, cgs, &delta);
        let status: Vec<_> = case.labels.iter().map(|(name,label)| {
            let target=key(name);
            let deferred=receipt.deferred.iter().find(|d| d.reference == target);
            let stage=if !allowed.permits(&target) {"unauthorized"} else if !index.candidates.contains_key(&target) {"not_indexed"} else if exposed.contains(&target) {"retained"} else if decisions.get(&target)==Some(&MatchChoice::Unrelated) {"judgment_rejected"} else if deferred.is_some() {"allocation_deferred"} else if !frontier.contains(&target) {"ranking_unvisited"} else {"allocation_unvisited"};
            json!({"name":name,"label":label,"stage":stage,"rank":rank.iter().position(|r|r==&target).map(|n|n+1),"frontier_rank":frontier.iter().position(|r|r==&target).map(|n|n+1),"standalone_bytes":standalone.get(name),"deferred":deferred,"judgment":decisions.get(&target)})
        }).collect();
        let visible = session.as_ref().is_some_and(relation_visible);
        turns.push(json!({"search":turn+1,"need":history.last(),"status":status,"newly_retained":delta,"retained":exposed,"complete":case.expected.iter().all(|n|exposed.contains(&key(n))),"relation_visible":visible,"repeated_rejections":repeats,"receipt":receipt,"packets":packets.into_inner()}));
    }
    json!({"id":case.id,"positive":case.positive,"turns":turns,"timing":{"indexing_us":indexing_us,"selection_us":elapsed}})
}

#[tokio::test]
#[ignore = "frozen offline development gradient; use progress-v3/run.py"]
async fn fixed_native_progress_suite_v3() {
    let fixture: Fixture =
        serde_json::from_slice(&std::fs::read(root().join("cases.json")).unwrap()).unwrap();
    assert_eq!(fixture.version, 3);
    let destination = PathBuf::from(std::env::var("PLASM_PROGRESS_REPORT").unwrap());
    assert!(!destination.exists());
    let exposure = exposure_controls(&fixture);
    let mut cases = Vec::new();
    for case in &fixture.cases {
        let a = run_case(&fixture, case, false).await;
        let b = run_case(&fixture, case, true).await;
        assert_eq!(a["turns"], b["turns"], "{} insertion permutation", case.id);
        println!("progress-v3 {} measured", case.id);
        cases.push(a);
    }
    std::fs::write(destination,serde_json::to_vec_pretty(&json!({"version":3,"policy":SEARCH_POLICY,"cases":cases,"exposure":exposure,"provider_calls":0})).unwrap()).unwrap();
}
