//! Lexical/allocation component gradient; this does not exercise hybrid recall.
//! Labels never enter retrieval or allocation. Service tests cover the database seam.
use super::*;
use plasm_core::{
    capability_exposure::selected_capability_surface,
    prompt_render::python::{prepare_python_teaching_wave, PythonTeachingState},
    symbol_tuning::TeachingExposureSession,
};
use std::{cell::RefCell, path::PathBuf, sync::Arc};

#[path = "resource_growth.rs"]
mod resource_growth;

#[path = "retention_value.rs"]
mod retention_value;

#[path = "catalog_partition.rs"]
mod catalog_partition;

#[path = "rare_action.rs"]
mod rare_action;

#[path = "action_role_pairs.rs"]
mod action_role_pairs;

#[path = "production_pipeline.rs"]
mod production_pipeline;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    version: usize,
    horizon: usize,
    gate: usize,
    growth: Vec<usize>,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    requests: Vec<String>,
    labels: BTreeMap<String, String>,
    outcomes: Vec<Vec<String>>,
    relation: bool,
}
fn root() -> PathBuf {
    PathBuf::from(crate::test_manifest_dir()).join("../../fixtures/discovery/trajectory-v4")
}
fn key(name: &str) -> CapabilityRef {
    CapabilityRef {
        catalog: "workshop".into(),
        capability: name.into(),
    }
}
fn catalogs(count: usize, reverse: bool) -> BTreeMap<String, CGS> {
    let mut workshop =
        plasm_core::loader::load_schema_dir(&root().join("../../schemas/discovery_trajectory_v4"))
            .unwrap();
    for name in ["sample_read", "cabinet_read", "specimen_read"] {
        workshop.capabilities.get_mut(name).unwrap().inputs.receiver =
            Some(plasm_core::CapabilityReceiver::None);
    }
    let mut journal = workshop.clone();
    journal.bind_registry_entry_id("journal");
    journal.capabilities.clear();
    for i in 0..count {
        let name = format!("r{i:04}");
        let mut capability = workshop.capabilities["record"].clone();
        capability.name = name.clone().into();
        journal.capabilities.insert(name.into(), capability);
    }
    if reverse {
        workshop.capabilities = workshop.capabilities.into_iter().rev().collect();
        journal.capabilities = journal.capabilities.into_iter().rev().collect();
    }
    let mut result = BTreeMap::from([("workshop".into(), workshop)]);
    if count != 0 {
        result.insert("journal".into(), journal);
    }
    result
}
fn teach(session: &mut Option<TeachingExposureSession>, cgs: &CGS, names: &[String]) {
    teach_catalog(session, cgs, "workshop", names);
}
fn teach_catalog(
    session: &mut Option<TeachingExposureSession>,
    cgs: &CGS,
    entry: &str,
    names: &[String],
) {
    if names.is_empty() {
        return;
    }
    let delta = selected_capability_surface(cgs, entry, names).unwrap();
    let entities: Vec<_> = delta
        .required
        .entities
        .iter()
        .map(|e| e.entity.as_str())
        .collect();
    if let Some(s) = session {
        s.expose_surface(
            &[cgs],
            Arc::new(cgs.clone()),
            entry,
            &entities,
            delta.clone(),
        );
    } else {
        *session = Some(TeachingExposureSession::new_with_intent_delta(
            cgs,
            entry,
            &entities,
            delta.clone(),
        ));
    }
}
async fn measure(
    fixture: &Fixture,
    case: &Case,
    count: usize,
    rotate: bool,
    reverse: bool,
    reject: bool,
) -> serde_json::Value {
    let catalogs = catalogs(count, reverse);
    measure_catalogs(fixture, case, count, rotate, reject, catalogs).await
}

async fn measure_catalogs(
    fixture: &Fixture,
    case: &Case,
    count: usize,
    rotate: bool,
    reject: bool,
    catalogs: BTreeMap<String, CGS>,
) -> serde_json::Value {
    measure_retention(fixture, case, count, rotate, reject, catalogs, false).await
}

