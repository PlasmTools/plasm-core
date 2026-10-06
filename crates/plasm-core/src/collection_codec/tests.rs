use super::*;
use proptest::prelude::*;

#[derive(Debug, PartialEq, Deserialize)]
struct RejectedJson;

impl Serialize for RejectedJson {
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("private-row-value"))
    }
}

fn frame_with_payload(payload: &[u8]) -> Vec<u8> {
    let mut frame = b"PCC\x01".to_vec();
    frame.extend_from_slice(&Sha256::digest(payload));
    frame.extend_from_slice(payload);
    frame
}

#[test]
fn identity_json_failures_preserve_typed_sources_without_rendering_values() {
    use std::error::Error;
    let failures = [
        CollectionIdentity::for_untyped_observation(&RejectedJson).unwrap_err(),
        identity(0).derived(&RejectedJson).unwrap_err(),
        CollectionIdentity::for_expression(&crate::CGS::new(), &RejectedJson, 0).unwrap_err(),
    ];
    for (index, fault) in failures.into_iter().enumerate() {
        let source = match &fault {
            CollectionFault::ObservationJson { source } if index == 0 => source,
            CollectionFault::DerivationJson { source } if index == 1 => source,
            CollectionFault::ExpressionJson { source } if index == 2 => source,
            other => panic!("unexpected fault: {other:?}"),
        };
        assert!(source.is_data());
        assert!(fault.source().is_some());
        assert!(!fault.to_string().contains("private-row-value"));
        assert!(matches!(
            fault.clone(),
            CollectionFault::ObservationJson { .. }
                | CollectionFault::DerivationJson { .. }
                | CollectionFault::ExpressionJson { .. }
        ));
    }
}

#[test]
fn frame_json_failures_preserve_sources_without_rendering_values() {
    use std::error::Error;
    let codec = RecordingCodec::<RejectedJson>::new();
    let collection = codec
        .record(identity(0), vec![RejectedJson], Observation::Literal)
        .unwrap();
    let fault = codec.encode(&collection).unwrap_err();
    assert!(matches!(&fault, CollectionFault::FrameEncodeJson { source } if source.is_data()));
    assert!(fault.source().is_some());
    assert!(!fault.to_string().contains("private-row-value"));

    let fault = RecordingCodec::<u8>::new()
        .decode(&frame_with_payload(b"{"), &identity(0))
        .unwrap_err();
    assert!(matches!(&fault, CollectionFault::FrameDecodeJson { source } if source.is_eof()));
    assert!(fault.source().is_some());
}

#[test]
fn frame_validation_rejects_specific_contract_violations() {
    let codec = RecordingCodec::<u8>::new();
    assert!(matches!(
        codec.decode(&[0; 35], &identity(0)),
        Err(CollectionFault::FrameTooShort { actual: 35 })
    ));
    let mut frame = frame_with_payload(b"{}");
    frame[3] = 2;
    assert!(matches!(
        codec.decode(&frame, &identity(0)),
        Err(CollectionFault::FrameHeader)
    ));
    frame[3] = 1;
    frame[4] ^= 1;
    assert!(matches!(
        codec.decode(&frame, &identity(0)),
        Err(CollectionFault::FrameDigestMismatch)
    ));
    let payload = serde_json::to_vec(&serde_json::json!({
        "identity": identity(0), "rows": [],
        "evidence": {"coverage": "unknown", "gaps": []},
    }))
    .unwrap();
    assert!(matches!(
        codec.decode(&frame_with_payload(&payload), &identity(0)),
        Err(CollectionFault::FrameEvidenceInconsistent)
    ));
}

fn identity(expression: u8) -> CollectionIdentity {
    CollectionIdentity {
        catalog: [7; 32],
        expression: [expression; 32],
        epoch: 12,
    }
}
fn observed(rows: Vec<u8>, hidden: usize) -> RecordedCollection<u8> {
    let decoded = rows.len() + hidden;
    RecordingCodec::new()
        .record(
            identity(0),
            rows,
            Observation::Exhausted {
                decoded,
                discarded: hidden,
            },
        )
        .unwrap()
}

