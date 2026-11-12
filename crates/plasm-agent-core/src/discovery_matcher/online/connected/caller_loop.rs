//! Explicit offline-native worker for the frozen caller-loop experiment.
use super::*;
use crate::discovery_matcher::online::tests::fixture;
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn reference(n: usize) -> CapabilityRef {
    CapabilityRef {
        catalog: "matrix".into(),
        capability: format!("routine_{n:03}"),
    }
}
fn goal(amended: bool) -> &'static str {
    if amended {
        "Apply an amber coating to specimen units instead of a violet inspection seal. Preserve their existing measurements and identifiers."
    } else {
        "Apply a violet inspection seal to specimen units. Preserve their existing measurements and identifiers."
    }
}
fn catalogs(case: &serde_json::Value) -> BTreeMap<String, CGS> {
    let mut catalogs = fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let prototype = cgs.capabilities["opaque"].clone();
    if !case["connected"].as_bool().unwrap() {
        cgs.capabilities.clear();
    }
    for n in 0..=case["distractors"].as_u64().unwrap() as usize {
        let mut cap = prototype.clone();
        cap.name = reference(n).capability.into();
        let is_target = n == case["position"].as_u64().unwrap() as usize;
        let purpose = if is_target {
            "Apply a violet inspection seal to specimen units."
        } else if n == 0 {
            "Apply an amber coating to specimen units."
        } else {
            "Perform an unrelated unit adjustment; this does not apply coatings or inspection seals."
        };
        let width = case[if is_target {
            "target_width"
        } else if n == 0 {
            "first_width"
        } else {
            "other_width"
        }]
        .as_u64()
        .unwrap() as usize;
        cap.description = format!(
            "{purpose} {}",
            "Preserve existing specimen measurements and identifiers. ".repeat(width)
        );
        cap.output_schema.as_mut().unwrap().output_type =
            plasm_core::schema::OutputType::SideEffect {
                description: purpose.into(),
            };
        cgs.capabilities.insert(cap.name.clone(), cap);
    }
    catalogs
}
fn step(
    case: &serde_json::Value,
    current: &str,
    visits: &mut BTreeMap<CapabilityRef, usize>,
    revise: bool,
    turn: usize,
) -> serde_json::Value {
    if revise {
        visits.clear();
    }
    let catalogs = catalogs(case);
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent = IntentProvenance::from_turns([current.into()]).unwrap();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "caller-loop-v1",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
        visits,
    };
    let mut roots = (0..=case["distractors"].as_u64().unwrap() as usize)
        .map(reference)
        .collect::<Vec<_>>();
    if case["connected"].as_bool().unwrap() {
        roots.splice(
            0..0,
            [
                CapabilityRef {
                    catalog: "matrix".into(),
                    capability: "process".into(),
                },
                CapabilityRef {
                    catalog: "matrix".into(),
                    capability: "source_read".into(),
                },
            ],
        );
    }
    let amended = case["amend_at"]
        .as_u64()
        .is_some_and(|at| turn >= at as usize);
    let target = reference(if amended {
        0
    } else {
        case["position"].as_u64().unwrap() as usize
    });
    if turn == 1 {
        let mut cache = BTreeMap::new();
        for r in [reference(0), target.clone()] {
            assert!(
                body_size(&Bundle::from([(r, vec![])]), &index, context, &mut cache).unwrap()
                    <= search_budget::MAX_INPUT_BYTES
            );
        }
        let combined = body_size(
            &Bundle::from([(reference(0), vec![]), (target.clone(), vec![])]),
            &index,
            context,
            &mut cache,
        )
        .unwrap();
        assert_eq!(
            combined > search_budget::MAX_INPUT_BYTES,
            case["other_width"].as_u64().unwrap() != 80
        );
    }
    let plan = plan_recalled(&index, context, roots).unwrap();
    let bytes = plan.issued.as_ref().unwrap().body().len();
    assert!(bytes <= search_budget::MAX_INPUT_BYTES);
    let mut judged = Vec::new();
    let mut found = false;
    for (r, _) in plan.wave {
        let relevant = r == target;
        found |= relevant;
        let purpose = &catalogs[&r.catalog].capabilities[r.capability.as_str()].description;
        judged.push(serde_json::json!({"capability":r,"purpose":purpose.split(" Preserve").next().unwrap(),"judgment":if relevant {"relevant"} else {"unrelated"}}));
        *visits.entry(r).or_default() += 1;
    }
    serde_json::json!({"found":found,"packet_bytes":bytes,"judged":judged,"more_candidates_exist":visits.len()<index.candidates.len()})
}
fn save(root: &Path, name: &str, value: &serde_json::Value) {
    let tmp = root.join(format!("{name}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    std::fs::rename(tmp, root.join(name)).unwrap();
}

#[test]
#[ignore = "explicit file protocol; native allocation only, no provider calls"]
fn frozen_caller_loop_worker() {
    let root = std::path::PathBuf::from(std::env::var("PLASM_CALLER_LOOP_ROOT").unwrap());
    let cases: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(root.join("cases.json")).unwrap()).unwrap();
    let mut controls = Vec::new();
    for case in &cases {
        let mut visits = BTreeMap::new();
        let mut first = None;
        let mut observations = Vec::new();
        for turn in 1..=6 {
            let amended = case["amend_at"]
                .as_u64()
                .is_some_and(|at| turn >= at as usize);
            let revise = turn == 1 || case["amend_at"].as_u64() == Some(turn as u64);
            let result = step(case, goal(amended), &mut visits, revise, turn);
            let found = result["found"].as_bool().unwrap();
            observations.push(result);
            if found {
                first = Some(turn);
                break;
            }
        }
        controls
            .push(serde_json::json!({"id":case["id"],"first":first,"observations":observations}));
    }
    save(
        &root,
        "READY.json",
        &serde_json::json!({"controls":controls}),
    );
    let start = Instant::now();
    let mut serial = 0;
    let mut state = BTreeMap::<String, (usize, BTreeMap<CapabilityRef, usize>)>::new();
    while !root.join("STOP").exists() {
        assert!(
            start.elapsed() < Duration::from_secs(1500),
            "caller loop worker deadline"
        );
        let path = root.join(format!("{serial:03}.request.json"));
        if !path.exists() {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        let request: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let id = request["id"].as_str().unwrap();
        let case = cases.iter().find(|c| c["id"] == id).unwrap();
        let (turn, visits) = state.entry(id.into()).or_default();
        *turn += 1;
        assert!(*turn <= 6);
        let signal = request["signal"].as_str().unwrap();
        assert!(matches!(signal, "continue" | "revise"));
        let result = step(
            case,
            request["intent"].as_str().unwrap(),
            visits,
            signal == "revise",
            *turn,
        );
        save(&root, &format!("{serial:03}.response.json"), &result);
        serial += 1;
    }
}
