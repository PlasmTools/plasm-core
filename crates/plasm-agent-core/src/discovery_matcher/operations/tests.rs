use super::*;

fn decision_catalogs() -> BTreeMap<String, plasm_core::CGS> {
    let mut catalogs = super::super::online::tests::fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let source = cgs.capabilities["source_read"].clone();
    for (name, description) in [
        ("inspect", "Inspect a parcel"),
        ("read", "Read the sensor"),
        ("send", "Send a notice"),
        ("reset", "Reset the sensor"),
    ] {
        let mut capability = source.clone();
        capability.name = name.into();
        capability.description = description.into();
        cgs.capabilities.insert(name.into(), capability);
    }
    catalogs
}

fn decision_fixture() -> (
    super::super::catalogue_search::CatalogueSearch,
    crate::discovery_store::DiscoveryAuthorization,
) {
    use plasm_core::prerequisites::DeploymentBindings;
    let catalogs = decision_catalogs();
    let allowed =
        crate::discovery_store::DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = super::super::catalogue_search::CatalogueSearch::new(
        &catalogs,
        &DeploymentBindings::default(),
        &allowed,
    )
    .unwrap();
    (index, allowed)
}

fn decision_scope(
    intent: &str,
    index: &super::super::catalogue_search::CatalogueSearch,
    authorization: &crate::discovery_store::DiscoveryAuthorization,
) -> MatrixScope {
    let permitted = index
        .candidates
        .keys()
        .filter(|r| authorization.permits(r))
        .cloned()
        .collect();
    MatrixScope::new(
        "session",
        "generation",
        &permitted,
        ExtractionRequest::new("fixture", intent)
            .unwrap()
            .identity(),
    )
    .unwrap()
}

fn issued(
    matrix: &OperationMatrix,
    pairs: BTreeSet<(OperationId, plasm_core::prerequisites::CapabilityRef)>,
) -> IssuedPairs {
    IssuedPairs {
        scope: matrix.scope.clone(),
        pairs,
    }
}

struct MockExtraction {
    response: String,
}

struct MockDecisionTransport {
    response: String,
}

#[derive(Default)]
struct MemoryDecisionCache(tokio::sync::Mutex<BTreeMap<String, String>>);

#[async_trait::async_trait]
impl crate::discovery_service::decision_checkpoint::DecisionCache for MemoryDecisionCache {
    type Owner = ();
    async fn claim(&self, _: &str) -> Result<Self::Owner, super::super::DecisionProviderError> {
        Ok(())
    }
    async fn load(&self, key: &str) -> Result<Option<String>, super::super::DecisionProviderError> {
        Ok(self.0.lock().await.get(key).cloned())
    }
    async fn save(
        &self,
        key: &str,
        raw: &str,
        _: &mut Self::Owner,
    ) -> Result<(), super::super::DecisionProviderError> {
        self.0.lock().await.insert(key.into(), raw.into());
        Ok(())
    }
}

#[tokio::test]
async fn operation_decisions_use_the_shared_validated_checkpoint() {
    use crate::discovery_service::decision_checkpoint::DecisionCheckpoint;
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Inspect the parcel.";
    let operations = OperationSet::from_excerpts(intent, &["Inspect the parcel".into()]).unwrap();
    let (index, authorization) = decision_fixture();
    let question = DecisionQuestion {
        operation: operations.operations()[0].id().clone(),
        capability: CapabilityRef {
            catalog: "matrix".into(),
            capability: "inspect".into(),
        },
    };
    let scope = decision_scope(intent, &index, &authorization);
    let packet = || {
        DecisionBatch::new(
            super::super::RELEVANCE_MODEL,
            intent,
            &operations,
            &scope,
            &authorization,
            &index,
            std::slice::from_ref(&question),
        )
        .unwrap()
    };
    let response = serde_json::json!({"model":super::super::RELEVANCE_MODEL,"provider":"OpenAI","answers":{"q1":{"type":"choice","choice":"relevant","confidence":1.0,"probabilities":{"relevant":1.0,"unrelated":0.0,"uncertain":0.0}}}}).to_string();
    let cache = MemoryDecisionCache::default();
    let locks = crate::discovery_scheduling::DecisionLocks::default();
    let checkpoint = DecisionCheckpoint::new(&cache, &locks);
    let received = checkpoint
        .receive(packet(), || async { Ok(response.clone()) })
        .await
        .unwrap();
    assert_eq!(received, response);
    checkpoint.persist().await.unwrap();
    let reused = checkpoint
        .receive(packet(), || async {
            panic!("validated cached decision must not call the provider again")
        })
        .await
        .unwrap();
    assert_eq!(reused, response);
    let poisoned = MemoryDecisionCache::default();
    let bad_packet = packet();
    poisoned
        .0
        .lock()
        .await
        .insert(bad_packet.cache_key().into(), "{}".into());
    let checkpoint = DecisionCheckpoint::new(&poisoned, &locks);
    assert!(
        checkpoint
            .receive(bad_packet, || async {
                panic!("corrupt cache must not trigger implicit replay")
            })
            .await
            .is_err()
    );
}

#[async_trait::async_trait]
impl plasm_runtime::provider_transport::ProviderTransport for MockDecisionTransport {
    async fn send(
        &self,
        body: &str,
        _: &mut plasm_runtime::provider_transport::AttemptRecorder<'_>,
    ) -> Result<
        plasm_runtime::provider_transport::ProviderResponse,
        plasm_runtime::provider_transport::ProviderTransportError,
    > {
        let packet: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(packet["model"], super::super::RELEVANCE_MODEL);
        assert!(packet["questions"].as_object().unwrap().len() <= 4);
        Ok(plasm_runtime::provider_transport::ProviderResponse {
            status: reqwest::StatusCode::OK,
            retry_after: None,
            body: self.response.clone(),
        })
    }
}