async fn measure_retention(
    fixture: &Fixture,
    case: &Case,
    count: usize,
    rotate: bool,
    reject: bool,
    catalogs: BTreeMap<String, CGS>,
    retain_admitted: bool,
) -> serde_json::Value {
    let deployments = DeploymentBindings::default();
    let allowed = DiscoveryAuthorization::catalogs(catalogs.keys().cloned().collect());
    let start = std::time::Instant::now();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let indexing_us = start.elapsed().as_micros();
    let mut visits = BTreeMap::new();
    let mut exposed = BTreeSet::new();
    let mut session = None;
    let mut turns = Vec::new();
    let mut timings = Vec::new();
    let mut teaching_timings = Vec::new();
    let mut history = Vec::new();
    for turn in 0..fixture.horizon {
        let request = &case.requests[if rotate {
            turn % case.requests.len()
        } else {
            0
        }];
        history.push(request.clone());
        let intent = IntentProvenance::from_turns(history.clone()).unwrap();
        let existing: Vec<_> = exposed.iter().cloned().collect();
        let context = SearchContext {
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "trajectory-v4",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &existing,
            visits: &visits,
        };
        let ranking = index.ranking.rank(request);
        let frontier = connected::root_order(&index, context);
        let attribution = if turn == 0 {
            let plan = connected::plan(&index, context).unwrap();
            Some((
                index.ranking.explain(request),
                plan.diagnostic,
                plan.issued.map(|issued| issued.body().to_owned()),
            ))
        } else {
            None
        };
        let packets = RefCell::new(Vec::new());
        let start = std::time::Instant::now();
        let (retrieval, matches, receipt) = select_indexed(context, &index, |issued| {
            assert!(issued.body().len() <= 16_000);
            let body: serde_json::Value = serde_json::from_str(issued.body()).unwrap();
            for q in &issued.questions {
                assert!(allowed.permits(&q.reference));
                let card = body["state"]["question_evidence"].as_object().unwrap().values()
                    .flat_map(|e| e["capabilities"].as_object().unwrap().values())
                    .find(|c| c["capability"]["reference"] == serde_json::to_value(&q.reference).unwrap()).unwrap();
                assert_eq!(card["capability"]["operation"]["contract"],
                    catalogs[&q.reference.catalog].capabilities[q.reference.capability.as_str()].description);
            }
            let answers: BTreeMap<_, _> = issued.bindings.iter().map(|(qid, cid)| {
                let c = index.candidates.values().find(|c| &c.id == cid).unwrap();
                let choice = if retain_admitted { "uncertain" } else if c.reference.catalog == "workshop" && !(reject && c.reference.capability == "fire") {
                    case.labels.get(&c.reference.capability).map(String::as_str).unwrap_or("unrelated")
                } else { "unrelated" };
                (qid.clone(), json!({"type":"choice", "choice":choice, "confidence":1.0,
                    "probabilities":{"relevant":if choice=="relevant" {1.0} else {0.0},
                    "uncertain":if choice=="uncertain" {1.0} else {0.0},
                    "unrelated":if choice=="unrelated" {1.0} else {0.0}}}))
            }).collect();
            packets.borrow_mut().push(json!({"body":body,"bytes":issued.body().len()}));
            let answer = json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","fixture_response":true,"answers":answers}).to_string();
            async move { Ok(answer) }
        }, |_| async { Ok(()) }).await.unwrap();
        timings.push(start.elapsed().as_micros());
        assert!(receipt.requests <= 1 && receipt.input_bytes <= 16_000);
        if let Some((_, _, expected)) = &attribution {
            let observed = packets.borrow();
            assert_eq!(
                expected
                    .as_ref()
                    .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap()),
                observed.first().map(|p| p["body"].clone())
            );
        }
        let before = exposed.clone();
        let mut decisions = BTreeMap::new();
        let mut repeated = 0;
        let mut negative_targets = 0;
        for c in &retrieval.candidates {
            let answer = matches
                .matches
                .iter()
                .find(|m| m.capability_id == c.id)
                .unwrap();
            assert!(!exposed.contains(&c.reference));
            if answer.choice == MatchChoice::Unrelated && visits.contains_key(&c.reference) {
                repeated += 1;
            }
            if c.reference.catalog == "journal" {
                negative_targets += 1;
            }
            *visits.entry(c.reference.clone()).or_insert(0usize) += 1;
            decisions.insert(c.reference.clone(), answer.choice.clone());
            if answer.choice != MatchChoice::Unrelated {
                exposed.insert(c.reference.clone());
            }
        }
        for edge in required(&exposed, &catalogs, &deployments, &allowed).unwrap() {
            exposed.insert(edge.source);
        }
        assert!(before.is_subset(&exposed));
        let delta: Vec<_> = exposed
            .difference(&before)
            .map(|r| {
                assert!(allowed.permits(r));
                if !retain_admitted {
                    assert_eq!(r.catalog, "workshop");
                    assert!(case.labels.contains_key(&r.capability));
                }
                r.capability.clone()
            })
            .collect();
        let teaching_start = std::time::Instant::now();
        if retain_admitted {
            for (entry, cgs) in &catalogs {
                let names: Vec<_> = exposed
                    .difference(&before)
                    .filter(|r| &r.catalog == entry)
                    .map(|r| r.capability.clone())
                    .collect();
                teach_catalog(&mut session, cgs, entry, &names);
            }
        } else {
            teach(&mut session, &catalogs["workshop"], &delta);
        }
        let relation_visible = session.as_ref().is_some_and(|s| {
            s.exposed_relation_symbol_rows()
                .iter()
                .any(|r| r.entity == "Cabinet" && r.wire == "specimens")
        });
        let declaration_bytes = session
            .as_ref()
            .map(|s| {
                prepare_python_teaching_wave(s, &PythonTeachingState::default())
                    .unwrap()
                    .declarations
                    .len()
            })
            .unwrap_or(0);
        teaching_timings.push(teaching_start.elapsed().as_micros());
        let outcomes: Vec<_> = case
            .outcomes
            .iter()
            .map(|path| {
                path.iter().all(|n| exposed.contains(&key(n)))
                    && (!case.relation || relation_visible)
            })
            .collect();
        let status: Vec<_> = case.labels.keys().map(|name| {
            let target = key(name);
            let deferred = receipt.deferred.iter().find(|d| d.reference == target);
            let stage = if exposed.contains(&target) { "retained" }
                else if decisions.get(&target)==Some(&MatchChoice::Unrelated) { "judgment_rejected" }
                else if deferred.is_some() { "budget_deferred" }
                else if !frontier.contains(&target) { "ranking_unvisited" } else { "allocation_unvisited" };
            json!({"target":target,"stage":stage,"lexical_rank":ranking.iter().position(|r|r==&target).map(|n|n+1),
                "frontier_rank":frontier.iter().position(|r|r==&target).map(|n|n+1),"deferred":deferred,"decision":decisions.get(&target)})
        }).collect();
        turns.push(json!({"search":turn+1,"request":request,"outcomes":outcomes,
            "relation_visible":relation_visible,"newly_exposed":delta,"declaration_bytes":declaration_bytes,
            "exposed_references":exposed,"incidental_exposure":exposed.iter().filter(|r| r.catalog != "workshop" || !case.labels.contains_key(&r.capability)).count(),
            "status":status,"repeated_rejections":repeated,"journal_targets":negative_targets,
            "receipt":receipt,"packets":packets.into_inner(),
            "attribution":attribution.map(|(scores,admissions,_)|json!({"scores":scores,"admissions":admissions}))}));
    }
    json!({"id":case.id,"distractors":count,"rotating":rotate,"diagnostic_rejection":reject,
        "turns":turns,"timing":{"indexing_us":indexing_us,"selection_us":timings,"teaching_us":teaching_timings}})
}

