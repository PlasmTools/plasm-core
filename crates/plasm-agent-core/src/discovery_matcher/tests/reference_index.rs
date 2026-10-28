use super::*;
use crate::discovery_store::RetrievedCapability;

pub(super) fn matrix() -> (BTreeMap<String, plasm_core::CGS>, RetrievalReceipt) {
    let cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/discovery_reference_index"),
    )
    .unwrap();
    let candidates = plasm_core::catalog_discovery::capability_documents(&cgs)
        .unwrap()
        .into_iter()
        .enumerate()
        .map(|(i, document)| RetrievedCapability {
            id: format!("candidate{i}"),
            reference: CapabilityRef {
                catalog: "matrix".into(),
                capability: document.capability.clone(),
            },
            document,
            admissions: BTreeSet::from(["lexical".into()]),
        })
        .collect();
    (
        BTreeMap::from([("matrix".into(), cgs)]),
        RetrievalReceipt {
            generation: "matrix".into(),
            candidates,
            lexical_count: 4,
            vector_count: 0,
            lexical_truncated: false,
            vector_truncated: false,
        },
    )
}

#[test]
fn canonical_rendering_uses_local_packets_not_retrieval_summary_sizes() {
    let (catalogs, mut retrieval) = matrix();
    for candidate in &mut retrieval.candidates {
        candidate.document.operation.contract = "retrieval evidence ".repeat(1200);
    }
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let intent = IntentProvenance::from_turns(["Process material.".into()]).unwrap();
    let batches =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    assert_eq!(batches.len(), retrieval.candidates.len());
    let issued_ids: BTreeSet<_> = batches
        .iter()
        .flat_map(|batch| batch.bindings.values().cloned())
        .collect();
    assert_eq!(
        issued_ids,
        retrieval.candidates.iter().map(|c| c.id.clone()).collect()
    );
    for batch in &batches {
        assert_eq!(batch.bindings.len(), 1);
        assert!(batch.body().len() <= witnesses::MAX_BYTES);
        assert!(!batch
            .body()
            .contains("retrieval evidence retrieval evidence"));
    }
}

#[test]
fn reference_index_preserves_composition_full_contracts_and_opaque_outputs() {
    let (catalogs, retrieval) = matrix();
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let intent =
        IntentProvenance::from_turns(["Process the source; preserve originals.".into()]).unwrap();
    let batches =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    let mut answers = Vec::new();
    for batch in &batches {
        let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
        let state = &body["state"];
        assert!(state.get("context_whitespace").is_none());
        assert!(state["capabilities"].as_object().unwrap().len() <= witnesses::MAX_CAPABILITIES);
        for (id, card) in state["capabilities"].as_object().unwrap() {
            assert!(card.get("types").is_none());
            assert!(card.get("type_refs").is_none());
            assert!(card.get("collection_meaning").is_none());
            let name = card["capability"]["reference"]["capability"]
                .as_str()
                .unwrap();
            let candidate = retrieval
                .candidates
                .iter()
                .find(|c| c.reference.capability == name)
                .unwrap();
            assert_eq!(
                card["capability"]["operation"]["contract"],
                catalogs["matrix"].capabilities[name].description
            );
            assert_ne!(
                card["capability"]["operation"]["contract"],
                candidate.document.operation.contract
            );
            for section in ["values", "records"] {
                for name in state["type_refs"][id][section].as_array().unwrap() {
                    assert!(!state["types"][section][name.as_str().unwrap()].is_null());
                }
            }
        }
        let reference = &batch.questions[0].reference;
        assert_eq!(
            packet_declaration(&body, &reference.capability),
            &serde_json::to_value(&evidence.capabilities[reference].declaration).unwrap()
        );
        answers.extend(decode_batch(batch, &response(batch, &[MatchChoice::Uncertain])).unwrap());
    }
    let source = target_packet(&batches, "source_read");
    assert_eq!(
        source["state"]["types"]["records"]["matrix/Source"]["description"],
        catalogs["matrix"].entities["Source"].description
    );
    let prepare = target_packet(&batches, "prepare");
    assert_eq!(
        packet_declaration(&prepare, "prepare")["arguments"]["input_type"]["fields"][0]
            ["value_ref"],
        "source"
    );
    let process = target_packet(&batches, "process");
    assert_eq!(
        packet_declaration(&process, "process")["arguments"]["input_type"]["fields"][0]
            ["value_ref"],
        "prepared"
    );
    let opaque = target_packet(&batches, "opaque");
    let declaration = packet_declaration(&opaque, "opaque");
    assert_eq!(declaration["output"]["type"], "side_effect");
    assert!(declaration["output"].get("entity_type").is_none());
    assert_eq!(declaration["provides"], json!([]));
    let selected = finish_selection(answers, &retrieval, &batches)
        .unwrap()
        .selected(&retrieval);
    assert_eq!(selected.len(), retrieval.candidates.len());
}