#[async_trait::async_trait]
impl plasm_runtime::provider_transport::ProviderTransport for MockExtraction {
    async fn send(
        &self,
        body: &str,
        _: &mut plasm_runtime::provider_transport::AttemptRecorder<'_>,
    ) -> Result<
        plasm_runtime::provider_transport::ProviderResponse,
        plasm_runtime::provider_transport::ProviderTransportError,
    > {
        let packet: serde_json::Value = serde_json::from_str(body).unwrap();
        let input: serde_json::Value =
            serde_json::from_str(packet["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert!(
            input["spans"]
                .as_array()
                .unwrap()
                .iter()
                .any(|span| span[1] == "Read the sensor and send a notice.")
        );
        Ok(plasm_runtime::provider_transport::ProviderResponse {
            status: reqwest::StatusCode::OK,
            retry_after: None,
            body: self.response.clone(),
        })
    }
}

#[tokio::test]
async fn provider_boundary_binds_operation_excerpts_to_the_request() {
    let response = serde_json::json!({"model":"fixture","choices":[{"finish_reason":"stop","message":{"content":serde_json::json!({"operations":["s0","s1"]}).to_string()}}]}).to_string();
    let extractor = ProviderOperationExtractor::new(
        "fixture",
        std::sync::Arc::new(MockExtraction { response }),
    );
    let operations = extractor
        .extract("Read the sensor and send a notice.")
        .await
        .unwrap();
    assert_eq!(operations.operations().len(), 2);
    assert!(
        operations
            .validate_revision("Read the sensor and send a notice.")
            .is_ok()
    );
    assert_eq!(
        operations.validate_revision("Read the sensor."),
        Err(OperationError::IntentMismatch)
    );
}

/// Independent synthetic fixture: a read and a notice are requested, while
/// a reset is an unrelated effect. Each bounded packet is checkpointed and
/// resumed; only the semantic provider's answers are scripted here.
#[tokio::test]
async fn operation_provider_and_bounded_decision_packets_resume_together() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Read the sensor and send a notice.";
    let response = serde_json::json!({"model":"fixture","choices":[{"finish_reason":"stop","message":{"content":serde_json::json!({"operations":["s0","s1"]}).to_string()}}]}).to_string();
    let extractor = ProviderOperationExtractor::new(
        "fixture",
        std::sync::Arc::new(MockExtraction { response }),
    );
    let operations = extractor.extract(intent).await.unwrap();
    let (index, authorization) = decision_fixture();
    let read = CapabilityRef {
        catalog: "matrix".into(),
        capability: "read".into(),
    };
    let reset = CapabilityRef {
        catalog: "matrix".into(),
        capability: "reset".into(),
    };
    let send = CapabilityRef {
        catalog: "matrix".into(),
        capability: "send".into(),
    };
    let allowed = index
        .candidates
        .keys()
        .filter(|r| authorization.permits(r))
        .cloned()
        .collect();
    let scope = decision_scope(intent, &index, &authorization);
    let mut matrix = OperationMatrix::new(
        scope.clone(),
        intent,
        operations.clone(),
        &allowed,
        vec![read.clone(), send.clone(), reset.clone()],
    )
    .unwrap();
    let mut turns = 0;
    loop {
        let pairs = matrix.next_packet(4).unwrap();
        if pairs.is_empty() {
            break;
        }
        turns += 1;
        let questions: Vec<_> = pairs
            .iter()
            .map(|(operation, capability)| DecisionQuestion {
                operation: operation.clone(),
                capability: capability.clone(),
            })
            .collect();
        let batch = DecisionBatch::new(
            super::super::RELEVANCE_MODEL,
            intent,
            &operations,
            &scope,
            &authorization,
            &index,
            &questions,
        )
        .unwrap();
        let answers: serde_json::Map<String, serde_json::Value> = batch.bindings.iter().map(|(key, (operation, capability))| {
                let relevant = (operation == operations.operations()[0].id() && capability == &read)
                    || (operation == operations.operations()[1].id() && capability == &send);
                let (choice, probability) = if relevant { ("relevant", [1.0, 0.0, 0.0]) } else { ("unrelated", [0.0, 1.0, 0.0]) };
                (key.clone(), serde_json::json!({"type":"choice","choice":choice,"confidence":1.0,"probabilities":{"relevant":probability[0],"unrelated":probability[1],"uncertain":probability[2]}}))
            }).collect();
        let raw = serde_json::json!({"model":super::super::RELEVANCE_MODEL,"provider":"OpenAI","answers":answers})
            .to_string();
        let judge = ProviderOperationJudge::new(std::sync::Arc::new(MockDecisionTransport {
            response: raw,
        }));
        let judgments = judge.judge(&batch).await.unwrap();
        matrix
            .record_packet(&batch.issued_pairs(), &judgments)
            .unwrap();
        let checkpoint = serde_json::to_string(&matrix.checkpoint()).unwrap();
        matrix = OperationMatrix::restore(
            &scope,
            intent,
            &allowed,
            serde_json::from_str(&checkpoint).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(turns, 2);
    assert!(
        matches!(matrix.progress(&read), Ok(CandidateProgress::Relevant { operations, pending, uncertain }) if operations == BTreeSet::from([operations_for_fixture(&matrix, 0)]) && pending.is_empty() && uncertain.is_empty())
    );
    assert!(
        matches!(matrix.progress(&send), Ok(CandidateProgress::Relevant { operations, pending, uncertain }) if operations == BTreeSet::from([operations_for_fixture(&matrix, 1)]) && pending.is_empty() && uncertain.is_empty())
    );
    assert_eq!(matrix.progress(&reset), Ok(CandidateProgress::Unrelated));
}

fn operations_for_fixture(matrix: &OperationMatrix, index: usize) -> OperationId {
    matrix.operations.operations()[index].id().clone()
}

#[test]
fn operation_recall_ranks_prioritize_questions_without_granting_relevance() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Read the sensor and reset the alarm.";
    let operations = OperationSet::from_excerpts(
        intent,
        &["Read the sensor".into(), "reset the alarm".into()],
    )
    .unwrap();
    let read = CapabilityRef {
        catalog: "matrix".into(),
        capability: "read".into(),
    };
    let reset = CapabilityRef {
        catalog: "matrix".into(),
        capability: "reset".into(),
    };
    let authorized = BTreeSet::from([read.clone(), reset.clone()]);
    let scope = MatrixScope::new(
        "session",
        "generation",
        &authorized,
        ExtractionRequest::new("fixture", intent)
            .unwrap()
            .identity(),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(
        scope,
        intent,
        operations.clone(),
        &authorized,
        vec![read.clone(), reset.clone()],
    )
    .unwrap();
    let read_operation = operations.operations()[0].id().clone();
    let reset_operation = operations.operations()[1].id().clone();
    matrix
        .set_recall_ranks(
            &authorized,
            BTreeMap::from([
                ((read_operation.clone(), read.clone()), 0),
                ((reset_operation.clone(), reset.clone()), 0),
                ((read_operation.clone(), reset.clone()), 5),
                ((reset_operation.clone(), read.clone()), 5),
            ]),
        )
        .unwrap();
    assert_eq!(
        matrix.next_packet(2).unwrap(),
        vec![(read_operation, read.clone()), (reset_operation, reset)]
    );
    assert!(matrix.directly_supported().is_empty());
    let denied = CapabilityRef {
        catalog: "other".into(),
        capability: "send".into(),
    };
    assert_eq!(
        matrix.set_recall_ranks(
            &authorized,
            BTreeMap::from([((operations.operations()[0].id().clone(), denied), 0)])
        ),
        Err(OperationError::InvalidCandidates)
    );
}

#[test]
fn recalled_pair_precedes_unrecalled_pair_after_other_operation_was_judged() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Inspect the archive. Find a replacement key.";
    let operations = OperationSet::from_excerpts(
        intent,
        &[
            "Inspect the archive".into(),
            "Find a replacement key".into(),
        ],
    )
    .unwrap();
    let first = CapabilityRef {
        catalog: "matrix".into(),
        capability: "inspect".into(),
    };
    let second = CapabilityRef {
        catalog: "matrix".into(),
        capability: "find_key".into(),
    };
    let authorized = BTreeSet::from([first.clone(), second.clone()]);
    let scope = MatrixScope::new(
        "session",
        "generation",
        &authorized,
        ExtractionRequest::new("fixture", intent)
            .unwrap()
            .identity(),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(
        scope,
        intent,
        operations.clone(),
        &authorized,
        vec![first.clone(), second.clone()],
    )
    .unwrap();
    let historical = operations.operations()[0].id().clone();
    let current = operations.operations()[1].id().clone();
    matrix
        .set_recall_ranks(
            &authorized,
            BTreeMap::from([((current.clone(), second.clone()), 0)]),
        )
        .unwrap();
    let earlier = (current.clone(), first.clone());
    matrix
        .record_packet(
            &issued(&matrix, BTreeSet::from([earlier.clone()])),
            &[OperationJudgment {
                operation: earlier.0,
                capability: earlier.1,
                contribution: DirectContribution::Unrelated,
            }],
        )
        .unwrap();
    assert_eq!(
        matrix.next_packet_for(&[second.clone()], 1).unwrap(),
        vec![(current, second)]
    );
    assert!(!matrix.answers.contains_key(&(historical, first)));
}

#[test]
fn uncovered_spans_distinguish_uncertain_from_affirmative_contributions() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Read the archive. Identify its owner.";
    let operations = OperationSet::from_excerpts(
        intent,
        &["Read the archive".into(), "Identify its owner".into()],
    )
    .unwrap();
    let candidate = CapabilityRef {
        catalog: "matrix".into(),
        capability: "inspect".into(),
    };
    let authorized = BTreeSet::from([candidate.clone()]);
    let scope = MatrixScope::new(
        "session",
        "generation",
        &authorized,
        ExtractionRequest::new("fixture", intent)
            .unwrap()
            .identity(),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(
        scope,
        intent,
        operations.clone(),
        &authorized,
        vec![candidate.clone()],
    )
    .unwrap();
    let pairs = matrix.next_packet(2).unwrap();
    let judgments: Vec<_> = pairs
        .iter()
        .enumerate()
        .map(|(i, (operation, capability))| OperationJudgment {
            operation: operation.clone(),
            capability: capability.clone(),
            contribution: if i == 0 {
                DirectContribution::Relevant
            } else {
                DirectContribution::Uncertain
            },
        })
        .collect();
    matrix
        .record_packet(
            &issued(&matrix, pairs.iter().cloned().collect()),
            &judgments,
        )
        .unwrap();
    assert_eq!(
        matrix.uncovered_operations().collect::<Vec<_>>(),
        vec!["Identify its owner"]
    );
}

#[test]
fn bounded_matrix_retains_unjudged_pairs_and_commits_packets_atomically() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Find the signed document and send it to the reviewer.";
    let operations = OperationSet::from_excerpts(
        intent,
        &[
            "Find the signed document".into(),
            "send it to the reviewer".into(),
        ],
    )
    .unwrap();
    let candidates: Vec<_> = ["find", "send", "delete"]
        .into_iter()
        .map(|name| CapabilityRef {
            catalog: "documents".into(),
            capability: name.into(),
        })
        .collect();
    let authorized = candidates.iter().cloned().collect();
    let scope = MatrixScope::new(
        "session",
        "generation",
        &authorized,
        ExtractionRequest::new("fixture", intent)
            .unwrap()
            .identity(),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(
        scope,
        intent,
        operations.clone(),
        &authorized,
        candidates.clone(),
    )
    .unwrap();
    let admitted = matrix.next_packet_for(&candidates[1..2], 4).unwrap();
    assert_eq!(admitted.len(), 2);
    assert!(
        admitted
            .iter()
            .all(|(_, candidate)| candidate == &candidates[1])
    );
    assert!(matches!(
        matrix.progress(&candidates[0]),
        Ok(CandidateProgress::Incomplete { .. })
    ));
    assert_eq!(
        matrix.next_packet_for(&[candidates[1].clone(), candidates[1].clone()], 4),
        Err(OperationError::InvalidCandidates)
    );
    let first = matrix.next_packet(4).unwrap();
    assert_eq!(first.len(), 4);
    let issued: BTreeSet<_> = first.iter().cloned().collect();
    let answers: Vec<_> = first
        .iter()
        .map(|(operation, capability)| OperationJudgment {
            operation: operation.clone(),
            capability: capability.clone(),
            contribution: DirectContribution::Unrelated,
        })
        .collect();
    assert_eq!(
        matrix.record_packet(&self::issued(&matrix, issued.clone()), &answers[..3]),
        Err(OperationError::IncompleteJudgments)
    );
    assert_eq!(
        matrix.next_packet(4).unwrap(),
        first,
        "failed packet must not change work"
    );
    matrix
        .record_packet(&self::issued(&matrix, issued.clone()), &answers)
        .unwrap();
    assert_eq!(
        matrix.record_packet(&self::issued(&matrix, issued.clone()), &answers),
        Err(OperationError::AlreadyJudged)
    );
    let second = matrix.next_packet(4).unwrap();
    assert_eq!(second.len(), 2);
    assert!(second.iter().all(|pair| !issued.contains(pair)));
    let remaining: BTreeSet<_> = second.iter().cloned().collect();
    let more: Vec<_> = second
        .iter()
        .map(|(operation, capability)| OperationJudgment {
            operation: operation.clone(),
            capability: capability.clone(),
            contribution: DirectContribution::Unrelated,
        })
        .collect();
    matrix
        .record_packet(&self::issued(&matrix, remaining), &more)
        .unwrap();
    assert!(matrix.next_packet(4).unwrap().is_empty());
    assert!(
        candidates
            .iter()
            .all(|candidate| matrix.progress(candidate).unwrap() == CandidateProgress::Unrelated)
    );
}

#[test]
fn incomplete_or_uncertain_work_is_never_an_unrelated_judgment() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Read temperature and humidity.";
    let operations =
        OperationSet::from_excerpts(intent, &["temperature".into(), "humidity".into()]).unwrap();
    let candidate = CapabilityRef {
        catalog: "sensors".into(),
        capability: "read".into(),
    };
    let allowed = BTreeSet::from([candidate.clone()]);
    let scope = MatrixScope::new(
        "session",
        "generation",
        &allowed,
        ExtractionRequest::new("fixture", intent)
            .unwrap()
            .identity(),
    )
    .unwrap();
    let mut matrix =
        OperationMatrix::new(scope, intent, operations, &allowed, vec![candidate.clone()]).unwrap();
    let pair = matrix.next_packet(1).unwrap().pop().unwrap();
    let answer = OperationJudgment {
        operation: pair.0.clone(),
        capability: pair.1.clone(),
        contribution: DirectContribution::Unrelated,
    };
    matrix
        .record_packet(&issued(&matrix, BTreeSet::from([pair])), &[answer])
        .unwrap();
    assert!(
        matches!(matrix.progress(&candidate), Ok(CandidateProgress::Incomplete { pending, uncertain }) if pending.len() == 1 && uncertain.is_empty())
    );
    let pair = matrix.next_packet(1).unwrap().pop().unwrap();
    let answer = OperationJudgment {
        operation: pair.0.clone(),
        capability: pair.1.clone(),
        contribution: DirectContribution::Uncertain,
    };
    matrix
        .record_packet(&issued(&matrix, BTreeSet::from([pair])), &[answer])
        .unwrap();
    assert!(
        matches!(matrix.progress(&candidate), Ok(CandidateProgress::Uncertain { operations }) if operations.len() == 1)
    );
}

#[test]
fn only_evidenced_direct_contributions_become_ready() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Inspect the parcel and send a receipt.";
    let operations = OperationSet::from_excerpts(
        intent,
        &["Inspect the parcel".into(), "send a receipt".into()],
    )
    .unwrap();
    let inspect = CapabilityRef {
        catalog: "parcels".into(),
        capability: "inspect".into(),
    };
    let send = CapabilityRef {
        catalog: "messages".into(),
        capability: "send".into(),
    };
    let allowed = BTreeSet::from([inspect.clone(), send.clone()]);
    let scope = MatrixScope::new(
        "session",
        "generation",
        &allowed,
        ExtractionRequest::new("fixture", intent)
            .unwrap()
            .identity(),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(
        scope,
        intent,
        operations,
        &allowed,
        vec![inspect.clone(), send],
    )
    .unwrap();
    assert!(matrix.directly_supported().is_empty());
    let pair = matrix.next_packet(1).unwrap().pop().unwrap();
    assert_eq!(pair.1, inspect);
    let answer = OperationJudgment {
        operation: pair.0.clone(),
        capability: pair.1.clone(),
        contribution: DirectContribution::Relevant,
    };
    matrix
        .record_packet(&issued(&matrix, BTreeSet::from([pair])), &[answer])
        .unwrap();
    assert_eq!(matrix.directly_supported(), vec![inspect]);
}

#[test]
fn checkpoint_revalidates_intent_authorization_and_answer_identity() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Measure humidity and notify the operator.";
    let operations = OperationSet::from_excerpts(
        intent,
        &["Measure humidity".into(), "notify the operator".into()],
    )
    .unwrap();
    let candidate = CapabilityRef {
        catalog: "sensors".into(),
        capability: "measure".into(),
    };
    let authorized = BTreeSet::from([candidate.clone()]);
    let scope = MatrixScope::new(
        "session",
        "generation",
        &authorized,
        ExtractionRequest::new("fixture", intent)
            .unwrap()
            .identity(),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(
        scope.clone(),
        intent,
        operations,
        &authorized,
        vec![candidate.clone()],
    )
    .unwrap();
    let pair = matrix.next_packet(1).unwrap().pop().unwrap();
    let answer = OperationJudgment {
        operation: pair.0.clone(),
        capability: pair.1.clone(),
        contribution: DirectContribution::Relevant,
    };
    matrix
        .record_packet(
            &issued(&matrix, BTreeSet::from([pair])),
            std::slice::from_ref(&answer),
        )
        .unwrap();
    let serialized = serde_json::to_string(&matrix.checkpoint()).unwrap();
    let checkpoint: MatrixCheckpoint = serde_json::from_str(&serialized).unwrap();
    let restored =
        OperationMatrix::restore(&scope, intent, &authorized, checkpoint.clone()).unwrap();
    assert_eq!(restored.progress(&candidate), matrix.progress(&candidate));
    assert!(matches!(
        OperationMatrix::restore(
            &scope,
            "Notify the operator.",
            &authorized,
            checkpoint.clone()
        ),
        Err(OperationError::IntentMismatch)
    ));
    assert!(matches!(
        OperationMatrix::restore(&scope, intent, &BTreeSet::new(), checkpoint.clone()),
        Err(OperationError::ScopeMismatch)
    ));
    let other_session = MatrixScope::new(
        "other-session",
        "generation",
        &authorized,
        ExtractionRequest::new("fixture", intent)
            .unwrap()
            .identity(),
    )
    .unwrap();
    assert!(matches!(
        OperationMatrix::restore(&other_session, intent, &authorized, checkpoint.clone()),
        Err(OperationError::ScopeMismatch)
    ));
    let other_generation = MatrixScope::new(
        "session",
        "other-generation",
        &authorized,
        ExtractionRequest::new("fixture", intent)
            .unwrap()
            .identity(),
    )
    .unwrap();
    assert!(matches!(
        OperationMatrix::restore(&other_generation, intent, &authorized, checkpoint.clone()),
        Err(OperationError::ScopeMismatch)
    ));
    let other_extractor = MatrixScope::new(
        "session",
        "generation",
        &authorized,
        ExtractionRequest::new("other-model", intent)
            .unwrap()
            .identity(),
    )
    .unwrap();
    assert!(matches!(
        OperationMatrix::restore(&other_extractor, intent, &authorized, checkpoint.clone()),
        Err(OperationError::ScopeMismatch)
    ));
    let mismatched_request =
        ExtractionRequest::new("fixture", "Measure a different item.").unwrap();
    let mismatched_scope = MatrixScope::new(
        "session",
        "generation",
        &authorized,
        mismatched_request.identity(),
    )
    .unwrap();
    let operations = OperationSet::from_excerpts(intent, &["Measure humidity".into()]).unwrap();
    assert!(matches!(
        OperationMatrix::new(
            mismatched_scope,
            intent,
            operations,
            &authorized,
            vec![candidate.clone()]
        ),
        Err(OperationError::ScopeMismatch)
    ));
    let mut forged = checkpoint;
    forged.answers.push(answer);
    assert!(matches!(
        OperationMatrix::restore(&scope, intent, &authorized, forged),
        Err(OperationError::DuplicateJudgment)
    ));
}

#[test]
fn candidate_expansion_preserves_answers_and_rechecks_authorization() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Inspect the parcel.";
    let operations = OperationSet::from_excerpts(intent, &["Inspect the parcel".into()]).unwrap();
    let first = CapabilityRef {
        catalog: "parcels".into(),
        capability: "inspect".into(),
    };
    let later = CapabilityRef {
        catalog: "parcels".into(),
        capability: "read".into(),
    };
    let allowed = BTreeSet::from([first.clone(), later.clone()]);
    let scope = MatrixScope::new(
        "session",
        "generation",
        &allowed,
        ExtractionRequest::new("fixture", intent)
            .unwrap()
            .identity(),
    )
    .unwrap();
    let mut matrix =
        OperationMatrix::new(scope, intent, operations, &allowed, vec![first.clone()]).unwrap();
    let pair = matrix.next_packet(1).unwrap().pop().unwrap();
    let answer = OperationJudgment {
        operation: pair.0.clone(),
        capability: pair.1.clone(),
        contribution: DirectContribution::Relevant,
    };
    matrix
        .record_packet(&issued(&matrix, BTreeSet::from([pair])), &[answer])
        .unwrap();
    assert_eq!(
        matrix.add_candidates(
            &BTreeSet::from([later.clone()]),
            std::slice::from_ref(&later)
        ),
        Err(OperationError::ScopeMismatch)
    );
    assert!(matrix.next_packet(4).unwrap().is_empty());
    matrix
        .add_candidates(&allowed, &[first.clone(), later.clone(), later.clone()])
        .unwrap();
    assert!(matches!(
        matrix.progress(&first),
        Ok(CandidateProgress::Relevant { pending, .. }) if pending.is_empty()
    ));
    assert!(
        matches!(matrix.progress(&later), Ok(CandidateProgress::Incomplete { pending, .. }) if pending.len() == 1)
    );
    assert_eq!(matrix.next_packet(4).unwrap().len(), 1);
}

