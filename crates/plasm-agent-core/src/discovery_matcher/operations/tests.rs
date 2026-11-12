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
        RequestIdentity::from_request(intent),
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
    assert!(checkpoint
        .receive(bad_packet, || async {
            panic!("corrupt cache must not trigger implicit replay")
        })
        .await
        .is_err());
}

/// Independent synthetic fixture: a read and a notice are requested, while
/// a reset is an unrelated effect. Each bounded packet is checkpointed and
/// resumed; only the semantic provider's answers are scripted here.

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
    assert!(spans
        .values()
        .any(|span| span.contains("do not pay without approval")));
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
    assert!(wire["questions"]["q1"]["instructions"]
        .as_str()
        .unwrap()
        .contains("necessary component"));
    assert!(wire["state"]["capabilities"]["c1"]["operation"]["contract"]
        .as_str()
        .unwrap()
        .starts_with("Inspect a parcel"));
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
fn oversized_canonical_contract_is_reported_as_budget_pressure() {
    use plasm_core::prerequisites::{CapabilityRef, DeploymentBindings};
    let mut catalogs = super::super::online::tests::fixture();
    catalogs
        .get_mut("matrix")
        .unwrap()
        .capabilities
        .get_mut("source_read")
        .unwrap()
        .description = "Detailed contract information. ".repeat(1600);
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

#[test]
fn whole_request_relevance_precedes_independent_fragment_coverage() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Read the sensor and send a notice.";
    let candidates: Vec<_> = ["read", "send"]
        .into_iter()
        .map(|capability| CapabilityRef {
            catalog: "matrix".into(),
            capability: capability.into(),
        })
        .collect();
    let authorized = candidates.iter().cloned().collect();
    let scope = MatrixScope::new(
        "session",
        "generation",
        &authorized,
        RequestIdentity::from_request(intent),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(
        scope,
        intent,
        OperationSet::from_request(intent).unwrap(),
        &authorized,
        candidates.clone(),
    )
    .unwrap();
    let first = matrix.next_packet(2).unwrap();
    assert_eq!(first.len(), 2);
    let primary = matrix.operations.operations[0].id.clone();
    assert_eq!(first[0].0, primary);
    assert_ne!(first[1].0, primary);
    assert_eq!(first[0].1, first[1].1);
    let issued = IssuedPairs {
        scope: matrix.scope.clone(),
        pairs: first.iter().cloned().collect(),
    };
    let judgments: Vec<_> = first
        .into_iter()
        .map(|(operation, capability)| OperationJudgment {
            operation,
            capability,
            contribution: DirectContribution::Relevant,
        })
        .collect();
    matrix.record_packet(&issued, &judgments).unwrap();
    let coverage = matrix.next_packet(2).unwrap();
    assert!(!coverage.is_empty());
    assert!(!matrix.uncovered_operations().collect::<Vec<_>>().is_empty());
}

#[test]
fn direct_coverage_rejects_fragment_as_the_primary_request() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Inspect the item and send a notice.";
    let candidate = CapabilityRef {
        catalog: "matrix".into(),
        capability: "read".into(),
    };
    let allowed = BTreeSet::from([candidate.clone()]);
    let scope = MatrixScope::new(
        "session",
        "generation",
        &allowed,
        RequestIdentity::from_request(intent),
    )
    .unwrap();
    let fragments = OperationSet::from_excerpts(intent, &["Inspect the item".into()]).unwrap();
    assert!(matches!(
        OperationMatrix::new(scope, intent, fragments, &allowed, vec![candidate]),
        Err(OperationError::IntentMismatch)
    ));
}

