use super::*;

#[test]
fn keyed_record_contracts_roundtrip_without_repeating_names() {
    let (catalogs, retrieval) = super::reference_index::matrix();
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let original = serde_json::to_value(&evidence.definitions).unwrap();
    for batch in packets(&catalogs, &retrieval).unwrap() {
        let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
        for (id, record) in body["state"]["types"]["records"].as_object().unwrap() {
            let mut restored = record.clone();
            for section in ["fields", "relations"] {
                if let Some(entries) = restored[section].as_object_mut() {
                    for (key, item) in entries {
                        assert!(item.get("name").is_none());
                        item["name"] = json!(key);
                    }
                }
            }
            assert_eq!(restored, original["records"][id]);
        }
    }
}

#[test]
fn reciprocal_membership_is_one_bidirectional_fact_without_losing_slots() {
    let (catalogs, retrieval) = super::reference_index::matrix();
    let mut evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let from = witnesses::Endpoint {
        capability: CapabilityRef {
            catalog: "matrix".into(),
            capability: "source_read".into(),
        },
        slot: witnesses::Slot::Identity {
            identity: plasm_core::prerequisites::RowIdentity::Field {
                field: "value".into(),
            },
        },
    };
    let to = witnesses::Endpoint {
        capability: CapabilityRef {
            catalog: "matrix".into(),
            capability: "prepare".into(),
        },
        slot: witnesses::Slot::Identity {
            identity: plasm_core::prerequisites::RowIdentity::Field {
                field: "value".into(),
            },
        },
    };
    evidence.bindings = vec![
        witnesses::Binding::Membership {
            from: from.clone(),
            to: to.clone(),
        },
        witnesses::Binding::Membership {
            from: to.clone(),
            to: from.clone(),
        },
        witnesses::Binding::Membership {
            from: witnesses::Endpoint {
                slot: witnesses::Slot::Identity {
                    identity: plasm_core::prerequisites::RowIdentity::Field {
                        field: "parent".into(),
                    },
                },
                ..from
            },
            to,
        },
    ];
    let intent = IntentProvenance::from_turns(["Process material".into()]).unwrap();
    let batches =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    evidence.bindings.reverse();
    let reordered =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    assert_eq!(
        batches.iter().map(|b| b.body()).collect::<Vec<_>>(),
        reordered.iter().map(|b| b.body()).collect::<Vec<_>>()
    );
    for batch in batches.iter().filter(|b| {
        ["source_read", "prepare"].contains(&b.questions[0].reference.capability.as_str())
    }) {
        let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
        let links = body["state"]["bindings"].as_array().unwrap();
        assert_eq!(links.len(), 2);
        let reciprocal = links.iter().find(|b| b["bidirectional"] == true).unwrap();
        assert_eq!(reciprocal["from"]["identity"]["field"], "value");
        assert_eq!(reciprocal["to"]["identity"]["field"], "value");
        let directed = links
            .iter()
            .find(|b| b.get("bidirectional").is_none())
            .unwrap();
        assert_eq!(directed["from"]["identity"]["field"], "parent");
        assert_eq!(directed["to"]["identity"]["field"], "value");
        let mut restored = BTreeSet::new();
        for link in links {
            let mut link = link.clone();
            let both = link.as_object_mut().unwrap().remove("bidirectional") == Some(json!(true));
            for side in ["from", "to"] {
                let id = link[side]["capability"].as_str().unwrap();
                link[side]["capability"] =
                    body["state"]["capabilities"][id]["capability"]["reference"].clone();
            }
            restored.insert(serde_json::to_string(&link).unwrap());
            if both {
                let from = link["from"].take();
                link["from"] = link["to"].take();
                link["to"] = from;
                restored.insert(serde_json::to_string(&link).unwrap());
            }
        }
        let expected: BTreeSet<_> = evidence
            .bindings
            .iter()
            .map(|b| serde_json::to_string(&serde_json::to_value(b).unwrap()).unwrap())
            .collect();
        assert_eq!(restored, expected);
    }
}