#[test]
fn operation_identity_is_revision_bound_and_proposals_are_grounded() {
    let intent = "Find the record and send it to the reviewer.";
    let operations = OperationSet::from_excerpts(
        intent,
        &["Find the record".into(), "send it to the reviewer".into()],
    )
    .unwrap();
    assert_eq!(operations.operations().len(), 2);
    assert_ne!(
        operations.operations()[0].id(),
        operations.operations()[1].id()
    );
    assert_eq!(operations.validate_revision(intent), Ok(()));
    assert_eq!(
        operations.validate_revision("Find the record."),
        Err(OperationError::IntentMismatch)
    );
    assert_eq!(
        OperationSet::from_excerpts(intent, &["delete the record".into()]),
        Err(OperationError::InvalidExcerpt)
    );
    assert_eq!(
        OperationSet::from_excerpts(
            intent,
            &["Find the record".into(), "Find the record".into()]
        ),
        Err(OperationError::DuplicateExcerpt)
    );
}

#[test]
fn forged_deserialized_identity_is_rejected() {
    let intent = "Measure humidity.";
    let mut value = serde_json::to_value(
        OperationSet::from_excerpts(intent, &["Measure humidity".into()]).unwrap(),
    )
    .unwrap();
    value["operations"][0]["id"] = "forged".into();
    let decoded: OperationSet = serde_json::from_value(value).unwrap();
    assert_eq!(
        decoded.validate_revision(intent),
        Err(OperationError::InvalidExcerpt)
    );
}