#[test]
fn canonical_packets_are_permutation_stable_and_replace_duplicate_prose() {
    let (catalogs, mut retrieval) = matrix();
    let intent = IntentProvenance::from_turns(["Process supplied material.".into()]).unwrap();
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let before =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    retrieval.candidates.reverse();
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let after =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    assert_eq!(
        before.iter().map(IssuedBatch::body).collect::<Vec<_>>(),
        after.iter().map(IssuedBatch::body).collect::<Vec<_>>()
    );
    let current = target_packet(&before, "prepare");
    let mut previous = current.clone();
    for card in previous["state"]["capabilities"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        let name = card["capability"]["reference"]["capability"]
            .as_str()
            .unwrap();
        let doc = &retrieval
            .candidates
            .iter()
            .find(|c| c.reference.capability == name)
            .unwrap()
            .document;
        card["capability"]["operation"] = json!(doc.operation);
        card["collection_meaning"] = json!(doc.collection.meaning);
        let declaration = card["declaration"].as_object_mut().unwrap();
        declaration.remove("acquired_inputs");
        declaration.remove("relation_uses");
        declaration.remove("input_sources");
    }
    let old_bytes = serde_json::to_vec(&previous).unwrap().len();
    let new_bytes = serde_json::to_vec(&current).unwrap().len();
    eprintln!("abstract canonical packet: previous={old_bytes} bytes, current={new_bytes} bytes");
    assert!(
        new_bytes < old_bytes,
        "new structural facts must replace, not merely append to, duplicated evidence"
    );
}

#[test]
fn reference_index_is_catalogue_scoped_and_changes_cache_identity() {
    let (mut catalogs, mut retrieval) = matrix();
    let mut other = catalogs["matrix"].clone();
    other.entry_id = Some("other".into());
    other.values.get_mut("source").unwrap().description = "A distinct source domain.".into();
    catalogs.insert("other".into(), other);
    let mut candidate = retrieval
        .candidates
        .iter()
        .find(|c| c.reference.capability == "source_read")
        .unwrap()
        .clone();
    candidate.id = "foreign".into();
    candidate.reference.catalog = "other".into();
    retrieval.candidates.push(candidate);
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let intent = IntentProvenance::from_turns(["Process the source.".into()]).unwrap();
    let before =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    let source_packet = |batches: &[IssuedBatch], catalog: &str| {
        let batch = batches
            .iter()
            .find(|b| {
                b.questions[0].reference.catalog == catalog
                    && b.questions[0].reference.capability == "source_read"
            })
            .unwrap();
        (
            serde_json::from_str::<serde_json::Value>(batch.body()).unwrap(),
            batch.cache_key().to_owned(),
        )
    };
    let (body, before_key) = source_packet(&before, "matrix");
    let (foreign, _) = source_packet(&before, "other");
    assert_ne!(
        body["state"]["types"]["values"]["matrix/source"],
        foreign["state"]["types"]["values"]["other/source"]
    );
    assert_eq!(
        body["state"]["types"]["values"]["matrix/source"]["description"],
        catalogs["matrix"].values["source"].description
    );
    assert_eq!(
        foreign["state"]["types"]["values"]["other/source"]["description"],
        catalogs["other"].values["source"].description
    );
    catalogs
        .get_mut("matrix")
        .unwrap()
        .values
        .get_mut("source")
        .unwrap()
        .description = "Revised authored definition.".into();
    let revised =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let after =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &revised).unwrap();
    assert_ne!(before_key, source_packet(&after, "matrix").1);
    catalogs.remove("other");
    assert!(matches!(
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true),
        Err(DiscoveryMatcherError::Witness(
            plasm_core::prerequisites::PrerequisiteError::MissingConsumerCatalog
        ))
    ));
}

