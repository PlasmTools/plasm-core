//! Frozen wire and binding controls for the abstract discovery fixture.
use super::*;

#[test]
fn prepared_selection_preserves_packet_bytes_keys_and_bindings() {
    let (catalogs, retrieval) = super::reference_index::matrix();
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let intent =
        IntentProvenance::from_turns(["Prepare material while preserving originals.".into()])
            .unwrap();
    let prepared = prepare_selection(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    let control: Vec<_> = prepared
        .batches
        .iter()
        .map(|batch| {
            (
                batch.body.len(),
                content_hash(batch.body.as_bytes()),
                batch.cache_key.clone(),
                batch.bindings.clone(),
            )
        })
        .collect();
    assert_eq!(
        control,
        vec![
            (
                1721,
                "cc659d7800f0a819656a503acc8523683f86fb51fb060b5f7496745d4d335493".into(),
                "05e99c4bc610f1a576fb6c86d9935f60ff46f6b6b07d9c94a171381f802a071f".into(),
                BTreeMap::from([("q0".into(), "candidate0".into())])
            ),
            (
                4723,
                "d734ca107cd153d3142c6e1d8ca5f5b3952dab79822bdabc7d766f666a5ddce2".into(),
                "c958a13b0231d3de03feba57edc61c55bd01220d2e4274e0578e7f7ef919962a".into(),
                BTreeMap::from([
                    ("q1".into(), "candidate1".into()),
                    ("q2".into(), "candidate2".into())
                ])
            ),
            (
                2860,
                "74b85ba5972c01cc3e09d7d3585adec8483002b02f1cb119b562d93de9e3f210".into(),
                "91b489f75b104cc86cc42780ff4053e1c8209c4e3cd4e2c571ff3f063a102f50".into(),
                BTreeMap::from([("q3".into(), "candidate3".into())])
            ),
        ]
    );
}

#[test]
fn typed_preparation_matches_decoded_wire_controls_across_groupings() {
    let (catalogs, original) = super::reference_index::matrix();
    for text in [
        "Prepare material while preserving originals.",
        "Inspect \"quoted\" material; preserve the café and originals.",
    ] {
        let intent = IntentProvenance::from_turns([text.to_owned()]).unwrap();
        for count in 1..=original.candidates.len() {
            for reversed in [false, true] {
                let mut retrieval: RetrievalReceipt =
                    serde_json::from_value(serde_json::to_value(&original).unwrap()).unwrap();
                retrieval.candidates.truncate(count);
                if reversed {
                    retrieval.candidates.reverse();
                }
                let evidence = CatalogEvidence::from_catalogs(
                    &retrieval,
                    &catalogs,
                    &Default::default(),
                    &|_| true,
                )
                .unwrap();
                let locals =
                    prepare_local_selection(RELEVANCE_MODEL, &intent, &retrieval, &evidence)
                        .unwrap();
                let expected = request_packing::pack_fixture(
                    locals
                        .batches
                        .into_iter()
                        .map(request_packing::PreparedLocalBatch::into_issued)
                        .collect(),
                )
                .unwrap();
                let actual =
                    prepare_selection(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
                assert_eq!(actual.budget_retained, locals.budget_retained);
                assert_eq!(actual.batches.len(), expected.len());
                let mut actual_answers = Vec::new();
                let mut expected_answers = Vec::new();
                for (actual, expected) in actual.batches.iter().zip(&expected) {
                    assert_eq!(actual.body, expected.body);
                    assert_eq!(actual.cache_key, expected.cache_key);
                    assert_eq!(actual.bindings, expected.bindings);
                    let choices: Vec<_> = actual
                        .bindings
                        .keys()
                        .enumerate()
                        .map(|(index, _)| {
                            if index % 2 == 0 {
                                MatchChoice::Uncertain
                            } else {
                                MatchChoice::Relevant
                            }
                        })
                        .collect();
                    let raw = response(expected, &choices);
                    actual_answers.extend(decode_batch(actual, &raw).unwrap());
                    expected_answers.extend(decode_batch(expected, &raw).unwrap());
                }
                let actual_receipt = finish_prepared_selection(
                    actual_answers,
                    &retrieval,
                    &actual.batches,
                    actual.budget_retained.clone(),
                )
                .unwrap();
                let expected_receipt = finish_prepared_selection(
                    expected_answers,
                    &retrieval,
                    &expected,
                    locals.budget_retained,
                )
                .unwrap();
                assert_eq!(actual_receipt, expected_receipt);
            }
        }
    }
}

#[test]
#[ignore = "manual packet-construction timing; no provider calls"]
fn packet_construction_timing_control() {
    let (catalogs, retrieval) = super::reference_index::matrix();
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let intent =
        IntentProvenance::from_turns(["Prepare material while preserving originals.".into()])
            .unwrap();
    let start = std::time::Instant::now();
    for _ in 0..500 {
        std::hint::black_box(
            prepare_selection(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap(),
        );
    }
    eprintln!(
        "packet-construction: 500 iterations in {:?}",
        start.elapsed()
    );
}