#[test]
fn provider_answers_cannot_escape_or_incompletely_cover_issued_questions() {
    use plasm_core::prerequisites::CapabilityRef;
    let operations =
        OperationSet::from_excerpts("Inspect and ship.", &["Inspect".into(), "ship".into()])
            .unwrap();
    let inspect = CapabilityRef {
        catalog: "fixture".into(),
        capability: "inspect".into(),
    };
    let ship = CapabilityRef {
        catalog: "fixture".into(),
        capability: "ship".into(),
    };
    let first = OperationJudgment {
        operation: operations.operations()[0].id().clone(),
        capability: inspect.clone(),
        contribution: DirectContribution::Relevant,
    };
    let second = OperationJudgment {
        operation: operations.operations()[1].id().clone(),
        capability: ship,
        contribution: DirectContribution::Uncertain,
    };
    let issued = BTreeSet::from([
        (first.operation.clone(), inspect),
        (second.operation.clone(), second.capability.clone()),
    ]);
    assert_eq!(
        validate_judgments(&issued, &[first.clone(), second]),
        Ok(())
    );
    assert_eq!(
        validate_judgments(&issued, std::slice::from_ref(&first)),
        Err(OperationError::IncompleteJudgments)
    );
    let mut forged = first.clone();
    forged.capability.catalog = "other-tenant-catalogue".into();
    assert_eq!(
        validate_judgments(&issued, &[forged]),
        Err(OperationError::UnknownQuestion)
    );
    let foreign_operation =
        OperationSet::from_excerpts("Erase the item.", &["Erase".into()]).unwrap();
    let mut forged = first.clone();
    forged.operation = foreign_operation.operations()[0].id().clone();
    assert_eq!(
        validate_judgments(&issued, &[forged]),
        Err(OperationError::UnknownQuestion)
    );
    assert_eq!(
        validate_judgments(&issued, &[first.clone(), first]),
        Err(OperationError::DuplicateJudgment)
    );
}