#[tokio::test]
#[ignore = "offline development gradient; trajectory-v4/run.py controls freeze and quality gate"]
async fn frozen_discovery_trajectory_v4() {
    let fixture: Fixture =
        serde_json::from_slice(&std::fs::read(root().join("cases.json")).unwrap()).unwrap();
    assert_eq!((fixture.version, fixture.horizon, fixture.gate), (4, 24, 4));
    let destination = PathBuf::from(std::env::var("PLASM_TRAJECTORY_REPORT").unwrap());
    assert!(!destination.exists());
    let mut results = Vec::new();
    for case in &fixture.cases {
        for &count in &fixture.growth {
            for rotate in [false, true] {
                let forward = measure(&fixture, case, count, rotate, false, false).await;
                let reverse = measure(&fixture, case, count, rotate, true, false).await;
                assert_eq!(
                    forward["turns"], reverse["turns"],
                    "{} / {count} / {rotate}",
                    case.id
                );
                println!(
                    "trajectory {} / {count} / rotating={rotate} measured",
                    case.id
                );
                results.push(forward);
            }
        }
    }
    let diagnostic = measure(&fixture, &fixture.cases[0], 0, false, false, true).await;
    assert!(diagnostic["turns"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["status"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["stage"] == "judgment_rejected")));
    std::fs::write(destination, serde_json::to_vec_pretty(&json!({"policy":SEARCH_POLICY,"cases":results,"diagnostic":diagnostic,"provider_calls":0})).unwrap()).unwrap();
}

#[tokio::test]
#[ignore = "frozen root-diversity counter-controls; explicit report path required"]
async fn frozen_discovery_root_counter_controls() {
    let fixture: Fixture = serde_json::from_slice(
        &std::fs::read(root().join("../root-diversity-v1/cases.json")).unwrap(),
    )
    .unwrap();
    assert_eq!((fixture.version, fixture.horizon, fixture.gate), (4, 24, 4));
    let destination = PathBuf::from(std::env::var("PLASM_TRAJECTORY_REPORT").unwrap());
    assert!(!destination.exists());
    let mut results = Vec::new();
    for case in &fixture.cases {
        for &count in &fixture.growth {
            for rotate in [false, true] {
                let forward = measure(&fixture, case, count, rotate, false, false).await;
                let reverse = measure(&fixture, case, count, rotate, true, false).await;
                assert_eq!(forward["turns"], reverse["turns"]);
                results.push(forward);
            }
        }
    }
    std::fs::write(
        destination,
        serde_json::to_vec_pretty(
            &json!({"policy":SEARCH_POLICY,"cases":results,"provider_calls":0}),
        )
        .unwrap(),
    )
    .unwrap();
}
