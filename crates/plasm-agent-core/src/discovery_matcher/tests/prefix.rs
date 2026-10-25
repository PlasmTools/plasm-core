use super::scheduling::many_cards;
use super::*;

#[test]
fn local_packets_are_bounded_and_cover_each_original_candidate_once() {
    for count in [1, 2, 8, 32, 128] {
        let (intent, retrieval) = many_cards(count);
        let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
        assert_eq!(batches.len(), count);
        let mut seen = BTreeSet::new();
        for batch in batches {
            assert!(batch.body().len() <= witnesses::MAX_BYTES);
            let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
            assert_eq!(body["questions"].as_object().unwrap().len(), 1);
            assert_eq!(body["state"]["capabilities"].as_object().unwrap().len(), 1);
            assert_eq!(body["state"]["rule"], RUBRIC);
            for id in batch.bindings.values() {
                assert!(seen.insert(id.clone()));
            }
        }
        assert_eq!(seen.len(), count);
    }
}

#[test]
fn candidate_permutation_does_not_change_wire_or_cache_keys() {
    let (intent, mut retrieval) = many_cards(8);
    let before = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    retrieval.candidates.reverse();
    let after = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    for (a, b) in before.iter().zip(&after) {
        assert_eq!(a.body(), b.body());
        assert_eq!(a.cache_key(), b.cache_key());
        assert!(a
            .body()
            .contains("\"types\":{\"values\":{},\"records\":{}}"));
    }
}

#[test]
fn changed_local_contract_invalidates_its_packet_not_unrelated_packets() {
    let (intent, mut retrieval) = many_cards(8);
    let before = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    retrieval.candidates[0]
        .document
        .operation
        .contract
        .push_str(" Changed effect.");
    let after = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    let changed: Vec<_> = before
        .iter()
        .zip(&after)
        .filter(|(a, b)| a.cache_key() != b.cache_key())
        .collect();
    assert_eq!(changed.len(), 1);
    assert_eq!(
        changed[0].0.bindings.values().next().unwrap(),
        &retrieval.candidates[0].id
    );
    let revised = intent.derived("Revised discovery need.".into()).unwrap();
    let revision = issue_batches(RELEVANCE_MODEL, &revised, &retrieval).unwrap();
    assert!(after
        .iter()
        .zip(&revision)
        .all(|(a, b)| a.cache_key() != b.cache_key()));
}

#[tokio::test]
async fn provider_context_rejection_does_not_split_or_retry_evidence() {
    let (intent, retrieval) = fixture();
    let mut calls = 0;
    let result = judge_candidates(RELEVANCE_MODEL, &intent, &retrieval, |_| {
        calls += 1;
        async {
            Err(
                Box::new(crate::decision_transport::DecisionTransportError::ContextLimit)
                    as DecisionProviderError,
            )
        }
    })
    .await;
    assert!(matches!(
        result,
        Err(DiscoveryMatcherError::IndivisibleItem { .. })
    ));
    assert_eq!(calls, 1);
}