#[test]
fn extraction_contract_distinguishes_discovery_from_the_requested_action() {
    for intent in [
        "Find capabilities to read the sensor and reset the alarm.",
        "List available capabilities for this catalogue.",
    ] {
        for corrective in [false, true] {
            let request = ExtractionRequest::build("fixture", intent, corrective).unwrap();
            let body: serde_json::Value = serde_json::from_str(request.body()).unwrap();
            let instruction = body["messages"][0]["content"].as_str().unwrap();
            assert!(instruction.contains("listing capabilities is itself the actual task"));
            let input: serde_json::Value =
                serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            assert!(
                input["spans"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|span| span[1] == intent)
            );
        }
    }
}

#[test]
fn extraction_rejects_invented_text_and_truncated_responses() {
    let intent = "Find the report and send it.";
    let request = ExtractionRequest::new("fixture", intent).unwrap();
    let response = |finish, operations: Vec<&str>| {
        serde_json::json!({"model":"fixture","choices":[{"finish_reason":finish,"message":{"content":serde_json::json!({"operations":operations}).to_string()}}]}).to_string()
    };
    assert!(
        request
            .decode(intent, &response("stop", vec!["s0", "s1"]))
            .is_ok()
    );
    assert_eq!(
        request.decode(intent, &response("stop", vec!["delete it"])),
        Err(OperationError::InvalidExcerpt)
    );
    assert_eq!(
        request.decode(intent, &response("length", vec!["s0"])),
        Err(OperationError::InvalidExtractionResponse)
    );
}