#[test]
fn receiver_consumers_do_not_expand_the_acquisition_question() {
    let (mut catalogs, mut retrieval) = super::reference_index::matrix();
    let cgs = catalogs.get_mut("matrix").unwrap();
    cgs.entities
        .get_mut("Source")
        .unwrap()
        .fields
        .shift_remove("parent");
    cgs.entities.get_mut("Source").unwrap().relations.clear();
    for i in 0..16 {
        let mut cap = cgs.capabilities["opaque"].clone();
        let name = format!("apply_{i}");
        cap.name = name.clone().into();
        cap.inputs.receiver = Some(plasm_core::CapabilityReceiver::Entity {
            entity: "Source".into(),
        });
        cgs.capabilities.insert(name.into(), cap);
    }
    retrieval.candidates = plasm_core::catalog_discovery::capability_documents(cgs)
        .unwrap()
        .into_iter()
        .filter(|d| d.capability == "source_read" || d.capability.starts_with("apply_"))
        .map(|document| crate::discovery_store::RetrievedCapability {
            id: document.capability.clone(),
            reference: CapabilityRef {
                catalog: "matrix".into(),
                capability: document.capability.clone(),
            },
            document,
            admissions: BTreeSet::from(["lexical".into()]),
        })
        .collect();
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let batches = packets(&catalogs, &retrieval).unwrap();
    assert_eq!(batches.len(), 17);
    for batch in batches {
        let reference = &batch.questions[0].reference;
        let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
        let cards = body["state"]["capabilities"].as_object().unwrap();
        let acquisition = reference.capability == "source_read";
        assert_eq!(cards.len(), if acquisition { 1 } else { 2 });
        let target = cards
            .values()
            .find(|c| c["capability"]["reference"]["capability"] == reference.capability)
            .unwrap();
        assert_eq!(
            target["declaration"],
            serde_json::to_value(&evidence.capabilities[reference].declaration).unwrap()
        );
        if acquisition {
            assert!(
                body["state"].get("bindings").is_none(),
                "self-acquisition is not evidence"
            );
        } else {
            let links = body["state"]["bindings"].as_array().unwrap();
            assert_eq!(links.len(), 1);
            assert_eq!(links[0]["to"]["kind"], "receiver");
            assert_eq!(links[0]["to"]["entity"], "Source");
            assert_eq!(links[0]["from"]["field"], "value");
        }
    }
}

