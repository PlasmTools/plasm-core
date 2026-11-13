//! Neutral allocation controls: old path judgments must not starve fresh roots.
use super::*;
use crate::discovery_matcher::online::tests::fixture;

#[test]
fn bounded_packet_sizing_rejects_only_when_full_packet_exceeds_limit() {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let intent = IntentProvenance::from_turns(["Inspect and prepare the sample.".into()]).unwrap();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "size-bound",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
        visits: &Default::default(),
    };
    let mut bundle = Bundle::new();
    for key in index.candidates.keys() {
        bundle.insert(key.clone(), Vec::new());
        let full = body_size(&bundle, &index, context, &mut BTreeMap::new()).unwrap();
        for limit in [0, full / 2, full.saturating_sub(1), full, full + 1] {
            let bounded =
                body_size_up_to(&bundle, &index, context, &mut BTreeMap::new(), Some(limit))
                    .unwrap();
            assert_eq!(bounded > limit, full > limit);
            if bounded <= limit {
                assert_eq!(bounded, full);
            }
        }
    }
}

#[test]
fn deferred_recalled_root_precedes_optional_consumer_but_not_current_lead() {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let reference = |capability: &str| CapabilityRef {
        catalog: "matrix".into(),
        capability: capability.into(),
    };
    let intent = IntentProvenance::from_turns(["Inspect and prepare the sample.".into()]).unwrap();
    let exposed = [reference("source_read")];
    let visits = BTreeMap::new();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "fixture",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &exposed,
        visits: &visits,
    };
    let lead = reference("opaque");
    let deferred = reference("prepare");
    let order = order_recalled(
        &index,
        context,
        vec![lead.clone(), reference("process"), deferred.clone()],
        &BTreeMap::from([(deferred.clone(), 4)]),
    );
    assert_eq!(order[0], lead);
    assert_eq!(order[1], deferred);
}

fn allocate(previous_visits: usize, width: usize) -> Plan {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let reference = |name: &str| CapabilityRef {
        catalog: "matrix".into(),
        capability: name.into(),
    };
    let intent = IntentProvenance::from_turns([
        "Produce observations and retain an independent action.".into(),
    ])
    .unwrap();
    let mut visits: BTreeMap<_, _> = index
        .candidates
        .keys()
        .cloned()
        .map(|key| (key, previous_visits))
        .collect();
    // The anchor was exposed by an earlier request. This request has already
    // judged its optional suppliers, but never asks about the anchor again.
    visits.remove(&reference("process"));
    let exposed = [reference("process")];
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "allocation-progress",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &exposed,
        visits: &visits,
    };
    // Existing process anchors acquisition paths. Two fresh direct roots occupy
    // the initial root slots; the third competes with optional path expansion.
    let mut catalogs = catalogs.clone();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let prototype = cgs.capabilities["opaque"].clone();
    for name in ["fresh_a", "fresh_b", "fresh_c"] {
        let mut cap = prototype.clone();
        cap.name = name.into();
        if name == "fresh_c" {
            cap.description =
                "Preserve the authored material and its recorded constraints. ".repeat(width);
        }
        cgs.capabilities.insert(name.into(), cap);
    }
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let context = SearchContext {
        catalogs: &catalogs,
        ..context
    };
    let direct = Bundle::from([
        (reference("process"), vec![]),
        (reference("fresh_a"), vec![]),
        (reference("fresh_b"), vec![]),
        (reference("fresh_c"), vec![]),
    ]);
    let fits = body_size(&direct, &index, context, &mut BTreeMap::new()).unwrap()
        <= search_budget::MAX_INPUT_BYTES;
    assert_eq!(
        fits,
        width < 800,
        "control must distinguish fitting and oversized roots"
    );
    let plan = plan_recalled(
        &index,
        context,
        vec![
            reference("process"),
            reference("fresh_a"),
            reference("fresh_b"),
            reference("fresh_c"),
        ],
    )
    .unwrap();
    assert!(plan.issued.as_ref().unwrap().body().len() <= search_budget::MAX_INPUT_BYTES);
    plan
}