// Independent finite oracle: apply ordinary sequence operations to the full
// source and every generated hidden suffix. Never calls evidence transfer code.
#[derive(Clone, Debug)]
enum Op {
    Filter(u8),
    Distinct,
    Take(usize),
    Identity,
}
fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..5).prop_map(Op::Filter),
        Just(Op::Distinct),
        (0usize..12).prop_map(Op::Take),
        Just(Op::Identity)
    ]
}
fn oracle(mut rows: Vec<u8>, ops: &[Op]) -> Vec<u8> {
    for op in ops {
        match op {
            Op::Filter(n) => rows.retain(|x| x != n),
            Op::Distinct => {
                let mut seen = std::collections::HashSet::new();
                rows.retain(|x| seen.insert(*x));
            }
            Op::Take(n) => rows.truncate(*n),
            Op::Identity => (),
        }
    }
    rows
}
fn execute(mut c: RecordedCollection<u8>, ops: &[Op]) -> RecordedCollection<u8> {
    let codec = RecordingCodec::new();
    for (i, op) in ops.iter().enumerate() {
        let retained: Vec<usize> = c
            .observed()
            .iter()
            .enumerate()
            .filter_map(|(i, x)| match op {
                Op::Filter(n) if x == n => None,
                _ => Some(i),
            })
            .collect();
        let transform = match op {
            Op::Filter(_) => Transform::Filter {
                retained: &retained,
                captures: &[],
            },
            Op::Distinct => Transform::Distinct,
            Op::Take(n) => Transform::Take(*n),
            Op::Identity => Transform::Identity,
        };
        c = codec
            .derive(identity(i as u8 + 1), &[&c], transform)
            .unwrap();
    }
    c
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn pipeline_completeness_is_sound_against_hidden_occurrence_oracle(
        visible in prop::collection::vec(0u8..5, 0..16),
        hidden in prop::collection::vec(0u8..5, 0..12),
        ops in prop::collection::vec(op_strategy(), 0..12),
    ) {
        let c = execute(observed(visible.clone(), hidden.len()), &ops);
        prop_assert_eq!(c.observed(), &oracle(visible.clone(), &ops));
        let mut full = visible; full.extend(hidden);
        if c.coverage() == ResultCoverage::Complete {
            prop_assert_eq!(c.observed(), &oracle(full, &ops));
        }
    }
    #[test]
    fn exact_inputs_produce_exact_results(
        rows in prop::collection::vec(any::<u8>(), 0..24),
        ops in prop::collection::vec(op_strategy(), 0..12),
    ) {
        let c = execute(observed(rows.clone(), 0), &ops);
        prop_assert_eq!(c.coverage(), ResultCoverage::Complete);
        prop_assert_eq!(c.observed(), &oracle(rows, &ops));
    }
    #[test]
    fn trusted_storage_roundtrip_preserves_demand_and_order(
        rows in prop::collection::vec(any::<u8>(), 0..64), hidden in 0usize..4,
        ops in prop::collection::vec(op_strategy(), 0..8), rounds in 1usize..8,
    ) {
        let codec = RecordingCodec::new();
        let original = execute(observed(rows, hidden), &ops);
        let mut restored = original.clone();
        for _ in 0..rounds {
            restored = codec.decode(&codec.encode(&restored).unwrap(), original.identity()).unwrap();
        }
        prop_assert_eq!(&restored, &original);
        match (codec.materialize(&restored, Demand::Whole), codec.materialize(&original, Demand::Whole)) {
            (Ok(actual), Ok(expected)) => prop_assert_eq!(actual, expected),
            (Err(CollectionFault::Incomplete { identity: actual, coverage: actual_coverage, gaps: actual_gaps }),
             Err(CollectionFault::Incomplete { identity: expected, coverage: expected_coverage, gaps: expected_gaps })) => {
                prop_assert_eq!(actual, expected);
                prop_assert_eq!(actual_coverage, expected_coverage);
                prop_assert_eq!(actual_gaps, expected_gaps);
            }
            _ => prop_assert!(false, "materialization outcomes differ"),
        }
    }
    #[test]
    fn corrupted_frames_never_decode(rows in prop::collection::vec(any::<u8>(), 0..64), position in any::<usize>(), mask in 1u8..=255) {
        let codec = RecordingCodec::new();let c = observed(rows, 0);
        let mut frame = codec.encode(&c).unwrap(); let i = position % frame.len(); frame[i] ^= mask;
        prop_assert!(codec.decode(&frame, c.identity()).is_err());
    }
    #[test]
    fn identity_and_epoch_cannot_be_reused(rows in prop::collection::vec(any::<u8>(), 0..32), epoch in 13u64..u64::MAX) {
        let codec = RecordingCodec::new();let c = observed(rows, 0);
        let mut wrong = identity(0); wrong.epoch = epoch;
        prop_assert!(matches!(codec.decode(&codec.encode(&c).unwrap(), &wrong), Err(CollectionFault::IdentityMismatch)), "expected typed collection fault");
        let derived = codec.derive(wrong, &[&c], Transform::Identity).unwrap();
        prop_assert_ne!(derived.identity(), c.identity());
        prop_assert_eq!(derived.coverage(), c.coverage());
    }
    #[test]
    fn concat_preserves_occurrences_and_known_omissions(a in prop::collection::vec(0u8..4, 0..16), b in prop::collection::vec(0u8..4, 0..16), missing in 1usize..8) {
        let codec = RecordingCodec::new();let left = observed(a.clone(), 0);let right = observed(b.clone(), missing);
        let output = codec.derive(identity(1), &[&left, &right], Transform::Concat).unwrap();
        let mut expected = a;expected.extend(b);
        prop_assert_eq!(output.observed(), &expected);
        prop_assert_eq!(output.coverage(), ResultCoverage::Partial);
        prop_assert!(codec.materialize(&output, Demand::Whole).is_err());
    }
    #[test]
    fn flatmap_preserves_child_order_and_duplicates(children in prop::collection::vec(prop::collection::vec(0u8..4, 0..8), 0..12)) {
        let codec = RecordingCodec::new();
        let parent = observed(vec![9; children.len()], 0);
        let recorded: Vec<_> = children.iter().enumerate().map(|(i,c)| codec.record(identity(i as u8 + 1), c.clone(), Observation::Literal).unwrap()).collect();
        let identities: Vec<_> = recorded.iter().map(|c|c.identity().clone()).collect();
        let mut inputs = vec![&parent];inputs.extend(recorded.iter());
        let output = codec.derive(identity(2), &inputs, Transform::FlatMap { children: &identities }).unwrap();
        prop_assert_eq!(output.observed(), &children.into_iter().flatten().collect::<Vec<_>>());
        prop_assert_eq!(output.coverage(), ResultCoverage::Complete);
    }
    #[test]
    fn embedded_declaration_requires_occurrence_conservation(rows in prop::collection::vec(any::<u8>(), 0..32), extra in 1usize..32) {
        let codec = RecordingCodec::new();let len=rows.len();
        prop_assert!(matches!(codec.record(identity(0), rows.clone(), Observation::Embedded {declared_exhaustive:true, observed:len+extra}), Err(CollectionFault::Conservation)), "expected typed collection fault");
        let c=codec.record(identity(0),rows,Observation::Embedded {declared_exhaustive:false,observed:len}).unwrap();
        prop_assert_eq!(c.coverage(),ResultCoverage::Unknown);
        prop_assert!(codec.materialize(&c,Demand::Whole).is_err());
    }
}