#[test]
fn reference_index_retains_nested_input_variants_and_named_array_elements() {
    use plasm_core::schema::{ArrayItemsSchema, FieldValueKind};
    let (mut catalogs, retrieval) = matrix();
    let cgs = catalogs.get_mut("matrix").unwrap();
    let mut array = cgs.values["source"].clone();
    array.field_type = plasm_core::FieldType::Array;
    array.array_items = Some(ArrayItemsSchema {
        kind: FieldValueKind::Registry(plasm_core::schema::ValueDomainKey::new("source").unwrap()),
        field_type: plasm_core::FieldType::String,
        value_format: None,
        allowed_values: None,
    });
    cgs.values.insert("sources".into(), array);
    let input = json!({"input_type":{"type":"union","variants":[
        {"name":"batch","fields":[{"name":"items","value_ref":"sources","required":true}],"wire":{"field":"mode","value":"batch"}},
        {"name":"single","fields":[{"name":"item","value_ref":"source","required":false}],"wire":{"field":"mode","value":"single"}}
    ]}});
    cgs.capabilities.get_mut("prepare").unwrap().inputs.payload =
        Some(serde_json::from_value(input.clone()).unwrap());
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let encoded = serde_json::to_value(&evidence.definitions).unwrap();
    assert_eq!(
        encoded["values"]["matrix/sources"]["array_items"]["value_ref"],
        "source"
    );
    assert!(!encoded["values"]["matrix/source"].is_null());
    let cap = &evidence.capabilities[&CapabilityRef {
        catalog: "matrix".into(),
        capability: "prepare".into(),
    }];
    let declaration = serde_json::to_value(&cap.declaration).unwrap();
    assert_eq!(declaration["payload"]["input_type"], input["input_type"]);
    assert!(declaration.get("input_sources").is_none());
    let references = serde_json::to_value(&cap.references).unwrap();
    assert!(references["values"]
        .as_array()
        .unwrap()
        .contains(&json!("matrix/sources")));
    let intent = IntentProvenance::from_turns(["Process supplied material.".into()]).unwrap();
    let batches =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    let packet = target_packet(&batches, "prepare");
    assert_eq!(
        packet_declaration(&packet, "prepare")["payload"]["input_type"],
        input["input_type"]
    );
    assert_eq!(
        packet["state"]["types"]["values"]["matrix/sources"]["array_items"]["value_ref"],
        "source"
    );
    assert!(!packet["state"]["types"]["values"]["matrix/source"].is_null());
    catalogs
        .get_mut("matrix")
        .unwrap()
        .values
        .shift_remove("source");
    assert!(
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .is_err()
    );
}

#[test]
fn canonical_contract_keeps_acquired_inputs_and_names_the_exact_acquired_seat() {
    let cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/prerequisite_matrix"),
    )
    .unwrap();
    let documents = plasm_core::catalog_discovery::capability_documents(&cgs).unwrap();
    let mut retrieval = matrix().1;
    retrieval.candidates = documents
        .into_iter()
        .enumerate()
        .map(|(i, document)| RetrievedCapability {
            id: format!("c{i}"),
            reference: CapabilityRef {
                catalog: "prerequisite_matrix".into(),
                capability: document.capability.clone(),
            },
            document,
            admissions: BTreeSet::from(["lexical".into()]),
        })
        .collect();
    let catalogs = BTreeMap::from([("prerequisite_matrix".into(), cgs)]);
    let deployments = plasm_core::prerequisites::DeploymentBindings {
        bindings: vec![plasm_core::prerequisites::DeploymentBinding {
            consumer: CapabilityRef {
                catalog: "prerequisite_matrix".into(),
                capability: "read".into(),
            },
            requirement: "scoped_access".into(),
            provider_catalog: "prerequisite_matrix".into(),
            provider: "value_source".into(),
        }],
    };
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &deployments, &|_| true).unwrap();
    let key = CapabilityRef {
        catalog: "prerequisite_matrix".into(),
        capability: "read".into(),
    };
    let contract = serde_json::to_value(&evidence.capabilities[&key].declaration).unwrap();
    assert_eq!(contract["selection"][0]["name"], "credential");
    assert_eq!(contract["selection"][0]["required"], true);
    assert_eq!(
        contract["acquired_inputs"],
        json!([{"lane":"selection","path":["credential"]}])
    );
    // An infrastructure acquisition is not inferred to be a source for arbitrary inputs.
    assert!(contract.get("input_sources").is_none());
}