#[test]
fn extraction_selects_host_issued_intent_spans() {
    let intent = "Read the account; send £25 only if approved.";
    let request = ExtractionRequest::new("fixture", intent).unwrap();
    let body: serde_json::Value = serde_json::from_str(request.body()).unwrap();
    let spans = body["messages"][1]["content"].as_str().unwrap();
    assert!(spans.contains("Read the account"));
    assert!(spans.contains("send £25 only if approved"));
    let response = |ids: Vec<&str>| {
        serde_json::json!({"model":"fixture","choices":[{"finish_reason":"stop","message":{"content":serde_json::json!({"operations":ids}).to_string()}}]}).to_string()
    };
    let result = request.decode(intent, &response(vec!["s0", "s1"])).unwrap();
    assert!(
        result
            .operations()
            .iter()
            .any(|v| v.text().contains("send £25 only if approved"))
    );
    assert!(request.decode(intent, &response(vec!["invented"])).is_err());
    assert!(request.decode(intent, &response(vec!["s0", "s0"])).is_err());
}

#[test]
fn issued_spans_are_exact_utf8_bounded_and_keep_decimal_amounts() {
    let intent = format!(
        "Review café invoice £25.50; {} and do not pay without approval.",
        "α".repeat(400)
    );
    let spans = source_spans(&intent).unwrap();
    assert!(spans.values().all(|span| !span.is_empty()
        && span.len() <= MAX_OPERATION_BYTES
        && intent.contains(span)));
    assert!(spans.values().any(|span| span.contains("£25.50")));
    assert!(
        spans
            .values()
            .any(|span| span.contains("do not pay without approval"))
    );
}

#[test]
fn extraction_preserves_original_currency_and_guards() {
    let intent = "Pay  £25 to Karen only if approved.";
    let request = ExtractionRequest::new("fixture", intent).unwrap();
    let response = |operation: &str| {
        serde_json::json!({"model":"fixture","choices":[{"finish_reason":"stop","message":{"content":serde_json::json!({"operations":[operation]}).to_string()}}]}).to_string()
    };
    let operations = request.decode(intent, &response("s0")).unwrap();
    assert_eq!(operations.operations()[0].text(), intent);
    assert_eq!(
        request.decode(intent, &response("Pay $25 to Karen")),
        Err(OperationError::InvalidExcerpt)
    );
    assert_eq!(
        request.decode(intent, &response("Pay £25 to Karen if not approved")),
        Err(OperationError::InvalidExcerpt)
    );
}

#[tokio::test]
async fn malformed_semantic_proposal_gets_one_corrective_request() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct CorrectingTransport(AtomicUsize);
    #[async_trait::async_trait]
    impl plasm_runtime::provider_transport::ProviderTransport for CorrectingTransport {
        async fn send(
            &self,
            body: &str,
            _: &mut plasm_runtime::provider_transport::AttemptRecorder<'_>,
        ) -> Result<
            plasm_runtime::provider_transport::ProviderResponse,
            plasm_runtime::provider_transport::ProviderTransportError,
        > {
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            if n == 1 {
                assert!(body.contains("preceding proposal"));
                let request: serde_json::Value = serde_json::from_str(body).unwrap();
                assert!(
                    request["messages"][2]["content"]
                        .as_str()
                        .unwrap()
                        .contains("Invent an action")
                );
                assert!(
                    request["messages"][3]["content"]
                        .as_str()
                        .unwrap()
                        .contains("not an exact intent excerpt")
                );
            }
            let operation = if n == 0 { "Invent an action" } else { "s0" };
            Ok(plasm_runtime::provider_transport::ProviderResponse { status: reqwest::StatusCode::OK, retry_after: None, body: serde_json::json!({"model":"fixture","choices":[{"finish_reason":"stop","message":{"content":serde_json::json!({"operations":[operation]}).to_string()}}]}).to_string() })
        }
    }
    let transport = std::sync::Arc::new(CorrectingTransport(AtomicUsize::new(0)));
    let extractor = ProviderOperationExtractor::new("fixture", transport.clone());
    let operations = extractor.extract("Read the record.").await.unwrap();
    assert_eq!(operations.operations()[0].text(), "Read the record.");
    assert_eq!(transport.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn repeated_ungrounded_proposals_fail_after_the_single_correction() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct InvalidTransport(AtomicUsize);
    #[async_trait::async_trait]
    impl plasm_runtime::provider_transport::ProviderTransport for InvalidTransport {
        async fn send(
            &self,
            _: &str,
            _: &mut plasm_runtime::provider_transport::AttemptRecorder<'_>,
        ) -> Result<
            plasm_runtime::provider_transport::ProviderResponse,
            plasm_runtime::provider_transport::ProviderTransportError,
        > {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(plasm_runtime::provider_transport::ProviderResponse { status: reqwest::StatusCode::OK, retry_after: None, body: serde_json::json!({"model":"fixture","choices":[{"finish_reason":"stop","message":{"content":serde_json::json!({"operations":["Invent an action"]}).to_string()}}]}).to_string() })
        }
    }
    let transport = std::sync::Arc::new(InvalidTransport(AtomicUsize::new(0)));
    let extractor = ProviderOperationExtractor::new("fixture", transport.clone());
    assert!(matches!(
        extractor.extract("Read the record.").await,
        Err(ExtractionError::Contract(OperationError::InvalidExcerpt))
    ));
    assert_eq!(transport.0.load(Ordering::SeqCst), 2);
}