#[test]
fn shared_wire_profiles_do_not_expand_a_consumer_scope() {
    use plasm_core::schema::{FieldValueKind, InputFieldWire, InputType};
    for profile in [
        plasm_core::value_domain::ProfileId::Email,
        plasm_core::value_domain::ProfileId::E164,
    ] {
        let (mut catalogs, mut retrieval) = super::reference_index::matrix();
        let cgs = catalogs.get_mut("matrix").unwrap();
        // Isolate format matching from the fixture's genuine self-reference.
        let source = cgs.entities.get_mut("Source").unwrap();
        source.fields.shift_remove("parent");
        source.relations.clear();
        let mut wire = cgs.values["source"].clone();
        wire.description = "An address with a validated wire format.".into();
        wire.domain.profile = Some(profile);
        cgs.values.insert("wire".into(), wire.clone());
        wire.description = "A different role with the same format.".into();
        cgs.values.insert("peer_wire".into(), wire);
        let mut field = cgs.entities["Source"].fields["value"].clone();
        field.name = "address".into();
        field.kind =
            FieldValueKind::Registry(plasm_core::ValueDomainKey::new("peer_wire").unwrap());
        cgs.entities
            .get_mut("Source")
            .unwrap()
            .fields
            .insert("address".into(), field);
        let InputType::Object { fields, .. } = &mut cgs
            .capabilities
            .get_mut("prepare")
            .unwrap()
            .inputs
            .arguments
            .as_mut()
            .unwrap()
            .input_type
        else {
            panic!("object input")
        };
        fields[0].wire = InputFieldWire::Registry(plasm_core::ValueDomainKey::new("wire").unwrap());
        for i in 0..16 {
            let mut cap = cgs.capabilities["source_read"].clone();
            let name = format!("address_read_{i}");
            cap.name = name.clone().into();
            cap.inputs.receiver = Some(plasm_core::CapabilityReceiver::None);
            cap.provides = vec!["address".into()];
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
        let batches = packets(&catalogs, &retrieval)
            .expect("format compatibility alone must not exhaust witness scope");
        assert_eq!(batches.len(), retrieval.candidates.len());
        let target = batches
            .iter()
            .find(|b| b.questions[0].reference.capability == "prepare")
            .unwrap();
        let body: serde_json::Value = serde_json::from_str(target.body()).unwrap();
        assert_eq!(body["state"]["capabilities"].as_object().unwrap().len(), 1);
        assert!(body["state"]["types"]["values"]["matrix/wire"].is_object());

        // A genuinely shared authored role still composes, even when profiled.
        let cgs = catalogs.get_mut("matrix").unwrap();
        let InputType::Object { fields, .. } = &mut cgs
            .capabilities
            .get_mut("prepare")
            .unwrap()
            .inputs
            .arguments
            .as_mut()
            .unwrap()
            .input_type
        else {
            panic!("object input")
        };
        fields[0].wire =
            InputFieldWire::Registry(plasm_core::ValueDomainKey::new("peer_wire").unwrap());
        let sources = plasm_core::prerequisites::project_input_source_candidates(
            &BTreeMap::from([("matrix".into(), &*cgs)]),
            &[CapabilityRef {
                catalog: "matrix".into(),
                capability: "prepare".into(),
            }],
            &|_| true,
        )
        .unwrap();
        assert_eq!(
            sources
                .iter()
                .filter(|s| s.provider.capability.starts_with("address_read_")
                    && s.bindings
                        .iter()
                        .any(|b| b.consumer.capability == "prepare"))
                .count(),
            16
        );
    }
}

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
async fn oversized_utf8_target_is_retained_without_any_provider_call() {
    let (mut catalogs, mut retrieval) = super::reference_index::matrix();
    retrieval
        .candidates
        .retain(|c| c.reference.capability == "opaque");
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
    let receipt = result.unwrap();
    assert_eq!(receipt.matches[0].choice, MatchChoice::Uncertain);
    assert_eq!(receipt.budget_retained.len(), 1);
    assert!(receipt.budget_retained[0].bytes > 16_000);
    assert_eq!(receipt.budget_retained[0].byte_limit, 16_000);
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

pub(super) fn prerequisite_fixture() -> (
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
fn prerequisite_provider_does_not_expand_to_all_consumers() {
    let (mut catalogs, mut retrieval, mut deployments) = prerequisite_fixture();
    let cgs = catalogs.get_mut("matrix").unwrap();
    // Isolate declared requirements from incidental ordinary string matches.
    cgs.values.get_mut("text").unwrap().description.clear();
    let requirement = cgs.prerequisites.requirements["read"].clone();
    for i in 0..16 {
        let name = format!("consumer_{i}");
        let mut cap = cgs.capabilities["read"].clone();
        cap.name = name.clone().into();
        cap.inputs.receiver = Some(plasm_core::CapabilityReceiver::None);
        cgs.capabilities.insert(name.clone().into(), cap);
        cgs.prerequisites
            .requirements
            .insert(name.clone(), requirement.clone());
        let mut deployment = deployments.bindings[0].clone();
        deployment.consumer.capability = name;
        deployments.bindings.push(deployment);
    }
    retrieval.candidates = plasm_core::catalog_discovery::capability_documents(cgs)
        .unwrap()
        .into_iter()
        .filter(|d| d.capability == "acquire" || d.capability.starts_with("consumer_"))
        .map(|document| crate::discovery_store::RetrievedCapability {
            id: document.capability.clone(),
            reference: CapabilityRef {
                catalog: "matrix".into(),
                capability: document.capability.clone(),
            },
            document,
            admissions: BTreeSet::from(["lexical".into()]),
        })
        .collect();
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &deployments, &|_| true).unwrap();
    let batches = super::super::issue_batches(
        RELEVANCE_MODEL,
        &IntentProvenance::from_turns(["Read records".into()]).unwrap(),
        &retrieval,
        &evidence,
    )
    .unwrap();
    assert_eq!(batches.len(), 17);
    for batch in batches {
        let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
        let provider = batch.questions[0].reference.capability == "acquire";
        assert_eq!(
            body["state"]["capabilities"].as_object().unwrap().len(),
            if provider { 1 } else { 2 }
        );
        if !provider {
            assert_eq!(body["state"]["bindings"][0]["kind"], "prerequisite");
        }
    }
    let borrowed = catalogs.iter().map(|(k, v)| (k.clone(), v)).collect();
    let closure = plasm_core::prerequisites::prerequisite_closure(
        &borrowed,
        &deployments,
        &[CapabilityRef {
            catalog: "matrix".into(),
            capability: "consumer_7".into(),
        }],
        &BTreeSet::from(["matrix".into()]),
    )
    .unwrap();
    assert!(closure
        .edges
        .iter()
        .any(|edge| edge.provider.capability == "acquire"));
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
        .find(|b| b.questions[0].reference.capability == "prepare")
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
    let source = body["state"]["capabilities"]
        .as_object()
        .unwrap()
        .values()
        .find(|c| c["capability"]["reference"]["capability"] == "source_read")
        .unwrap();
    let identity = source["slots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s.get("output_field").is_some())
        .unwrap();
    assert_eq!(identity["output_field"]["name"], "value");
    assert!(body["state"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["to"]["kind"] == "receiver" && b["to"]["entity"] == "Source"));
    // The receiver's record belongs to the target's full contract. Witnesses
    // must not add records beyond those already referenced by that target.
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let refs =
        serde_json::to_value(&evidence.capabilities[&batch.questions[0].reference].references)
            .unwrap();
    assert_eq!(
        body["state"]["types"]["records"].as_object().unwrap().len(),
        refs["records"].as_array().unwrap().len()
    );
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
