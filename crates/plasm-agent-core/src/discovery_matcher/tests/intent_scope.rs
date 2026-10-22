//! Mechanical delivery proofs; classifier recall is measured separately.
use super::*;

#[test]
fn outcome_and_need_preserve_revisions_constraints_and_unknown_state() {
    let policy: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../fixtures/discovery/possible-contribution-policy.json"
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
        let mut cache_keys = std::collections::BTreeSet::new();
        for turns in &histories {
            let intent = IntentProvenance::from_turns(turns.iter().map(|s| (*s).into())).unwrap();
            let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
            assert!(cache_keys.insert(batches[0].cache_key.clone()));
            for batch in batches {
                let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
                let state = &body["state"];
                assert_eq!(state["outcome"], turns[0]);
                assert_eq!(state["discovery_need"], *turns.last().unwrap());
                assert_eq!(state["intent_turns"], json!(turns));
                assert_eq!(state["rule"], policy["rule"]);
                assert_eq!(
                    state["intent_interpretation"],
                    policy["intent_interpretation"]
                );
                assert!(state.get("current_intent").is_none());
                // No inferred effective goal or fabricated readiness is injected.
                assert_eq!(state.as_object().unwrap().len(), 6);
                let cards = state["capabilities"].as_object().unwrap();
                assert_eq!(cards.len(), size);
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