#[test]
fn decision_packet_binds_authorized_candidate_and_exact_operation() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Inspect the parcel and send a receipt.";
    let operations = OperationSet::from_excerpts(
        intent,
        &["Inspect the parcel".into(), "send a receipt".into()],
    )
    .unwrap();
    let (index, authorization) = decision_fixture();
    let capability = CapabilityRef {
        catalog: "matrix".into(),
        capability: "inspect".into(),
    };
    let question = DecisionQuestion {
        operation: operations.operations()[0].id().clone(),
        capability: capability.clone(),
    };
    let scope = decision_scope(intent, &index, &authorization);
    let batch = DecisionBatch::new(
        super::super::RELEVANCE_MODEL,
        intent,
        &operations,
        &scope,
        &authorization,
        &index,
        std::slice::from_ref(&question),
    )
    .unwrap();
    let wire: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
    assert_eq!(wire["state"]["operations"]["o1"], "Inspect the parcel");
    assert!(
        wire["questions"]["q1"]["instructions"]
            .as_str()
            .unwrap()
            .contains("necessary component")
    );
    assert!(
        wire["state"]["capabilities"]["c1"]["operation"]["contract"]
            .as_str()
            .unwrap()
            .starts_with("Inspect a parcel")
    );
    assert!(
        !batch.body().contains(question.operation.as_str()),
        "provider receives compact labels, not internal digests"
    );
    let response = serde_json::json!({"model":super::super::RELEVANCE_MODEL,"provider":"OpenAI","answers":{"q1":{"type":"choice","choice":"relevant","confidence":1.0,"probabilities":{"relevant":1.0,"unrelated":0.0,"uncertain":0.0}}}}).to_string();
    let judgments = batch.decode(&response).unwrap();
    assert_eq!(judgments[0].operation, question.operation);
    assert_eq!(judgments[0].capability, capability);
    assert!(matches!(
        DecisionBatch::new(
            "openai/gpt-6-luna",
            intent,
            &operations,
            &scope,
            &authorization,
            &index,
            std::slice::from_ref(&question)
        ),
        Err(OperationError::UnsupportedDecisionModel)
    ));
    assert!(matches!(
        DecisionBatch::new(
            super::super::RELEVANCE_MODEL,
            intent,
            &operations,
            &scope,
            &crate::discovery_store::DiscoveryAuthorization::catalogs(BTreeSet::new()),
            &index,
            &[question]
        ),
        Err(OperationError::ScopeMismatch)
    ));
    assert_eq!(
        batch.decode("{}"),
        Err(OperationError::InvalidDecisionResponse)
    );
}

#[test]
fn issued_decisions_cannot_cross_session_boundaries() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Inspect the parcel.";
    let operations = OperationSet::from_excerpts(intent, &["Inspect the parcel".into()]).unwrap();
    let (index, authorization) = decision_fixture();
    let allowed: BTreeSet<_> = index
        .candidates
        .keys()
        .filter(|r| authorization.permits(r))
        .cloned()
        .collect();
    let first_scope = decision_scope(intent, &index, &authorization);
    let second_scope = MatrixScope::new(
        "another-session",
        "generation",
        &allowed,
        ExtractionRequest::new("fixture", intent)
            .unwrap()
            .identity(),
    )
    .unwrap();
    let reference = CapabilityRef {
        catalog: "matrix".into(),
        capability: "inspect".into(),
    };
    let question = DecisionQuestion {
        operation: operations.operations()[0].id().clone(),
        capability: reference.clone(),
    };
    let first = DecisionBatch::new(
        super::super::RELEVANCE_MODEL,
        intent,
        &operations,
        &first_scope,
        &authorization,
        &index,
        std::slice::from_ref(&question),
    )
    .unwrap();
    let second = DecisionBatch::new(
        super::super::RELEVANCE_MODEL,
        intent,
        &operations,
        &second_scope,
        &authorization,
        &index,
        &[question],
    )
    .unwrap();
    assert_ne!(first.cache_key(), second.cache_key());
    let mut matrix =
        OperationMatrix::new(first_scope, intent, operations, &allowed, vec![reference]).unwrap();
    assert_eq!(
        matrix.record_packet(&second.issued_pairs(), &[]),
        Err(OperationError::ScopeMismatch)
    );
}

#[test]
fn operation_packet_carries_the_connected_catalogue_witness() {
    use plasm_core::prerequisites::DeploymentBindings;
    let intent = "Inspect the parcel.";
    let operations = OperationSet::from_excerpts(intent, &["Inspect the parcel".into()]).unwrap();
    let catalogs = decision_catalogs();
    let deployments = DeploymentBindings::default();
    let (index, authorization) = decision_fixture();
    let provenance =
        crate::intent_provenance::IntentProvenance::from_turns([intent.to_string()]).unwrap();
    let visits = BTreeMap::new();
    let context = super::super::online::SearchContext {
        model: super::super::RELEVANCE_MODEL,
        intent: &provenance,
        generation: "generation",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &authorization,
        exposed: &[],
        visits: &visits,
    };
    let scope = decision_scope(intent, &index, &authorization);
    let edge = index
        .graph
        .edges()
        .find(|edge| edge.relationship == super::super::search_graph::Relationship::SuppliesInput)
        .unwrap()
        .clone();
    let question = DecisionQuestion {
        operation: operations.operations()[0].id().clone(),
        capability: edge.consumer.clone(),
    };
    let wave = vec![(edge.consumer.clone(), vec![edge])];
    let batch = DecisionBatch::from_graph(
        context,
        &operations,
        &scope,
        &index,
        &wave,
        std::slice::from_ref(&question),
    )
    .unwrap();
    let wire: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
    let witness = &wire["state"]["question_evidence"]["w1"];
    assert!(witness["capabilities"].as_object().unwrap().len() >= 2);
    assert!(!witness["bindings"].as_array().unwrap().is_empty());
    assert!(witness["types"].is_object());
    assert!(matches!(
        DecisionBatch::from_graph(context, &operations, &scope, &index, &[], &[question]),
        Err(OperationError::InvalidGraphWitness)
    ));
}

