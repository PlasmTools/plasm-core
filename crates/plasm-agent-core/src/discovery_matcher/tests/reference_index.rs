use super::*;
use crate::discovery_store::RetrievedCapability;

fn matrix() -> (BTreeMap<String, plasm_core::CGS>, RetrievalReceipt) {
    let cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
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
fn reference_index_preserves_composition_full_contracts_and_opaque_outputs() {
    let (catalogs, retrieval) = matrix();
    let evidence = CatalogEvidence::from_catalogs(&retrieval, &catalogs).unwrap();
    let intent =
        IntentProvenance::from_turns(["Process the source; preserve originals.".into()]).unwrap();
    let batches =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    let body: serde_json::Value = serde_json::from_str(batches[0].body()).unwrap();
    let state = &body["state"];
    assert!(state.get("context_whitespace").is_none());
    assert_eq!(state["types"]["values"].as_object().unwrap().len(), 4);
    assert_eq!(state["types"]["records"].as_object().unwrap().len(), 3);
    let mut ids = BTreeMap::new();
    for (id, card) in state["capabilities"].as_object().unwrap() {
        assert!(card.get("types").is_none());
        assert!(card.get("type_refs").is_none());
        let name = card["capability"]["reference"]["capability"]
            .as_str()
            .unwrap();
        ids.insert(name, id);
        let candidate = retrieval
            .candidates
            .iter()
            .find(|c| c.reference.capability == name)
            .unwrap();
        assert_eq!(
            card["capability"]["operation"]["contract"],
            candidate.document.operation.contract
        );
        for section in ["values", "records"] {
            for name in state["type_refs"][id][section].as_array().unwrap() {
                assert!(!state["types"][section][name.as_str().unwrap()].is_null());
            }
        }
    }
    let caps = &state["capabilities"];
    assert_eq!(
        caps[ids["prepare"]]["declaration"]["arguments"]["input_type"]["fields"][0]["value_ref"],
        "source"
    );
    assert_eq!(
        caps[ids["process"]]["declaration"]["arguments"]["input_type"]["fields"][0]["value_ref"],
        "prepared"
    );
    assert_eq!(
        caps[ids["opaque"]]["declaration"]["output"]["type"],
        "side_effect"
    );
    assert!(caps[ids["opaque"]]["declaration"]["output"]
        .get("entity_type")
        .is_none());
    assert_eq!(caps[ids["opaque"]]["declaration"]["provides"], json!([]));
    for part in repartition(&batches[0], &intent).unwrap() {
        let child: serde_json::Value = serde_json::from_str(part.body()).unwrap();
        assert_eq!(state, &child["state"]);
    }
    let decoded = decode_batch(
        &batches[0],
        &response(
            &batches[0],
            &vec![MatchChoice::Uncertain; retrieval.candidates.len()],
        ),
    )
    .unwrap();
    let selected = finish_selection(decoded, &retrieval, &batches)
        .unwrap()
        .selected(&retrieval);
    assert_eq!(selected.len(), retrieval.candidates.len());
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
    let evidence = CatalogEvidence::from_catalogs(&retrieval, &catalogs).unwrap();
    let intent = IntentProvenance::from_turns(["Process the source.".into()]).unwrap();
    let before =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    let body: serde_json::Value = serde_json::from_str(before[0].body()).unwrap();
    assert_ne!(
        body["state"]["types"]["values"]["matrix/source"],
        body["state"]["types"]["values"]["other/source"]
    );
    catalogs
        .get_mut("matrix")
        .unwrap()
        .values
        .get_mut("source")
        .unwrap()
        .description = "Revised authored definition.".into();
    let revised = CatalogEvidence::from_catalogs(&retrieval, &catalogs).unwrap();
    let after =
        super::super::issue_batches(RELEVANCE_MODEL, &intent, &retrieval, &revised).unwrap();
    assert_ne!(before[0].cache_key(), after[0].cache_key());
    catalogs.remove("other");
    assert!(matches!(
        CatalogEvidence::from_catalogs(&retrieval, &catalogs),
        Err(DiscoveryMatcherError::MissingCatalogueEvidence(_))
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
    let evidence = CatalogEvidence::from_catalogs(&retrieval, &catalogs).unwrap();
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
    let references = serde_json::to_value(&cap.references).unwrap();
    assert!(references["values"]
        .as_array()
        .unwrap()
        .contains(&json!("matrix/sources")));
    catalogs
        .get_mut("matrix")
        .unwrap()
        .values
        .shift_remove("source");
    assert!(matches!(
        CatalogEvidence::from_catalogs(&retrieval, &catalogs),
        Err(DiscoveryMatcherError::MissingEvidenceDefinition { .. })
    ));
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
        let evidence = CatalogEvidence::from_catalogs(&retrieval, &catalogs).unwrap();
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
    }
}