#[test]
fn reference_index_links_entity_inputs_without_inventing_an_output() {
    use plasm_core::schema::{InputFieldSchema, InputFieldWire, InputSchema, InputType};
    let scalar = InputType::Value {
        field_type: plasm_core::FieldType::EntityRef {
            entry_id: "matrix".into(),
            target: "Source".into(),
        },
        allowed_values: None,
    };
    let nested = InputType::Array {
        element_type: Box::new(scalar.clone()),
        min_length: None,
        max_length: None,
    };
    let field: InputFieldSchema =
        serde_json::from_value(json!({"name":"resource","value_ref":"source","required":true}))
            .unwrap();
    for ty in [
        scalar.clone(),
        nested,
        InputType::Object {
            fields: vec![InputFieldSchema {
                wire: InputFieldWire::Inline(Box::new(scalar.clone())),
                ..field.clone()
            }],
            additional_fields: false,
        },
        InputType::Union {
            variants: vec![plasm_core::schema::InputVariantSchema {
                name: "row".into(),
                description: None,
                constructor_symbol: None,
                fields: vec![InputFieldSchema {
                    wire: InputFieldWire::Inline(Box::new(scalar)),
                    ..field
                }],
                wire: plasm_core::schema::WireVariantDiscriminator {
                    field: "kind".into(),
                    value: "row".into(),
                },
            }],
        },
    ] {
        let (mut catalogs, mut retrieval) = matrix();
        retrieval
            .candidates
            .retain(|c| c.reference.capability == "opaque");
        let cap = catalogs
            .get_mut("matrix")
            .unwrap()
            .capabilities
            .get_mut("opaque")
            .unwrap();
        cap.output_schema = None;
        cap.inputs.arguments = Some(InputSchema {
            input_type: ty,
            validation: Default::default(),
            description: Some("Supplied resource".into()),
            examples: vec![json!("not contract evidence")],
        });
        let evidence =
            CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
                .unwrap();
        let cap = evidence.capabilities.values().next().unwrap();
        let refs = serde_json::to_value(&cap.references).unwrap();
        assert!(refs["records"]
            .as_array()
            .unwrap()
            .contains(&json!("matrix/Source")));
        let definitions = serde_json::to_value(&evidence.definitions).unwrap();
        assert!(!definitions["records"]["matrix/Source"].is_null());
        let declaration = serde_json::to_value(&cap.declaration).unwrap();
        assert!(declaration.get("output").is_none());
        assert!(declaration["arguments"].get("examples").is_none());
        let intent = IntentProvenance::from_turns(["Inspect the resource.".into()]).unwrap();
        let batches =
            super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
        let packet = target_packet(&batches, "opaque");
        assert_eq!(packet_declaration(&packet, "opaque"), &declaration);
        assert!(packet["state"].get("bindings").is_none());
    }
}

// Exercise the actual decision wire, not just the core projection helper.
fn canonical_packet(cgs: plasm_core::CGS, excluded: &[&str]) -> serde_json::Value {
    let mut retrieval = matrix().1;
    retrieval.candidates = plasm_core::catalog_discovery::capability_documents(&cgs)
        .unwrap()
        .into_iter()
        .filter(|doc| !excluded.contains(&doc.capability.as_str()))
        .enumerate()
        .map(|(i, document)| RetrievedCapability {
            id: format!("c{i}"),
            reference: CapabilityRef {
                catalog: "matrix".into(),
                capability: document.capability.clone(),
            },
            document,
            admissions: BTreeSet::from(["lexical".into()]),
        })
        .collect();
    let catalogs = BTreeMap::from([("matrix".into(), cgs)]);
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let intent = IntentProvenance::from_turns(["Process supplied resources.".into()]).unwrap();
    let batches =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    target_packet(&batches, "prepare")
}

fn target_packet(batches: &[IssuedBatch], capability: &str) -> serde_json::Value {
    let batch = batches
        .iter()
        .find(|b| b.questions[0].reference.capability == capability)
        .unwrap();
    serde_json::from_str(batch.body()).unwrap()
}

fn packet_declaration<'a>(
    packet: &'a serde_json::Value,
    capability: &str,
) -> &'a serde_json::Value {
    &packet["state"]["capabilities"]
        .as_object()
        .unwrap()
        .values()
        .find(|card| card["capability"]["reference"]["capability"] == capability)
        .unwrap()["declaration"]
}