#[test]
fn direct_coverage_keeps_uncertain_candidates_and_fragments_unresolved() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Inspect the item and send a notice.";
    let candidate = CapabilityRef {
        catalog: "matrix".into(),
        capability: "read".into(),
    };
    let allowed = BTreeSet::from([candidate.clone()]);
    let scope = MatrixScope::new(
        "session",
        "generation",
        &allowed,
        RequestIdentity::from_request(intent),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(
        scope,
        intent,
        OperationSet::from_request(intent).unwrap(),
        &allowed,
        vec![candidate.clone()],
    )
    .unwrap();
    let pair = matrix.next_packet(1).unwrap().pop().unwrap();
    matrix
        .record_packet(
            &issued(&matrix, BTreeSet::from([pair.clone()])),
            &[OperationJudgment {
                operation: pair.0,
                capability: pair.1,
                contribution: DirectContribution::Uncertain,
            }],
        )
        .unwrap();
    assert!(matches!(
        matrix.progress(&candidate),
        Ok(CandidateProgress::Uncertain { .. })
    ));
    assert!(matrix.directly_supported().is_empty());
    assert!(!matrix.uncovered_operations().collect::<Vec<_>>().is_empty());
}

fn direct_fixture() -> (
    OperationMatrix,
    BTreeSet<plasm_core::prerequisites::CapabilityRef>,
    String,
) {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Read the sensor and notify the operator.".to_owned();
    let candidates: Vec<_> = ["read", "send"]
        .into_iter()
        .map(|capability| CapabilityRef {
            catalog: "matrix".into(),
            capability: capability.into(),
        })
        .collect();
    let allowed = candidates.iter().cloned().collect();
    let scope = MatrixScope::new(
        "session",
        "generation",
        &allowed,
        RequestIdentity::from_request(&intent),
    )
    .unwrap();
    let matrix = OperationMatrix::new(
        scope,
        &intent,
        OperationSet::from_request(&intent).unwrap(),
        &allowed,
        candidates,
    )
    .unwrap();
    (matrix, allowed, intent)
}

#[test]
fn direct_coverage_packet_is_atomic_and_unknown_is_not_unrelated() {
    let (mut matrix, _, _) = direct_fixture();
    let pairs = matrix.next_packet(2).unwrap();
    let first = &pairs[0];
    let issued = issued(&matrix, pairs.iter().cloned().collect());
    let answer = OperationJudgment {
        operation: first.0.clone(),
        capability: first.1.clone(),
        contribution: DirectContribution::Relevant,
    };
    assert_eq!(
        matrix.record_packet(&issued, &[answer]),
        Err(OperationError::IncompleteJudgments)
    );
    assert!(matrix.directly_supported().is_empty());
    assert!(matches!(
        matrix.progress(&first.1),
        Ok(CandidateProgress::Incomplete { .. })
    ));
    assert!(!matrix.uncovered_operations().collect::<Vec<_>>().is_empty());
}

#[test]
fn direct_coverage_checkpoint_revalidates_scope_and_request() {
    let (matrix, allowed, intent) = direct_fixture();
    let checkpoint = matrix.checkpoint();
    assert!(
        OperationMatrix::restore(matrix.scope(), &intent, &allowed, checkpoint.clone()).is_ok()
    );
    assert!(matches!(
        OperationMatrix::restore(
            matrix.scope(),
            "different request",
            &allowed,
            checkpoint.clone()
        ),
        Err(OperationError::IntentMismatch)
    ));
    assert!(matches!(
        OperationMatrix::restore(
            matrix.scope(),
            &intent,
            &BTreeSet::new(),
            checkpoint.clone()
        ),
        Err(OperationError::ScopeMismatch)
    ));
    let other = MatrixScope::new(
        "other session",
        "generation",
        &allowed,
        RequestIdentity::from_request(&intent),
    )
    .unwrap();
    assert!(matches!(
        OperationMatrix::restore(&other, &intent, &allowed, checkpoint),
        Err(OperationError::ScopeMismatch)
    ));
}

