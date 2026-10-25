use super::*;

fn packets(
    catalogs: &BTreeMap<String, plasm_core::CGS>,
    retrieval: &RetrievalReceipt,
) -> Result<Vec<IssuedBatch>, DiscoveryMatcherError> {
    let evidence =
        CatalogEvidence::from_catalogs(retrieval, catalogs, &Default::default(), &|_| true)?;
    super::super::issue_batches(
        RELEVANCE_MODEL,
        &IntentProvenance::from_turns(["Process material without deleting originals.".into()])
            .unwrap(),
        retrieval,
        &evidence,
    )
}

#[test]
fn local_projection_preserves_full_target_and_compact_declaration_output() {
    let (catalogs, retrieval) = super::reference_index::matrix();
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let batches = packets(&catalogs, &retrieval).unwrap();
    assert_eq!(batches.len(), retrieval.candidates.len());
    let batch = batches
        .iter()
        .find(|b| b.questions[0].reference.capability == "prepare")
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
    assert_eq!(body["state"]["rule"], RUBRIC);
    assert!(batch.body().find("\"types\":{\"values\":").is_some());
    assert_eq!(body["questions"].as_object().unwrap().len(), 1);
    let cards = body["state"]["capabilities"].as_object().unwrap();
    assert_eq!(cards.len(), 2);
    let target = cards
        .values()
        .find(|c| c["capability"]["reference"]["capability"] == "prepare")
        .unwrap();
    let target_ref = CapabilityRef {
        catalog: "matrix".into(),
        capability: "prepare".into(),
    };
    assert_eq!(
        target["declaration"],
        serde_json::to_value(&evidence.capabilities[&target_ref].declaration).unwrap()
    );
    let source = cards
        .values()
        .find(|c| c["capability"]["reference"]["capability"] == "source_read")
        .unwrap();
    let source_ref = CapabilityRef {
        catalog: "matrix".into(),
        capability: "source_read".into(),
    };
    let original = serde_json::to_value(&evidence.capabilities[&source_ref].declaration).unwrap();
    assert_eq!(source["declaration"]["output"], original["output"]);
    assert!(source.get("output").is_none());
    assert!(source["declaration"].get("provides").is_none());
    assert_eq!(source["slots"][0]["output_field"]["name"], "value");
    assert!(body["state"]["types"]["records"]
        .get("matrix/Source")
        .is_none());
    assert_eq!(body["state"]["bindings"].as_array().unwrap().len(), 1);
    assert!(!batch.body().contains("source_parent"));
}

#[test]
fn isolated_opaque_targets_keep_authored_uncertainty_and_no_peer_records() {
    let (catalogs, retrieval) = super::reference_index::matrix();
    let batches = packets(&catalogs, &retrieval).unwrap();
    let batch = batches
        .iter()
        .find(|b| b.questions[0].reference.capability == "opaque")
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
    let cards = body["state"]["capabilities"].as_object().unwrap();
    assert_eq!(cards.len(), 1);
    let card = cards.values().next().unwrap();
    assert_eq!(card["declaration"]["output"]["type"], "side_effect");
    assert!(body["state"].get("bindings").is_none());
    assert!(body["state"]["types"]["records"]
        .get("matrix/Source")
        .is_none());
}

#[test]
fn nested_named_lane_bindings_survive_compact_projection() {
    use plasm_core::schema::{InputFieldWire, InputType};
    for lane in ["scope", "selection", "controls"] {
        let (mut catalogs, retrieval) = super::reference_index::matrix();
        let cap = catalogs
            .get_mut("matrix")
            .unwrap()
            .capabilities
            .get_mut("prepare")
            .unwrap();
        let arguments = cap.inputs.arguments.take().unwrap();
        let InputType::Object { fields, .. } = arguments.input_type else {
            panic!("fixture object arguments");
        };
        let mut nested = fields[0].clone();
        nested.name = "binding".into();
        nested.wire = InputFieldWire::Inline(Box::new(InputType::Object {
            fields,
            additional_fields: false,
        }));
        match lane {
            "scope" => cap.inputs.scope.0 = vec![nested],
            "selection" => cap.inputs.selection.0 = vec![nested],
            "controls" => cap.inputs.controls.0 = vec![nested],
            _ => unreachable!(),
        }
        let batches = packets(&catalogs, &retrieval).unwrap();
        let source = batches
            .iter()
            .find(|batch| batch.questions[0].reference.capability == "source_read")
            .unwrap();
        let body: serde_json::Value = serde_json::from_str(source.body()).unwrap();
        let consumer = body["state"]["capabilities"]
            .as_object()
            .unwrap()
            .values()
            .find(|card| card["capability"]["reference"]["capability"] == "prepare")
            .unwrap();
        let slot = &consumer["slots"][0];
        assert_eq!(slot["input"]["lane"], lane);
        assert_eq!(
            slot["input"]["path"],
            serde_json::json!(["binding", "resource"])
        );
        assert_eq!(slot["fields"][0]["present"], true);
        assert_eq!(slot["fields"][0]["field"]["value_ref"], "source");
        assert_eq!(slot["fields"][0]["context"][1]["field"]["required"], true);
        assert!(body["state"]["types"]["values"]["matrix/source"].is_object());
    }
}