#[test]
fn relationship_scope_is_explicit_in_extraction_and_decision_packets() {
    let intent = "Update records for projects assigned to my team.";
    let extraction = ExtractionRequest::new("fixture", intent).unwrap();
    assert!(extraction.body().contains("membership conditions"));
    let operations = OperationSet::from_excerpts(intent, &[intent.into()]).unwrap();
    let (index, authorization) = decision_fixture();
    let scope = decision_scope(intent, &index, &authorization);
    let question = DecisionQuestion {
        operation: operations.operations()[0].id().clone(),
        capability: plasm_core::prerequisites::CapabilityRef {
            catalog: "matrix".into(),
            capability: "inspect".into(),
        },
    };
    let packet = DecisionBatch::new(
        super::super::RELEVANCE_MODEL,
        intent,
        &operations,
        &scope,
        &authorization,
        &index,
        &[question],
    )
    .unwrap();
    assert!(packet.body().contains("target-selection condition"));
}

#[test]
fn extraction_receives_ordered_conditions_and_does_not_offer_host_header() {
    let provenance = crate::intent_provenance::IntentProvenance::from_turns([
        "Update records belonging to the selected team.".into(),
        "Use the other team instead; preserve unrelated records.".into(),
    ])
    .unwrap();
    let request = provenance.selection_request();
    let extraction = ExtractionRequest::new("fixture", &request).unwrap();
    let packet: serde_json::Value = serde_json::from_str(extraction.body()).unwrap();
    let user = packet["messages"][1]["content"].as_str().unwrap();
    let data: serde_json::Value = serde_json::from_str(user).unwrap();
    assert!(data["spans"].to_string().contains("selected team"));
    assert!(data["spans"].to_string().contains("other team"));
    assert!(
        packet["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Do not select superseded requirements")
    );
    assert!(
        data["spans"]
            .as_array()
            .unwrap()
            .iter()
            .all(|span| !span[1].as_str().unwrap().starts_with("Ordered requests:"))
    );
}

#[test]
fn bounded_extraction_preserves_long_ordered_history_without_duplicate_request_copy() {
    let mut turns = vec!["Select records belonging to the original owner.".to_owned()];
    turns.extend((0..16).map(|i| format!("Turn detail {i}: Inspect documents and retain the ownership condition. Do not alter records in group {i}. Observation {i} has not established a qualifying match. Check source {i} before selecting a target and preserve identity through comparison. Continue step {i} only after the relevant inputs have been inspected and the matching relation is established.")));
    let provenance = crate::intent_provenance::IntentProvenance::from_turns(turns).unwrap();
    let request = provenance.selection_request();
    let extraction = ExtractionRequest::new("fixture", &request).unwrap();
    assert!(extraction.body().len() <= MAX_EXTRACTION_INPUT_BYTES);
    let wire: serde_json::Value = serde_json::from_str(extraction.body()).unwrap();
    let input: serde_json::Value =
        serde_json::from_str(wire["messages"][1]["content"].as_str().unwrap()).unwrap();
    let text = input.to_string();
    assert!(text.contains("original owner"));
    assert!(text.contains("Turn detail 15"));
}

#[test]
fn extraction_preserves_repeated_spans_in_temporal_order() {
    let provenance = crate::intent_provenance::IntentProvenance::from_turns([
        "Inspect records. Preserve the source.".into(),
        "Cancel inspection.".into(),
        "Resume inspection. Preserve the source.".into(),
    ])
    .unwrap();
    let extraction = ExtractionRequest::new("fixture", &provenance.selection_request()).unwrap();
    let wire: serde_json::Value = serde_json::from_str(extraction.body()).unwrap();
    let input: serde_json::Value =
        serde_json::from_str(wire["messages"][1]["content"].as_str().unwrap()).unwrap();
    let spans = input["spans"].as_array().unwrap();
    let texts: Vec<_> = spans.iter().map(|span| span[1].as_str().unwrap()).collect();
    let repeats: Vec<_> = texts
        .iter()
        .enumerate()
        .filter(|(_, text)| **text == "Preserve the source.")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(repeats.len(), 2);
    let cancel = texts
        .iter()
        .position(|text| text.contains("Cancel inspection"))
        .unwrap();
    assert!(repeats[0] < cancel && cancel < repeats[1]);
}

#[test]
fn oversized_canonical_contract_is_reported_as_budget_pressure() {
    use plasm_core::prerequisites::{CapabilityRef, DeploymentBindings};
    let mut catalogs = super::super::online::tests::fixture();
    catalogs
        .get_mut("matrix")
        .unwrap()
        .capabilities
        .get_mut("source_read")
        .unwrap()
        .description = "Detailed contract information. ".repeat(800);
    let authorization =
        crate::discovery_store::DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let index = super::super::catalogue_search::CatalogueSearch::new(
        &catalogs,
        &DeploymentBindings::default(),
        &authorization,
    )
    .unwrap();
    let intent = "Inspect the object.";
    let operations = OperationSet::from_excerpts(intent, &["Inspect the object".into()]).unwrap();
    let question = DecisionQuestion {
        operation: operations.operations()[0].id().clone(),
        capability: CapabilityRef {
            catalog: "matrix".into(),
            capability: "source_read".into(),
        },
    };
    let scope = decision_scope(intent, &index, &authorization);
    assert!(matches!(
        DecisionBatch::new(
            super::super::RELEVANCE_MODEL,
            intent,
            &operations,
            &scope,
            &authorization,
            &index,
            &[question]
        ),
        Err(OperationError::DecisionPacketTooLarge { .. })
    ));
}