#[test]
fn direct_coverage_rejects_unauthorized_candidates_and_issued_pairs() {
    use plasm_core::prerequisites::CapabilityRef;
    let (mut matrix, allowed, _) = direct_fixture();
    let denied = CapabilityRef {
        catalog: "other".into(),
        capability: "write".into(),
    };
    assert_eq!(
        matrix.add_candidates(&allowed, &[denied.clone()]),
        Err(OperationError::InvalidCandidates)
    );
    let pair = (matrix.operations.operations[0].id.clone(), denied.clone());
    let issued = issued(&matrix, BTreeSet::from([pair.clone()]));
    let answer = OperationJudgment {
        operation: pair.0,
        capability: denied,
        contribution: DirectContribution::Relevant,
    };
    assert_eq!(
        matrix.record_packet(&issued, &[answer]),
        Err(OperationError::UnknownQuestion)
    );
    assert!(matrix.directly_supported().is_empty());
}

#[test]
fn direct_coverage_prioritizes_current_request_without_dropping_history() {
    use plasm_core::prerequisites::CapabilityRef;
    let provenance = crate::intent_provenance::IntentProvenance::from_turns([
        "Check ownership.".into(),
        "Read the device.".into(),
    ])
    .unwrap();
    let intent = provenance.selection_request();
    let candidate = CapabilityRef {
        catalog: "matrix".into(),
        capability: "read".into(),
    };
    let allowed = BTreeSet::from([candidate.clone()]);
    let scope = MatrixScope::new(
        "session",
        "generation",
        &allowed,
        RequestIdentity::from_request(&intent),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(
        scope,
        &intent,
        OperationSet::from_request(&intent).unwrap(),
        &allowed,
        vec![candidate.clone()],
    )
    .unwrap();
    matrix.prioritize_current_request(provenance.current());
    let first = matrix.next_packet(1).unwrap().pop().unwrap();
    matrix
        .record_packet(
            &issued(&matrix, BTreeSet::from([first.clone()])),
            &[OperationJudgment {
                operation: first.0,
                capability: first.1,
                contribution: DirectContribution::Relevant,
            }],
        )
        .unwrap();
    let coverage = matrix.next_packet(1).unwrap().pop().unwrap();
    let selected = matrix
        .operations
        .operations
        .iter()
        .find(|operation| operation.id == coverage.0)
        .unwrap();
    assert!(selected.text.contains("Read the device"));
    assert!(
        matrix
            .uncovered_operations()
            .any(|text| text.contains("Check ownership"))
    );
}

#[test]
fn direct_coverage_whole_request_relevance_is_not_coverage_proof() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Read the device.";
    let candidate = CapabilityRef {
        catalog: "matrix".into(),
        capability: "read".into(),
    };
    let allowed = BTreeSet::from([candidate.clone()]);
    let scope = MatrixScope::new(
        "session",
        "generation",
        &allowed,
        RequestIdentity::from_request(intent),
    )
    .unwrap();
    let mut matrix = OperationMatrix::new(
        scope,
        intent,
        OperationSet::from_request(intent).unwrap(),
        &allowed,
        vec![candidate],
    )
    .unwrap();
    let pair = matrix.next_packet(1).unwrap().pop().unwrap();
    matrix
        .record_packet(
            &issued(&matrix, BTreeSet::from([pair.clone()])),
            &[OperationJudgment {
                operation: pair.0,
                capability: pair.1,
                contribution: DirectContribution::Relevant,
            }],
        )
        .unwrap();
    assert_eq!(
        matrix.uncovered_operations().collect::<Vec<_>>(),
        vec![intent]
    );
    let coverage = matrix.next_packet(1).unwrap().pop().unwrap();
    assert_ne!(coverage.0, matrix.operations.operations[0].id);
    matrix
        .record_packet(
            &issued(&matrix, BTreeSet::from([coverage.clone()])),
            &[OperationJudgment {
                operation: coverage.0,
                capability: coverage.1,
                contribution: DirectContribution::Relevant,
            }],
        )
        .unwrap();
    assert!(matrix.uncovered_operations().next().is_none());
}