#[test]
fn candidate_permutation_keeps_local_bytes_and_cache_identity() {
    let (catalogs, mut retrieval) = super::reference_index::matrix();
    let before = packets(&catalogs, &retrieval).unwrap();
    retrieval.candidates.reverse();
    let after = packets(&catalogs, &retrieval).unwrap();
    assert_eq!(
        before.iter().map(|b| b.body()).collect::<Vec<_>>(),
        after.iter().map(|b| b.body()).collect::<Vec<_>>()
    );
    assert_eq!(
        before.iter().map(|b| b.cache_key()).collect::<Vec<_>>(),
        after.iter().map(|b| b.cache_key()).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn oversized_utf8_target_fails_before_any_provider_call() {
    let (mut catalogs, retrieval) = super::reference_index::matrix();
    catalogs
        .get_mut("matrix")
        .unwrap()
        .capabilities
        .get_mut("opaque")
        .unwrap()
        .description = "λ".repeat(witnesses::MAX_BYTES);
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let mut calls = 0;
    let result = super::super::judge_candidates(
        RELEVANCE_MODEL,
        &IntentProvenance::from_turns(["Process material".into()]).unwrap(),
        &retrieval,
        &evidence,
        |_| {
            calls += 1;
            async { unreachable!("must preflight every scope") }
        },
    )
    .await;
    assert_eq!(calls, 0);
    assert!(
        matches!(result,Err(DiscoveryMatcherError::ScopeLimit{bytes,byte_limit:16_000,..}) if bytes>16_000)
    );
}

#[test]
fn unrelated_retrieved_capabilities_never_expand_local_evidence() {
    let (catalogs, retrieval) = super::reference_index::matrix();
    for batch in packets(&catalogs, &retrieval).unwrap() {
        let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
        assert!(
            body["state"]["capabilities"].as_object().unwrap().len() < retrieval.candidates.len()
        );
        assert!(batch.body().len() <= 16_000);
        assert_eq!(body["questions"].as_object().unwrap().len(), 1);
    }
}

#[test]
fn denied_candidate_never_reaches_projection() {
    let (catalogs, retrieval) = super::reference_index::matrix();
    assert!(matches!(
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|r| r
            .capability
            != "source_read"),
        Err(DiscoveryMatcherError::UnauthorizedWitness(_))
    ));
}

fn prerequisite_fixture() -> (
    BTreeMap<String, plasm_core::CGS>,
    RetrievalReceipt,
    plasm_core::prerequisites::DeploymentBindings,
) {
    let cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/prerequisite_matrix"),
    )
    .unwrap();
    let document = plasm_core::catalog_discovery::capability_documents(&cgs)
        .unwrap()
        .into_iter()
        .find(|d| d.capability == "read")
        .unwrap();
    let reference = CapabilityRef {
        catalog: "matrix".into(),
        capability: "read".into(),
    };
    let deployments = plasm_core::prerequisites::DeploymentBindings {
        bindings: vec![plasm_core::prerequisites::DeploymentBinding {
            consumer: reference.clone(),
            requirement: "scoped_access".into(),
            provider_catalog: "matrix".into(),
            provider: "value_source".into(),
        }],
    };
    let retrieval = RetrievalReceipt {
        generation: "abstract".into(),
        lexical_count: 1,
        vector_count: 0,
        lexical_truncated: false,
        vector_truncated: false,
        candidates: vec![crate::discovery_store::RetrievedCapability {
            id: "read".into(),
            reference,
            document,
            admissions: BTreeSet::from(["lexical".into()]),
        }],
    };
    (
        BTreeMap::from([("matrix".into(), cgs)]),
        retrieval,
        deployments,
    )
}

#[test]
fn supplemental_prerequisite_is_authorized_evidence_only() {
    let (catalogs, retrieval, deployments) = prerequisite_fixture();
    assert!(matches!(
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &deployments, &|r| r.capability
            != "acquire"),
        Err(DiscoveryMatcherError::UnauthorizedWitness(_))
    ));
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &deployments, &|_| true).unwrap();
    let batches = super::super::issue_batches(
        RELEVANCE_MODEL,
        &IntentProvenance::from_turns(["Read records".into()]).unwrap(),
        &retrieval,
        &evidence,
    )
    .unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(
        batches[0].bindings.values().collect::<Vec<_>>(),
        vec![&"read".to_string()]
    );
    let body: serde_json::Value = serde_json::from_str(batches[0].body()).unwrap();
    assert_eq!(body["state"]["capabilities"].as_object().unwrap().len(), 2);
    assert_eq!(
        body["state"]["bindings"][0]["arguments"]["account"]["value"],
        "account-a"
    );
    assert_eq!(
        body["state"]["bindings"][0]["provider_inputs"]["account"]["path"],
        json!(["account"])
    );
}