#[test]
fn filtering_omitted_rows_does_not_claim_proven_output_omissions() {
    let c = execute(observed(vec![1], 1), &[Op::Filter(2)]);
    assert_eq!(c.coverage(), ResultCoverage::Unknown);
    assert_eq!(oracle(vec![1, 2], &[Op::Filter(2)]), vec![1]);
}
#[test]
fn complete_empty_parent_and_unknown_empty_parent_are_different() {
    let codec = RecordingCodec::new();
    for (obs, expected) in [
        (Observation::Literal, ResultCoverage::Complete),
        (Observation::UnprovenPage, ResultCoverage::Unknown),
    ] {
        let parent = codec.record(identity(0), Vec::<u8>::new(), obs).unwrap();
        let result = codec
            .derive(
                identity(1),
                &[&parent],
                Transform::FlatMap { children: &[] },
            )
            .unwrap();
        assert_eq!(result.coverage(), expected);
    }
}
#[test]
fn trait_object_is_the_test_seam() {
    let codec: Box<dyn CollectionCodec<Row = u8>> = Box::new(RecordingCodec::new());
    let result = codec
        .record(identity(0), vec![2, 1, 2], Observation::Literal)
        .unwrap();
    assert_eq!(
        codec.materialize(&result, Demand::Whole).unwrap(),
        &[2, 1, 2]
    );
}