fn fresh_position(plan: &Plan) -> Option<usize> {
    plan.diagnostic.iter().position(|e| {
        e["members"].as_array().is_some_and(|members| {
            members
                .iter()
                .any(|m| m["reference"]["capability"] == "fresh_c")
        })
    })
}

#[test]
fn unjudged_root_precedes_repeated_optional_path() {
    let plan = allocate(3, 80);
    let events = &plan.diagnostic;
    let fresh = fresh_position(&plan);
    let repeat = events.iter().position(|e| e["kind"] == "optional_path");
    assert!(
        fresh.is_some(),
        "unjudged root was not admitted: {events:?}"
    );
    assert!(
        repeat.is_none_or(|repeat| fresh.unwrap() < repeat),
        "repeated optional path preceded unjudged root: {events:?}"
    );
}

#[test]
fn equal_visit_round_keeps_connected_path_priority() {
    let plan = allocate(0, 80);
    let optional = plan
        .diagnostic
        .iter()
        .position(|e| e["kind"] == "optional_path")
        .unwrap();
    assert!(fresh_position(&plan).is_none_or(|fresh| optional < fresh));
}

#[test]
fn oversized_unjudged_root_does_not_block_optional_paths() {
    let plan = allocate(3, 800);
    assert!(fresh_position(&plan).is_none());
    assert!(plan.diagnostic.iter().any(|e| e["kind"] == "optional_path"));
    assert!(plan
        .deferred
        .iter()
        .any(|(key, reason)| key.capability == "fresh_c"
            && matches!(reason, DeferredReason::InputBytes)));
}

#[test]
fn compact_unjudged_root_also_precedes_repeated_optional_paths() {
    let plan = allocate(3, 1);
    let fresh = fresh_position(&plan).unwrap();
    assert!(plan
        .diagnostic
        .iter()
        .position(|e| e["kind"] == "optional_path")
        .is_none_or(|optional| fresh < optional));
}

#[test]
fn input_supply_cycles_do_not_suppress_a_connected_candidate() {
    use crate::discovery_matcher::search_graph::SearchGraph;
    let mut catalogs = fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let mut alternate = cgs.capabilities["source_read"].clone();
    alternate.name = "source_alternate".into();
    cgs.capabilities.insert(alternate.name.clone(), alternate);
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let mut index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let reference = |name: &str| CapabilityRef {
        catalog: "matrix".into(),
        capability: name.into(),
    };
    let root = reference("process");
    let source = reference("source_read");
    let alternate = reference("source_alternate");
    use plasm_core::prerequisites::{InputSourceBinding, InputSourceTarget};
    let mut edges = Vec::new();
    for (provider, consumer, provenance) in [
        (source.clone(), root.clone(), "forward"),
        (alternate.clone(), root.clone(), "alternate"),
        (root.clone(), source.clone(), "reverse"),
    ] {
        index.input_bindings.insert(
            provenance.into(),
            InputSourceBinding {
                provider: provider.clone(),
                consumer: consumer.clone(),
                input: InputSourceTarget::Receiver {
                    entity: "Prepared".into(),
                },
                output_field: "id".into(),
                collect: false,
            },
        );
        edges.push(Edge {
            source: provider,
            consumer,
            relationship: Relationship::SuppliesInput,
            provenance: provenance.into(),
        });
    }
    index.graph = SearchGraph::new(index.candidates.keys().cloned().collect(), edges).unwrap();
    let intent = IntentProvenance::from_turns(["Inspect the sample.".into()]).unwrap();
    let exposed = [root.clone()];
    let visits = BTreeMap::new();
    let context = SearchContext {
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "input-cycle",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &exposed,
        visits: &visits,
    };
    let plan = plan_ordered(&index, context, vec![root], &mut PacketCache::default()).unwrap();
    assert!(plan.wave.iter().any(|(key, _)| key == &source));
    assert!(plan.wave.iter().any(|(key, _)| key == &alternate));
}
