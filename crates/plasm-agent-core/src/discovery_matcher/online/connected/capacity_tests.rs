//! Fixed neutral capacity controls. Passing means valid observations, not promotion.
use super::*;
use crate::discovery_matcher::online::tests::fixture;

#[derive(Clone, Copy, Debug)]
enum Arm {
    Exact,
    Session,
    Split,
    AlternatingFresh,
    AlternatingProgress,
    DeferredNomination,
    ExplicitCorrect,
    RepeatedFalseRevision,
    MissedRevision,
    OneSpuriousRevision,
    DelayedRevision,
}
#[derive(Clone, Copy, Debug)]
enum Case {
    ExpensiveFirst,
    ExpensiveTarget,
    Equal,
    Changed,
    ChangedSecond,
    Connected,
    SameRequest,
}

fn reference(n: usize) -> CapabilityRef {
    CapabilityRef {
        catalog: "matrix".into(),
        capability: format!("routine_{n:03}"),
    }
}

#[test]
fn frozen_capacity_progress_ablation() {
    for distractors in [8, 64] {
        for position in [1, 4] {
            for case in [
                Case::ExpensiveFirst,
                Case::ExpensiveTarget,
                Case::Equal,
                Case::Changed,
                Case::ChangedSecond,
                Case::Connected,
                Case::SameRequest,
            ] {
                for arm in [
                    Arm::Exact,
                    Arm::Session,
                    Arm::Split,
                    Arm::AlternatingFresh,
                    Arm::AlternatingProgress,
                    Arm::DeferredNomination,
                ] {
                    let _ = measure(distractors, position, case, arm);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ObjectiveSignal {
    Continue,
    Revise,
}

fn objective_signal(arm: Arm, case: Case, turn: usize) -> ObjectiveSignal {
    let changed = matches!(case, Case::Changed | Case::ChangedSecond);
    let revise = match arm {
        Arm::ExplicitCorrect => changed && turn == 0,
        Arm::RepeatedFalseRevision => true,
        Arm::MissedRevision => false,
        Arm::OneSpuriousRevision => (changed && turn == 0) || turn == 1,
        Arm::DelayedRevision => changed && turn == 1,
        _ => false,
    };
    if revise {
        ObjectiveSignal::Revise
    } else {
        ObjectiveSignal::Continue
    }
}

#[test]
fn frozen_objective_revision_ablation() {
    for distractors in [8, 64] {
        for position in [1, 4] {
            for case in [
                Case::ExpensiveFirst,
                Case::ExpensiveTarget,
                Case::Equal,
                Case::Changed,
                Case::Connected,
                Case::SameRequest,
            ] {
                for arm in [
                    Arm::ExplicitCorrect,
                    Arm::RepeatedFalseRevision,
                    Arm::MissedRevision,
                    Arm::OneSpuriousRevision,
                    Arm::DelayedRevision,
                ] {
                    let _ = measure(distractors, position, case, arm);
                }
            }
        }
    }
}

#[test]
fn repeatedly_deferred_recalled_root_eventually_receives_capacity() {
    assert_eq!(
        measure(8, 1, Case::ExpensiveFirst, Arm::DeferredNomination),
        Some(4)
    );
    assert_eq!(
        measure(8, 1, Case::Connected, Arm::DeferredNomination),
        Some(3)
    );
}

fn measure(distractors: usize, position: usize, case: Case, arm: Arm) -> Option<usize> {
    let mut catalogs = fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let prototype = cgs.capabilities["opaque"].clone();
    if !matches!(case, Case::Connected) {
        cgs.capabilities.clear();
    }
    for n in 0..=distractors {
        let mut cap = prototype.clone();
        cap.name = reference(n).capability.into();
        let width = if (matches!(case, Case::ExpensiveFirst) && n == position)
            || (matches!(case, Case::ExpensiveTarget) && n == 0)
        {
            110
        } else {
            140
        };
        cap.description = format!(
            "Apply routine {n}. {}",
            "Preserve the authored material and its recorded constraints. ".repeat(width)
        );
        cgs.capabilities.insert(cap.name.clone(), cap);
    }
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let deployments = Default::default();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let target = reference(position);
    let mut session = BTreeMap::new();
    if matches!(case, Case::Changed) {
        session.insert(target.clone(), 1);
    }
    let mut exact = BTreeMap::<String, BTreeMap<CapabilityRef, usize>>::new();
    let mut seen = BTreeSet::new();
    let mut repeats = 0;
    let mut first = None;
    let mut max_bytes = 0;
    let mut optional = 0;
    let mut deferred_age = BTreeMap::<CapabilityRef, usize>::new();
    let mut previous_intent = None;
    let texts = [
        "Inspect the specimens.",
        "Observe these specimens.",
        "Read the specimen observations.",
        "Obtain observations of the specimens.",
        "Show observations for these specimens.",
        "Find the specimens' observations.",
    ];
    for turn in 0..6 {
        if matches!(objective_signal(arm, case, turn), ObjectiveSignal::Revise) {
            session.clear();
        }
        let current =
            if matches!(case, Case::Changed) || (matches!(case, Case::ChangedSecond) && turn > 0) {
                "Calibrate the specimens; reversible adjustments are now permitted."
            } else if matches!(case, Case::SameRequest) {
                texts[0]
            } else {
                texts[turn]
            };
        let intent = IntentProvenance::from_turns([current.into()]).unwrap();
        let visits = if matches!(arm, Arm::Exact | Arm::DeferredNomination) {
            exact.entry(current.into()).or_default()
        } else {
            &mut session
        };
        let context = SearchContext {
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "capacity-v1",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &[],
            visits,
        };
        if turn == 0 {
            let mut cache = BTreeMap::new();
            for root in [reference(0), target.clone()] {
                assert!(
                    body_size(&Bundle::from([(root, vec![])]), &index, context, &mut cache)
                        .unwrap()
                        <= search_budget::MAX_INPUT_BYTES
                );
            }
            assert!(
                body_size(
                    &Bundle::from([(reference(0), vec![]), (target.clone(), vec![])]),
                    &index,
                    context,
                    &mut cache
                )
                .unwrap()
                    > search_budget::MAX_INPUT_BYTES
            );
        }
        let mut roots = (0..=distractors).map(reference).collect::<Vec<_>>();
        if matches!(case, Case::Changed) {
            roots.retain(|r| r != &target);
            roots.insert(0, target.clone());
        }
        if matches!(case, Case::Connected) {
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
        let freshness = matches!(arm, Arm::Split)
            || (matches!(arm, Arm::AlternatingFresh) && turn % 2 == 0)
            || (matches!(arm, Arm::AlternatingProgress) && turn % 2 == 1);
        let deferred_nomination = matches!(arm, Arm::DeferredNomination)
            && previous_intent.is_some_and(|previous| previous != current);
        let experimental = matches!(
            arm,
            Arm::Split | Arm::AlternatingFresh | Arm::AlternatingProgress
        ) || deferred_nomination;
        if experimental && !deferred_nomination {
            let requested = if freshness {
                Some(roots.remove(0))
            } else {
                None
            };
            roots.sort_by_key(|r| visits.get(r).copied().unwrap_or(0));
            if let Some(root) = requested {
                roots.insert(0, root);
            }
        }
        let plan = if deferred_nomination {
            plan_recalled_with_deferred(&index, context, roots, &deferred_age)
        } else if experimental {
            plan_ordered(&index, context, roots)
        } else {
            plan_recalled(&index, context, roots)
        }
        .unwrap();
        deferred_age = plan
            .deferred
            .iter()
            .filter(|(_, reason)| matches!(reason, DeferredReason::InputBytes))
            .map(|(reference, _)| {
                (
                    reference.clone(),
                    deferred_age
                        .get(reference)
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(1),
                )
            })
            .collect();
        previous_intent = Some(current);
        let bytes = plan.issued.as_ref().unwrap().body().len();
        assert!(bytes <= search_budget::MAX_INPUT_BYTES);
        max_bytes = max_bytes.max(bytes);
        optional += plan
            .diagnostic
            .iter()
            .filter(|v| v["kind"] == "optional_path")
            .count();
        for (root, _) in plan.wave {
            if root == target
                && first.is_none()
                && (!matches!(case, Case::ChangedSecond) || turn > 0)
            {
                first = Some(turn + 1);
            }
            repeats += usize::from(!seen.insert(root.clone()));
            *visits.entry(root).or_default() += 1;
        }
    }
    println!("CAPACITY {distractors} {position} {case:?} {arm:?} first={first:?} unique={} repeats={repeats} optional={optional} max_bytes={max_bytes}",seen.len());
    first
}