#[test]
fn same_count_child_substitution_is_rejected() {
    let codec = RecordingCodec::new();
    let parent = observed(vec![1, 1], 0);
    let a = codec
        .record(identity(1), vec![2], Observation::Literal)
        .unwrap();
    let b = codec
        .record(identity(2), vec![2], Observation::Literal)
        .unwrap();
    let declared = [identity(1), identity(2)];
    assert!(
        matches!(
            codec.derive(
                identity(3),
                &[&parent, &b, &a],
                Transform::FlatMap {
                    children: &declared
                }
            ),
            Err(CollectionFault::InputMismatch { index: 0 })
        ),
        "expected typed collection fault"
    );
}

#[test]
fn gaps_keep_the_producer_identity_after_derivation() {
    let codec = RecordingCodec::new();
    let source = codec
        .record(identity(1), vec![1], Observation::UnprovenPage)
        .unwrap();
    let output = codec
        .derive(identity(2), &[&source], Transform::Distinct)
        .unwrap();
    let error = codec.materialize(&output, Demand::Whole).unwrap_err();
    let CollectionFault::Incomplete {
        identity: actual_identity,
        coverage,
        gaps,
    } = error
    else {
        panic!("expected incomplete collection");
    };
    assert_eq!(
        actual_identity,
        identity(2).with_inputs([source.identity()])
    );
    assert_eq!(coverage, ResultCoverage::Unknown);
    assert_eq!(
        gaps,
        BTreeSet::from([EvidenceGap {
            source: identity(1),
            reason: Gap::UnprovenTermination,
        }])
    );
}

#[test]
fn typed_reference_storage_preserves_identity_occurrences() {
    let codec = RecordingCodec::<crate::Ref>::new();
    let rows = vec![
        crate::Ref::new("Item", "01"),
        crate::Ref::new("Item", "1"),
        crate::Ref::new("Item", "01"),
    ];
    let collection = codec
        .record(identity(0), rows.clone(), Observation::Literal)
        .unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), codec.encode(&collection).unwrap()).unwrap();
    let restored = codec
        .decode(&std::fs::read(file.path()).unwrap(), &identity(0))
        .unwrap();
    assert_eq!(codec.materialize(&restored, Demand::Whole).unwrap(), &rows);
}

#[test]
fn declaration_does_not_hide_decoded_occurrence_loss() {
    let codec = RecordingCodec::<u8>::new();
    assert!(
        matches!(
            codec.record(
                identity(0),
                vec![1],
                Observation::Exhausted {
                    decoded: 2,
                    discarded: 0
                }
            ),
            Err(CollectionFault::Conservation)
        ),
        "expected typed collection fault"
    );
    let capped = codec
        .record(
            identity(0),
            vec![1],
            Observation::Exhausted {
                decoded: 2,
                discarded: 1,
            },
        )
        .unwrap();
    assert_eq!(capped.coverage(), ResultCoverage::Partial);
    assert!(codec.materialize(&capped, Demand::Whole).is_err());
}

