//! Frozen scheduling ablation; no production policy switch or model calls.
use super::{tests::fixture, *};

#[derive(Clone, Copy, Debug)]
enum VisitScope {
    CurrentRequest,
    Session,
    Split,
}

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Reformulation,
    SameRequest,
    ChangedConstraint,
    NewOutcome,
}

#[derive(Debug)]
struct Observation {
    first_target_turn: Option<usize>,
    unique: usize,
    repeated: usize,
    max_bytes: usize,
}

fn reference(n: usize) -> CapabilityRef {
    CapabilityRef {
        catalog: "matrix".into(),
        capability: format!("operation_{n:03}"),
    }
}

fn measure(scope: VisitScope, scenario: Scenario, distractors: usize) -> Observation {
    let mut catalogs = fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let independent = cgs.capabilities["opaque"].clone();
    cgs.capabilities.clear();
    for n in 0..=distractors {
        let mut cap = independent.clone();
        cap.name = reference(n).capability.into();
        cap.description = format!(
            "Apply independent routine {n}. {}",
            "Preserve the authored material and its recorded constraints. ".repeat(80)
        );
        cgs.capabilities.insert(cap.name.clone(), cap);
    }
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let target = reference(4);
    let changed = matches!(scenario, Scenario::ChangedConstraint | Scenario::NewOutcome);
    let mut frontier = BTreeMap::<String, BTreeMap<CapabilityRef, usize>>::new();
    // Prior requests judged this operation; no relevance decision or permission
    // is inherited. Only the experimental session arm inherits its visit count.
    if changed {
        frontier.insert("session".into(), BTreeMap::from([(target.clone(), 1)]));
        frontier.insert(
            "Inspect specimens without changes.".into(),
            BTreeMap::from([(target.clone(), 1)]),
        );
    }
    let reformulations = [
        "Inspect specimens without changes.",
        "Observe the specimens while leaving them unchanged.",
        "Read specimen information without modifying specimens.",
        "Obtain observations of specimens; preserve their state.",
        "Show specimen observations without applying changes.",
        "Find a read-only observation of the specimens.",
    ];
    let mut seen = BTreeSet::new();
    let mut repeated = 0;
    let mut max_bytes = 0;
    let mut first_target_turn = None;
    for turn in 0..6 {
        let current = match scenario {
            Scenario::Reformulation => reformulations[turn],
            Scenario::SameRequest => reformulations[0],
            Scenario::ChangedConstraint => {
                "Inspect specimens; a reversible adjustment is now permitted."
            }
            Scenario::NewOutcome => "Calibrate the specimen; preserve other specimens.",
        };
        let key = match scope {
            VisitScope::CurrentRequest => current,
            VisitScope::Session | VisitScope::Split => "session",
        };
        let visits = frontier.entry(key.into()).or_default();
        let intent = IntentProvenance::from_turns([current.into()]).unwrap();
        let mut recalled = (0..=distractors).map(reference).collect::<Vec<_>>();
        if changed {
            recalled.retain(|r| r != &target);
            recalled.insert(0, target.clone());
        }
        // Freeze one current-recall opportunity, then advance the session's
        // least-visited candidates. This is experimental ordering only.
        if matches!(scope, VisitScope::Split) {
            let requested = recalled.remove(0);
            recalled.sort_by_key(|key| visits.get(key).copied().unwrap_or(0));
            recalled.insert(0, requested);
        }
        let planner = if matches!(scope, VisitScope::Split) {
            connected::plan_ordered
        } else {
            connected::plan_recalled
        };
        let plan = planner(
            &index,
            SearchContext {
                model: RELEVANCE_MODEL,
                intent: &intent,
                generation: "reformulation-scope-v1",
                catalogs: &catalogs,
                deployments: &deployments,
                allowed: &allowed,
                exposed: &[],
                visits,
            },
            recalled,
        )
        .unwrap();
        let issued = plan.issued.unwrap();
        max_bytes = max_bytes.max(issued.body().len());
        assert!(issued.body().len() <= search_budget::MAX_INPUT_BYTES);
        assert!(!plan.wave.is_empty());
        for (cap, _) in plan.wave {
            if cap == target && first_target_turn.is_none() {
                first_target_turn = Some(turn + 1);
            }
            repeated += usize::from(!seen.insert(cap.clone()));
            *visits.entry(cap).or_default() += 1;
        }
    }
    Observation {
        first_target_turn,
        unique: seen.len(),
        repeated,
        max_bytes,
    }
}

#[test]
fn frozen_reformulation_scope_ablation() {
    for distractors in [8, 64] {
        for scenario in [
            Scenario::Reformulation,
            Scenario::SameRequest,
            Scenario::ChangedConstraint,
            Scenario::NewOutcome,
        ] {
            for scope in [
                VisitScope::CurrentRequest,
                VisitScope::Session,
                VisitScope::Split,
            ] {
                let result = measure(scope, scenario, distractors);
                println!("REFORMULATION_SCOPE {distractors} {scenario:?} {scope:?} target={:?} unique={} repeated={} max_bytes={}", result.first_target_turn, result.unique, result.repeated, result.max_bytes);
                // These are integrity checks, not a concealed promotion gate.
                // Missing targets are reported as experiment failures, not
                // hidden by expanding the packet or call horizon.
                assert!(result.unique > 0);
                assert!(result.max_bytes <= search_budget::MAX_INPUT_BYTES);
            }
        }
    }
}