#[test]
fn unresolved_and_empty_prerequisites_fail_without_omitting_evidence() {
    let (mut catalogs, retrieval, deployments) = prerequisite_fixture();
    assert!(matches!(
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true),
        Err(DiscoveryMatcherError::Witness(_))
    ));
    catalogs
        .get_mut("matrix")
        .unwrap()
        .prerequisites
        .requirements
        .get_mut("read")
        .unwrap()[0]
        .bindings
        .clear();
    assert!(matches!(
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &deployments, &|_| true),
        Err(DiscoveryMatcherError::InvalidWitness {
            part: "prerequisite has no output binding",
            ..
        })
    ));
}

#[test]
fn too_many_direct_sources_fail_with_measured_count() {
    let (mut catalogs, mut retrieval) = super::reference_index::matrix();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let original = cgs.capabilities["source_read"].clone();
    for i in 0..9 {
        let name = format!("source_copy_{i}");
        let mut cap = original.clone();
        cap.name = name.clone().into();
        cgs.capabilities.insert(name.into(), cap);
    }
    retrieval.candidates = plasm_core::catalog_discovery::capability_documents(cgs)
        .unwrap()
        .into_iter()
        .enumerate()
        .map(
            |(i, document)| crate::discovery_store::RetrievedCapability {
                id: format!("candidate{i}"),
                reference: CapabilityRef {
                    catalog: "matrix".into(),
                    capability: document.capability.clone(),
                },
                document,
                admissions: BTreeSet::from(["lexical".into()]),
            },
        )
        .collect();
    assert!(
        matches!(packets(&catalogs,&retrieval),Err(DiscoveryMatcherError::ScopeLimit{capabilities,capability_limit:8,..}) if capabilities>8)
    );
}

#[test]
fn receiver_witness_preserves_exact_identity_without_neighbor_record_expansion() {
    let (mut catalogs, retrieval) = super::reference_index::matrix();
    catalogs
        .get_mut("matrix")
        .unwrap()
        .capabilities
        .get_mut("prepare")
        .unwrap()
        .inputs
        .receiver = Some(plasm_core::CapabilityReceiver::Entity {
        entity: "Source".into(),
    });
    let batches = packets(&catalogs, &retrieval).unwrap();
    let batch = batches
        .iter()
        .find(|b| b.questions[0].reference.capability == "source_read")
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
    let prepare = body["state"]["capabilities"]
        .as_object()
        .unwrap()
        .values()
        .find(|c| c["capability"]["reference"]["capability"] == "prepare")
        .unwrap();
    let receiver = prepare["slots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s.get("receiver").is_some())
        .unwrap();
    assert_eq!(receiver["receiver"], "Source");
    assert_eq!(receiver["identity_field"]["name"], "value");
    assert!(body["state"]["types"]["records"]
        .get("matrix/Prepared")
        .is_none());
}

#[test]
fn relation_membership_keeps_exact_authored_relation() {
    let (mut catalogs, retrieval) = super::reference_index::matrix();
    let relation:plasm_core::schema::RelationSchema=serde_json::from_value(json!({"name":"prepared","description":"Prepared identity","target_resource":"Prepared","cardinality":"one"})).unwrap();
    catalogs
        .get_mut("matrix")
        .unwrap()
        .entities
        .get_mut("Source")
        .unwrap()
        .relations
        .insert("prepared".into(), relation.clone());
    let mut evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let target = CapabilityRef {
        catalog: "matrix".into(),
        capability: "prepare".into(),
    };
    evidence.bindings.push(witnesses::Binding::Membership {
        from: witnesses::Endpoint {
            capability: CapabilityRef {
                catalog: "matrix".into(),
                capability: "source_read".into(),
            },
            slot: witnesses::Slot::Identity {
                identity: plasm_core::prerequisites::RowIdentity::Relation {
                    relation: "prepared".into(),
                },
            },
        },
        to: witnesses::Endpoint {
            capability: target.clone(),
            slot: witnesses::Slot::Identity {
                identity: plasm_core::prerequisites::RowIdentity::Field {
                    field: "value".into(),
                },
            },
        },
    });
    let batches = super::super::issue_batches(
        RELEVANCE_MODEL,
        &IntentProvenance::from_turns(["Process material".into()]).unwrap(),
        &retrieval,
        &evidence,
    )
    .unwrap();
    let body: serde_json::Value = serde_json::from_str(
        batches
            .iter()
            .find(|b| b.questions[0].reference == target)
            .unwrap()
            .body(),
    )
    .unwrap();
    let source = body["state"]["capabilities"]
        .as_object()
        .unwrap()
        .values()
        .find(|c| c["capability"]["reference"]["capability"] == "source_read")
        .unwrap();
    assert!(source["slots"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s.get("identity_relation") == Some(&serde_json::to_value(&relation).unwrap())));
}
