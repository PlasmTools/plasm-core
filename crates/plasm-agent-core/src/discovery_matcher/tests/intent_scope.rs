//! Mechanical delivery proofs; classifier recall is measured separately.
use super::*;

#[test]
fn selection_receives_original_conditions_and_later_revision() {
    let (_, retrieval) = fixture();
    let intent = IntentProvenance::from_turns([
        "Inspect records belonging to the selected team.".into(),
        "Use the other team instead; inspect its members.".into(),
    ])
    .unwrap();
    let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    let body: serde_json::Value = serde_json::from_str(batches[0].body()).unwrap();
    let selection = body["state"]["outcome"].as_str().unwrap();
    assert!(selection.contains("records belonging to the selected team"));
    assert!(selection.contains("Use the other team instead"));
    assert!(selection.contains("later"));
    let different = IntentProvenance::from_turns([
        "Inspect records belonging to a different department.".into(),
        "Use the other team instead; inspect its members.".into(),
    ])
    .unwrap();
    let other = issue_batches(RELEVANCE_MODEL, &different, &retrieval).unwrap();
    assert_ne!(batches[0].cache_key, other[0].cache_key);
}

#[test]
fn current_request_preserves_explicit_revisions_constraints_and_unknown_state() {
    let policy: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../fixtures/discovery/contribution-only-policy.json"
    ))
    .unwrap();
    let histories = [
        vec!["Process the supplied records."],
        vec![
            "Process the supplied records.",
            "Inspect available definitions.",
        ],
        vec![
            "Process the supplied records.",
            "Read only; do not register definitions.",
        ],
        vec![
            "Process using existing definitions only.",
            "Inspect alternatives.",
        ],
        vec![
            "Process the supplied records.",
            "Cancel processing; report inventory only.",
        ],
        vec!["Process the supplied records.", "Readiness is unknown."],
        vec![
            "Process the supplied records.",
            "Every observed saved definition is incompatible.",
        ],
        vec![
            "Process the supplied records.",
            "Cancel processing.",
            "Resume processing; preserve existing definitions.",
        ],
        vec!["Request a measurement appointment. Preserve the source records."],
        vec![
            "Calculate the aggregate. Only if calculation is unavailable, request manual measurement. Preserve the source records.",
            "Calculation readiness is unknown.",
        ],
        vec![
            "Calculate the aggregate. Only if calculation is unavailable, request manual measurement. Preserve the source records.",
            "Calculation is unavailable with the observed inputs.",
        ],
        vec![
            "Calculate the aggregate. Only if calculation is unavailable, request manual measurement. Preserve the source records.",
            "Calculation is available with the observed inputs.",
        ],
        vec![
            "Calculate the aggregate. If calculation is unavailable, request manual measurement; if measurement is also unavailable, report the missing inputs.",
            "Inspect the available inputs.",
        ],
        vec![
            "Calculate the aggregate. Only if calculation is unavailable, request manual measurement.",
            "Cancel the measurement alternative; report missing inputs instead.",
        ],
        vec![
            "Calculate the aggregate.",
            "If calculation is unavailable, request manual measurement. Preserve the source records.",
        ],
    ];
    for size in [1, 8, 64, 128] {
        let (_, mut retrieval) = fixture();
        let original = retrieval.candidates[0].clone();
        let contracts = [
            "Read available definitions, including unsaved definitions.",
            "Register a supplied definition and return its identifier. Preserve all existing entries.",
            "Process supplied records using a saved definition identifier.",
        ];
        retrieval.candidates = (0..size)
            .map(|i| {
                let mut candidate = original.clone();
                candidate.id = format!("candidate{i}");
                candidate.reference.capability = format!("operation{i}");
                candidate.document.operation.contract = contracts[i % contracts.len()].into();
                candidate
            })
            .collect();
        for turns in &histories {
            let intent = IntentProvenance::from_turns(turns.iter().map(|s| (*s).into())).unwrap();
            let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
            for batch in batches {
                let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
                let state = &body["state"];
                assert_eq!(state["outcome"], intent.selection_request());
                assert_eq!(state["discovery_need"], intent.current());
                assert!(state.get("intent_turns").is_none());
                assert_eq!(state["rule"], policy["rule"]);
                // Preserve ordered source text without inventing a structured
                // interpretation or synthesized alternatives.
                assert!(state["outcome"].is_string());
                assert!(state.get("conditional_alternatives").is_none());
                assert!(state.get("intent_interpretation").is_none());
                assert!(state.get("current_intent").is_none());
                // No inferred effective goal or fabricated readiness is injected.
                assert_eq!(state.as_object().unwrap().len(), 6);
                assert!(state["types"].is_object());
                assert!(state["type_refs"].is_object());
                let cards = state["capabilities"].as_object().unwrap();
                assert_eq!(cards.len(), 1);
                for card in cards.values() {
                    let capability = &card["capability"];
                    let name = capability["reference"]["capability"].as_str().unwrap();
                    let i: usize = name.strip_prefix("operation").unwrap().parse().unwrap();
                    assert_eq!(
                        capability["operation"]["contract"],
                        contracts[i % contracts.len()]
                    );
                }
            }
        }
    }
}