#[test]
fn direct_coverage_cannot_be_proved_by_an_unrelated_candidate() {
    let (mut matrix, _, _) = direct_fixture();
    let packet = matrix.next_packet(2).unwrap();
    let primary = matrix.operations.operations[0].id.clone();
    assert_eq!(packet[0].0, primary);
    assert_ne!(packet[1].0, primary);
    let issued = issued(&matrix, packet.iter().cloned().collect());
    let answers: Vec<_> = packet
        .iter()
        .map(|(operation, capability)| OperationJudgment {
            operation: operation.clone(),
            capability: capability.clone(),
            contribution: if *operation == primary {
                DirectContribution::Unrelated
            } else {
                DirectContribution::Relevant
            },
        })
        .collect();
    matrix.record_packet(&issued, &answers).unwrap();
    assert!(matrix.directly_supported().is_empty());
    assert!(!matrix.uncovered_operations().collect::<Vec<_>>().is_empty());
}

#[test]
fn contextual_fragment_remains_unverified_and_cannot_authorize_a_candidate() {
    let (mut matrix, _, _) = direct_fixture();
    let pairs = matrix.next_packet(2).unwrap();
    let primary = matrix.operations.operations[0].id.clone();
    assert_eq!(pairs[0].0, primary);
    let fragment = matrix
        .operations
        .operations
        .iter()
        .find(|op| op.id == pairs[1].0)
        .unwrap()
        .text
        .clone();
    let issued = issued(&matrix, pairs.iter().cloned().collect());
    let answers: Vec<_> = pairs
        .iter()
        .map(|(operation, capability)| OperationJudgment {
            operation: operation.clone(),
            capability: capability.clone(),
            contribution: if *operation == primary {
                DirectContribution::Uncertain
            } else {
                DirectContribution::Contextual
            },
        })
        .collect();
    matrix.record_packet(&issued, &answers).unwrap();
    assert!(matrix.contextual_fragment(&fragment));
    assert!(matrix.uncovered_operations().any(|text| text == fragment));
    assert!(matrix.directly_supported().is_empty());
}

#[test]
fn contextual_judgment_is_rejected_for_the_whole_request() {
    let (mut matrix, _, _) = direct_fixture();
    let pair = matrix.next_packet(1).unwrap().pop().unwrap();
    let issued = issued(&matrix, BTreeSet::from([pair.clone()]));
    let answer = OperationJudgment {
        operation: pair.0,
        capability: pair.1,
        contribution: DirectContribution::Contextual,
    };
    assert_eq!(
        matrix.record_packet(&issued, &[answer]),
        Err(OperationError::InvalidDecisionResponse)
    );
    assert!(matrix.directly_supported().is_empty());
}

#[test]
fn contextual_choice_is_accepted_only_for_an_issued_fragment_question() {
    use plasm_core::prerequisites::CapabilityRef;
    let intent = "Inspect the parcel.";
    let operations = OperationSet::from_excerpts(intent, &[intent.into()]).unwrap();
    let (index, authorization) = decision_fixture();
    let scope = decision_scope(intent, &index, &authorization);
    let question = DecisionQuestion { operation: operations.operations()[0].id().clone(), capability: CapabilityRef { catalog: "matrix".into(), capability: "inspect".into() } };
    let mut batch = DecisionBatch::new(super::super::RELEVANCE_MODEL, intent, &operations, &scope, &authorization, &index, &[question]).unwrap();
    let raw = serde_json::json!({"model":super::super::RELEVANCE_MODEL,"provider":"OpenAI","answers":{"q1":{"type":"choice","choice":"contextual","confidence":1.0,"probabilities":{"relevant":0.0,"unrelated":0.0,"uncertain":0.0,"contextual":1.0}}}}).to_string();
    assert!(batch.decode(&raw).is_err());
    batch.contextual_questions.insert("q1".into());
    assert_eq!(batch.decode(&raw).unwrap()[0].contribution, DirectContribution::Contextual);
}