#[test]
fn uncertain_nonempty_parent_cannot_prove_empty_flattened_result() {
    let codec = RecordingCodec::<u8>::new();
    let parent = codec
        .record(identity(0), vec![1], Observation::MoreAvailable)
        .unwrap();
    let child = codec
        .record(identity(1), vec![], Observation::Literal)
        .unwrap();
    let output = codec
        .derive(
            identity(2),
            &[&parent, &child],
            Transform::FlatMap {
                children: &[identity(1)],
            },
        )
        .unwrap();
    assert!(output.observed().is_empty());
    assert_eq!(output.coverage(), ResultCoverage::Unknown);
}

#[test]
fn filter_record_cannot_reorder_duplicate_or_invent_occurrences() {
    let codec = RecordingCodec::new();
    let source = observed(vec![3, 3, 4], 0);
    for retained in [vec![1, 0], vec![0, 0], vec![3]] {
        assert!(
            matches!(
                codec.derive(
                    identity(1),
                    &[&source],
                    Transform::Filter {
                        retained: &retained,
                        captures: &[]
                    }
                ),
                Err(CollectionFault::Conservation)
            ),
            "expected typed collection fault"
        );
    }
    let kept = codec
        .derive(
            identity(1),
            &[&source],
            Transform::Filter {
                retained: &[0, 1],
                captures: &[],
            },
        )
        .unwrap();
    assert_eq!(kept.observed(), &[3, 3]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn unknown_source_cannot_be_promoted_by_nonzero_pipeline(
        rows in prop::collection::vec(0u8..5, 0..24),
        ops in prop::collection::vec(op_strategy(), 0..10),
    ) {
        let codec=RecordingCodec::new();
        let source=codec.record(identity(0), rows, Observation::UnprovenPage).unwrap();
        let output=execute(source, &ops);
        if !ops.iter().any(|op| matches!(op, Op::Take(0))) {
            prop_assert_ne!(output.coverage(),ResultCoverage::Complete);
            prop_assert!(codec.materialize(&output,Demand::Whole).is_err());
        }
    }
    #[test]
    fn incomplete_child_prevents_whole_flatmap_materialization(
        rows in prop::collection::vec(0u8..5, 0..24), known_missing in any::<bool>(),
    ) {
        let codec=RecordingCodec::new();let parent=observed(vec![1],0);
        let observation=if known_missing {Observation::MoreAvailable} else {Observation::UnprovenPage};
        let child=codec.record(identity(1),rows,observation).unwrap();
        let output=codec.derive(identity(2),&[&parent,&child],Transform::FlatMap {children:&[identity(1)]}).unwrap();
        prop_assert_ne!(output.coverage(),ResultCoverage::Complete);
        prop_assert!(codec.materialize(&output,Demand::Whole).is_err());
        prop_assert!(output.gaps().iter().all(|gap|gap.source==identity(1)));
    }
}

#[test]
fn filter_cannot_drop_or_substitute_collection_capture_evidence() {
    let codec = RecordingCodec::new();
    let source = observed(vec![1, 2], 0);
    let rhs = codec
        .record(identity(1), vec![2], Observation::UnprovenPage)
        .unwrap();
    let captures = [identity(1)];
    let filtered = codec
        .derive(
            identity(2),
            &[&source, &rhs],
            Transform::Filter {
                retained: &[0],
                captures: &captures,
            },
        )
        .unwrap();
    assert_eq!(filtered.coverage(), ResultCoverage::Unknown);
    assert!(codec.materialize(&filtered, Demand::Whole).is_err());
    assert!(
        matches!(
            codec.derive(
                identity(2),
                &[&source],
                Transform::Filter {
                    retained: &[0],
                    captures: &captures
                }
            ),
            Err(CollectionFault::Arity)
        ),
        "expected typed collection fault"
    );
    assert!(
        matches!(
            codec.derive(
                identity(2),
                &[&source, &source],
                Transform::Filter {
                    retained: &[0],
                    captures: &captures
                }
            ),
            Err(CollectionFault::InputMismatch { index: 0 })
        ),
        "expected typed collection fault"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn reorder_conserves_duplicate_occurrences_and_evidence(
        rows in prop::collection::vec(0u8..5, 0..60),
        hidden in 0usize..5,
    ) {
        let source = observed(rows.clone(), hidden);
        let mut positions: Vec<_> = (0..rows.len()).collect();
        positions.sort_by_key(|&i| rows[i]);
        let result = RecordingCodec::new().derive(
            identity(1), &[&source], Transform::Reorder { positions: &positions },
        ).unwrap();
        let mut expected = rows;
        expected.sort();
        prop_assert_eq!(result.observed(), expected.as_slice());
        prop_assert_eq!(result.coverage(), source.coverage());
        prop_assert_eq!(result.gaps(), source.gaps());
    }
}

#[test]
fn reorder_rejects_equal_value_occurrence_substitution() {
    let source = observed(vec![7, 7, 9], 0);
    let codec = RecordingCodec::new();
    for positions in [&[0, 0, 2][..], &[0, 2][..], &[0, 1, 3][..]] {
        assert!(
            matches!(
                codec.derive(identity(1), &[&source], Transform::Reorder { positions }),
                Err(CollectionFault::Conservation)
            ),
            "expected typed collection fault"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn occurrence_prefix_counts_only_valid_ordered_prefixes(
        expected in prop::collection::vec(0u8..6, 0..80),
        retained in 0usize..90,
    ) {
        let kept = retained.min(expected.len());
        let prefix = validate_occurrence_prefix(&expected, &expected[..kept]).unwrap();
        prop_assert_eq!(prefix.decoded(), expected.len());
        prop_assert_eq!(prefix.discarded(), expected.len() - kept);
    }
}

#[test]
fn occurrence_prefix_rejects_substitution_reordering_and_excess() {
    for actual in [vec![1, 2, 2], vec![2, 1, 1], vec![1, 1, 2, 2], vec![1, 2]] {
        assert!(
            matches!(
                validate_occurrence_prefix(&[1, 1, 2], &actual),
                Err(CollectionFault::Conservation)
            ),
            "expected typed collection fault"
        );
    }
    assert_eq!(
        validate_occurrence_prefix(&[1, 1, 2], &[1, 1])
            .unwrap()
            .discarded(),
        1
    );
}

#[test]
fn prefix_validation_is_available_through_the_codec_trait() {
    let codec: &dyn CollectionCodec<Row = u8> = &RecordingCodec::new();
    let expected = [1, 1, 2];
    let retained = [1, 1];
    let observation = codec
        .validate_prefix(&mut expected.iter(), &mut retained.iter())
        .unwrap();
    assert_eq!(observation.decoded(), 3);
    assert_eq!(observation.discarded(), 1);
    assert!(
        matches!(
            codec.validate_prefix(&mut expected.iter(), &mut [1, 2].iter()),
            Err(CollectionFault::Conservation)
        ),
        "expected typed collection fault"
    );
}

#[test]
fn derivation_shares_non_clone_payloads_and_keeps_storage_alive() {
    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Payload {
        key: u8,
        bytes: Vec<u8>,
    }
    let implementation = RecordingCodec::<Payload>::new();
    let codec: &dyn CollectionCodec<Row = Payload> = &implementation;
    let source = codec
        .record(
            identity(0),
            vec![
                Payload {
                    key: 1,
                    bytes: vec![7; 4096],
                },
                Payload {
                    key: 2,
                    bytes: vec![8; 4096],
                },
            ],
            Observation::Literal,
        )
        .unwrap();
    let selected = codec
        .derive(
            identity(1),
            &[&source],
            Transform::Filter {
                retained: &[1],
                captures: &[],
            },
        )
        .unwrap();
    assert!(std::ptr::eq(&source.observed()[1], &selected.observed()[0]));
    let reordered = codec
        .derive(
            identity(2),
            &[&source],
            Transform::Reorder { positions: &[1, 0] },
        )
        .unwrap();
    assert!(std::ptr::eq(
        &source.observed()[1],
        &reordered.observed()[0]
    ));
    let joined = codec
        .derive(identity(3), &[&selected, &selected], Transform::Concat)
        .unwrap();
    assert!(std::ptr::eq(&joined.observed()[0], &joined.observed()[1]));
    let cloned_record = joined.clone();
    assert!(std::ptr::eq(joined.gaps(), cloned_record.gaps()));
    assert!(std::ptr::eq(
        &joined.observed()[0],
        &cloned_record.observed()[0]
    ));
    drop(source);
    drop(selected);
    drop(reordered);
    drop(joined);
    assert_eq!(cloned_record.observed()[0].bytes.len(), 4096);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn derived_occurrences_borrow_original_payloads(
        rows in prop::collection::vec(any::<u64>(), 0..64),
        selection in prop::collection::vec(any::<bool>(), 0..64),
        take in 0usize..70,
    ) {
        let codec = RecordingCodec::new();
        let source = codec.record(identity(0), rows, Observation::Literal).unwrap();
        let retained: Vec<_> = (0..source.observed().len())
            .filter(|&i| selection.get(i).copied().unwrap_or(false)).collect();
        let selected = codec.derive(identity(1), &[&source], Transform::Filter {
            retained: &retained, captures: &[],
        }).unwrap();
        let taken = codec.derive(identity(2), &[&selected], Transform::Take(take)).unwrap();
        let joined = codec.derive(identity(3), &[&taken, &taken], Transform::Concat).unwrap();
        let expected: Vec<_> = retained.iter().take(take).chain(retained.iter().take(take)).copied().collect();
        prop_assert_eq!(joined.observed().len(), expected.len());
        for (output, &position) in joined.observed().iter().zip(&expected) {
            prop_assert!(std::ptr::eq(output, &source.observed()[position]));
        }
        // Identity cloning keeps the occurrence table as well as the payloads shared.
        let cloned = joined.clone();
        for (a, b) in joined.observed().iter().zip(cloned.observed()) {
            prop_assert!(std::ptr::eq(a, b));
        }
    }

    #[test]
    fn mapped_evaluations_do_not_erase_dependency_gaps(
        rows in prop::collection::vec(any::<u8>(), 0..30),
        hidden in 1usize..10,
    ) {
        let codec = RecordingCodec::new();
        let source = observed(rows.clone(), hidden);
        let mapped = codec.derive(identity(1), &[&source], Transform::Map { rows: rows.into() }).unwrap();
        prop_assert_eq!(mapped.coverage(), ResultCoverage::Partial);
        prop_assert_eq!(mapped.gaps(), source.gaps());
        let evaluated = codec.derive(identity(2), &[&source], Transform::Evaluate { rows: vec![0] }).unwrap();
        prop_assert_eq!(evaluated.coverage(), ResultCoverage::Unknown);
        prop_assert_eq!(evaluated.gaps(), source.gaps());
        prop_assert!(codec.materialize(&evaluated, Demand::Whole).is_err());
    }
}

#[test]
fn replace_allocates_only_changed_payload_and_preserves_evidence() {
    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct NonClone(u32);
    let codec = RecordingCodec::new();
    let source = codec
        .record(
            identity(0),
            vec![NonClone(1), NonClone(2), NonClone(3)],
            Observation::UnprovenPage,
        )
        .unwrap();
    let changed = codec
        .derive(
            identity(1),
            &[&source],
            Transform::Replace {
                index: 1,
                row: NonClone(9),
            },
        )
        .unwrap();
    assert!(std::ptr::eq(&changed.observed()[0], &source.observed()[0]));
    assert!(std::ptr::eq(&changed.observed()[2], &source.observed()[2]));
    assert_eq!(changed.observed()[1].0, 9);
    assert_eq!(source.observed()[1].0, 2);
    assert_eq!(changed.coverage(), source.coverage());
    assert_eq!(changed.gaps(), source.gaps());
    assert!(
        matches!(
            codec.derive(
                identity(1),
                &[&source],
                Transform::Replace {
                    index: 3,
                    row: NonClone(9)
                }
            ),
            Err(CollectionFault::Conservation)
        ),
        "expected typed collection fault"
    );
}

#[test]
fn map_validates_cardinality_and_capture_uncertainty() {
    let codec = RecordingCodec::new();
    let source = observed(vec![1, 2], 0);
    let capture = codec
        .record(identity(1), vec![3], Observation::UnprovenPage)
        .unwrap();
    assert!(
        matches!(
            codec.derive(
                identity(2),
                &[&source],
                Transform::Map {
                    rows: vec![9].into()
                }
            ),
            Err(CollectionFault::Conservation)
        ),
        "expected typed collection fault"
    );
    let mapped = codec
        .derive(
            identity(2),
            &[&source, &capture],
            Transform::Map {
                rows: vec![9, 9].into(),
            },
        )
        .unwrap();
    assert_eq!(mapped.coverage(), ResultCoverage::Unknown);
    assert_eq!(mapped.gaps(), capture.gaps());
}

#[test]
fn derived_identity_binds_ordered_catalogs_and_observation_epochs() {
    let codec = RecordingCodec::new();
    let a = codec
        .record(identity(0), vec![1], Observation::Literal)
        .unwrap();
    let mut other_catalog = identity(0);
    other_catalog.catalog = [8; 32];
    let b = codec
        .record(other_catalog, vec![1], Observation::Literal)
        .unwrap();
    let mut other_epoch = identity(0);
    other_epoch.epoch += 1;
    let c = codec
        .record(other_epoch, vec![1], Observation::Literal)
        .unwrap();
    let ab = codec
        .derive(identity(2), &[&a, &b], Transform::Concat)
        .unwrap();
    let ba = codec
        .derive(identity(2), &[&b, &a], Transform::Concat)
        .unwrap();
    let ac = codec
        .derive(identity(2), &[&a, &c], Transform::Concat)
        .unwrap();
    assert_ne!(ab.identity(), ba.identity());
    assert_ne!(ab.identity(), ac.identity());
    assert_eq!(ab.coverage(), ResultCoverage::Complete);
    assert!(
        matches!(
            codec.decode(&codec.encode(&ab).unwrap(), ba.identity()),
            Err(CollectionFault::IdentityMismatch)
        ),
        "expected typed collection fault"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn checkpoint_preserves_occurrences_and_authority_and_rejects_corruption(
        rows in prop::collection::vec(any::<u8>(), 0..80),
        omitted in 0usize..20,
        tamper_at in any::<usize>(),
    ) {
        let codec = RecordingCodec::new();
        let source = observed(rows.clone(), omitted);
        let checkpoint = CollectionCheckpoint::capture(&codec, &source).unwrap();
        let wire = serde_json::to_vec(&checkpoint).unwrap();
        let restored: CollectionCheckpoint = serde_json::from_slice(&wire).unwrap();
        let restored = restored.restore(&codec).unwrap();
        prop_assert_eq!(restored.observed().iter().copied().collect::<Vec<_>>(), rows);
        prop_assert_eq!(restored.identity(), source.identity());
        prop_assert_eq!(restored.gaps(), source.gaps());
        prop_assert_eq!(codec.materialize(&restored, Demand::Whole).is_ok(), omitted == 0);
        let mut broken = checkpoint;
        let index = tamper_at % broken.frame.len();
        broken.frame[index] ^= 1;
        prop_assert!(broken.restore(&codec).is_err());
    }
}

#[test]
fn arc_batch_selection_preserves_allocation_without_clone_bound() {
    struct Unclonable(u8);
    let owner: std::sync::Arc<[Unclonable]> = vec![Unclonable(1), Unclonable(2)].into();
    let view = SharedRows::from(owner.clone()).select([1, 0, 1]).unwrap();
    assert!(std::ptr::eq(&view[0], &owner[1]));
    assert!(std::ptr::eq(&view[0], &view[2]));
    assert_eq!(view[1].0, 1);
}