#[test]
fn dense_compatible_reads_fail_the_local_count_bound_without_truncation() {
    for count in [32, 128] {
        let mut cgs = matrix().0.remove("matrix").unwrap();
        let source = cgs.capabilities["source_read"].clone();
        let arguments = cgs.capabilities["prepare"].inputs.arguments.clone();
        let entity = cgs.entities["Source"].clone();
        cgs.capabilities.clear();
        for i in 0..count {
            let name = format!("read_{i:03}");
            let domain = format!("Source{i:03}");
            cgs.entities.insert(domain.clone().into(), entity.clone());
            let mut cap = source.clone();
            cap.name = name.clone().into();
            cap.domain = domain.clone().into();
            cap.inputs.arguments = arguments.clone();
            cap.output_schema.as_mut().unwrap().output_type =
                plasm_core::schema::OutputType::Entity {
                    entity_type: domain,
                };
            cgs.capabilities.insert(name.into(), cap);
        }
        let mut retrieval = matrix().1;
        retrieval.candidates = plasm_core::catalog_discovery::capability_documents(&cgs)
            .unwrap()
            .into_iter()
            .enumerate()
            .map(|(i, document)| RetrievedCapability {
                id: format!("candidate{i}"),
                reference: CapabilityRef {
                    catalog: "matrix".into(),
                    capability: document.capability.clone(),
                },
                document,
                admissions: BTreeSet::from(["lexical".into()]),
            })
            .collect();
        let catalogs = BTreeMap::from([("matrix".into(), cgs)]);
        let evidence =
            CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
                .unwrap();
        let intent = IntentProvenance::from_turns(["Read supplied material.".into()]).unwrap();
        assert!(matches!(
            super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence),
            Err(DiscoveryMatcherError::ScopeLimit { capabilities, capability_limit: 8, .. }) if capabilities == count
        ));
    }
}

#[test]
fn canonical_packet_keeps_absent_variant_fields_separate_from_null_permission() {
    let mut cgs = matrix().0.remove("matrix").unwrap();
    let input = json!({"input_type":{"type":"union","variants":[
        {"name":"present","fields":[{"name":"resource","value_ref":"source","required":true}],"wire":{"field":"mode","value":"present"}},
        {"name":"absent","fields":[],"wire":{"field":"mode","value":"absent"}}
    ]},"validation":{"allow_null":true}});
    cgs.capabilities.get_mut("prepare").unwrap().inputs.payload =
        Some(serde_json::from_value(input.clone()).unwrap());
    let packet = canonical_packet(cgs, &[]);
    let declaration = packet_declaration(&packet, "prepare");
    assert_eq!(declaration["payload"]["input_type"], input["input_type"]);
    assert_eq!(declaration["payload"]["validation"]["allow_null"], true);
    assert_eq!(
        declaration["payload"]["input_type"]["variants"][1]["fields"],
        json!([])
    );
    assert!(declaration.get("input_sources").is_none());
}

#[test]
fn nested_union_target_survives_local_projection_without_invented_branch_bindings() {
    let (mut catalogs, retrieval) = matrix();
    let input = json!({
        "input_type": {"type":"object", "additional_fields":false, "fields":[{
            "name":"choice", "required":true,
            "input_type":{"type":"union","variants":[
                {"name":"present", "description":"A supplied resource", "fields":[{
                    "name":"bundle", "required":true,
                    "input_type":{"type":"object","additional_fields":false,"fields":[
                        {"name":"resource","value_ref":"source","required":true},
                        {"name":"note","value_ref":"result","required":false}
                    ]}
                }], "wire":{"field":"mode","value":"present"}},
                {"name":"absent", "description":"No resource in this branch", "fields":[],
                 "wire":{"field":"mode","value":"absent"}}
            ]}
        }]},
        "validation":{"allow_null":true},
        "description":"The alternatives are part of the authored contract."
    });
    catalogs
        .get_mut("matrix")
        .unwrap()
        .capabilities
        .get_mut("prepare")
        .unwrap()
        .inputs
        .payload = Some(serde_json::from_value(input.clone()).unwrap());
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let intent = IntentProvenance::from_turns(["Prepare supplied material.".into()]).unwrap();
    let batches =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    let target = target_packet(&batches, "prepare");
    let declaration = packet_declaration(&target, "prepare");
    assert_eq!(declaration["payload"]["input_type"], input["input_type"]);
    assert_eq!(declaration["payload"]["validation"]["allow_null"], true);
    assert_eq!(
        declaration["payload"]["input_type"]["fields"][0]["input_type"]["variants"][1]["fields"],
        json!([])
    );
    let source = target_packet(&batches, "source_read");
    let prepare = source["state"]["capabilities"]
        .as_object()
        .unwrap()
        .values()
        .find(|card| card["capability"]["reference"]["capability"] == "prepare")
        .unwrap();
    let slots = prepare["slots"].as_array().unwrap();
    assert_eq!(
        slots.len(),
        1,
        "only the ordinary required argument has a projected source binding"
    );
    assert_eq!(
        slots[0]["input"],
        json!({"lane":"arguments","path":["resource"]})
    );
    assert!(prepare["declaration"].get("payload").is_none());
    assert_eq!(prepare["declaration"]["output"], declaration["output"]);
}