#[test]
fn contribution_selection_preserves_effect_limits_without_issuing_permission() {
    let contracts = [
        "Read measurements and return their aggregate; leaves stored measurements unchanged.",
        "Return the aggregate and permanently delete the input measurements.",
        "Send measurements to an external processor and return its aggregate.",
    ];
    for constraint in [
        "Readiness and permission have not been established.",
        "Preserve all measurements; do not delegate to an external processor.",
    ] {
        let (_, mut retrieval) = fixture();
        let original = retrieval.candidates[0].clone();
        retrieval.candidates = contracts
            .iter()
            .enumerate()
            .map(|(i, contract)| {
                let mut candidate = original.clone();
                candidate.id = format!("candidate{i}");
                candidate.reference.capability = format!("aggregate{i}");
                candidate.document.operation.contract = (*contract).into();
                candidate
            })
            .collect();
        let intent = IntentProvenance::from_turns([
            "Calculate the aggregate of the supplied measurements.".into(),
            constraint.into(),
        ])
        .unwrap();
        let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
        for batch in &batches {
            let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
            assert_eq!(body["state"]["discovery_need"], intent.current());
            for candidate in retrieval
                .candidates
                .iter()
                .filter(|candidate| batch.bindings.values().any(|id| id == &candidate.id))
            {
                let card = body["state"]["capabilities"]
                    .as_object()
                    .unwrap()
                    .values()
                    .find(|card| card["capability"]["reference"] == json!(candidate.reference))
                    .unwrap();
                assert_eq!(
                    card["capability"]["operation"],
                    json!(candidate.document.operation)
                );
            }
        }
        // A relevance receipt can expose a forbidden contributor. It neither
        // strips its effects nor creates execution authorization. Model accuracy
        // is measured separately; these supplied judgments test the host boundary.
        let mut pages = SelectionPages::new(batches);
        while let Some(batch) = pages.current() {
            let raw = response(batch, &vec![MatchChoice::Relevant; batch.bindings.len()]);
            pages.accept(&raw).unwrap();
        }
        let receipt = pages.finish(&retrieval).unwrap();
        assert_eq!(receipt.selected(&retrieval).len(), contracts.len());
        let wire = serde_json::to_value(&receipt).unwrap();
        assert_eq!(
            wire.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["matches"]
        );
        for judgment in wire["matches"].as_array().unwrap() {
            assert_eq!(judgment.as_object().unwrap().len(), 2);
            assert!(judgment.get("capability_id").is_some());
            assert_eq!(judgment["choice"], "relevant");
        }
        let mut forged = wire;
        forged["authorized"] = json!(true);
        assert!(serde_json::from_value::<CapabilityMatchReceipt>(forged).is_err());
    }
}
