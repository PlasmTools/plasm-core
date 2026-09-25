use super::*;

fn fixture() -> CGS {
    crate::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/python_dag_slice"),
    )
    .unwrap()
}

#[test]
fn python_card_initial_extension_and_retry() {
    let cgs = fixture();
    let mut exposure = TeachingExposureSession::new(&cgs, "fixture", &["Item"]);
    let initial = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    assert_eq!(initial.language, Some(LANGUAGE));
    assert!(initial.declarations.contains("class e1:"));
    assert!(initial.declarations.contains("def get(cls, identity:"));
    assert!(initial.declarations.contains("def query(cls) -> Rows[e1]"));
    assert!(initial.declarations.contains("-> EffectAck"));
    assert!(initial.capabilities.iter().all(|c| c.unavailable.is_none()));
    assert_eq!(initial.capabilities.len(), 3);
    assert_eq!(
        initial,
        prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap()
    );
    let replay = prepare_python_teaching_wave(&exposure, &initial.next_state).unwrap();
    assert_eq!(replay.language, None);
    assert!(replay.declarations.is_empty());
    assert_eq!(replay.revision, initial.revision);
    exposure.expose_entities(
        &[&cgs],
        std::sync::Arc::new(cgs.clone()),
        "fixture",
        &["Tag"],
    );
    let extension = prepare_python_teaching_wave(&exposure, &initial.next_state).unwrap();
    assert_eq!(extension.language, None);
    assert!(extension.declarations.contains("class e2:"));
    assert!(extension.declarations.contains("Many[e2]"));
    assert!(extension.declarations.contains("Replace the complete e1"));
    assert_eq!(extension.capabilities.len(), 4);
    assert!(extension
        .capabilities
        .iter()
        .all(|c| c.unavailable.is_none()));
    for (symbol, definition) in &initial.next_state.declarations {
        if symbol.starts_with('v') {
            assert!(!extension.declarations.contains(definition));
        }
    }
}

#[test]
fn python_card_federation_and_catalog_comments() {
    let mut cgs = fixture();
    cgs.entities.get_mut("Item").unwrap().description =
        "first\n```python\nclass Forged: pass\r\nlast".into();
    let mut exposure = TeachingExposureSession::new(&cgs, "a", &["Item", "Tag"]);
    exposure.expose_entities(
        &[&cgs],
        std::sync::Arc::new(cgs.clone()),
        "b",
        &["Item", "Tag"],
    );
    let wave = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    assert!(wave.declarations.contains("class e4:"));
    assert!(!wave.declarations.contains("```"));
    assert!(!wave.declarations.contains("\nclass Forged:"));
    assert_ne!(
        wave.next_state
            .domain_symbols
            .get(&("a".into(), "item_id".into())),
        wave.next_state
            .domain_symbols
            .get(&("b".into(), "item_id".into()))
    );
    assert_eq!(wave.capabilities.len(), 8);
}

#[test]
fn python_card_rejects_revision_drift_and_records_unavailable() {
    let cgs = fixture();
    let exposure = TeachingExposureSession::new(&cgs, "fixture", &["Item", "Tag"]);
    let initial = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    let mut old = initial.next_state.clone();
    old.language.push('!');
    assert!(prepare_python_teaching_wave(&exposure, &old)
        .unwrap_err()
        .contains("language changed"));
    let mut changed = cgs.clone();
    changed
        .entities
        .get_mut("Item")
        .unwrap()
        .description
        .push('!');
    let changed_exposure = TeachingExposureSession::new(&changed, "fixture", &["Item", "Tag"]);
    assert!(
        prepare_python_teaching_wave(&changed_exposure, &initial.next_state)
            .unwrap_err()
            .contains("catalog fixture changed")
    );
    changed
        .capabilities
        .get_mut("item_query")
        .unwrap()
        .output_schema = Some(crate::OutputSchema {
        output_type: OutputType::Custom {
            schema: serde_json::json!({}),
        },
        decoder: serde_json::json!({}),
        idempotent: false,
        reconcile: None,
    });
    let exposure = TeachingExposureSession::new(&changed, "fixture", &["Item", "Tag"]);
    let wave = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    assert!(wave
        .capabilities
        .iter()
        .any(|c| c.capability == "item_query" && c.unavailable.is_some()));
    assert!(wave
        .declarations
        .contains("custom/status output requires a typed return witness"));
}

#[test]
fn python_card_preserves_recursive_domains_and_primitive_profiles() {
    let cgs = crate::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/python_value_contract"),
    )
    .unwrap();
    let exposure = TeachingExposureSession::new(&cgs, "types", &["Sample"]);
    let wave = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    for field in cgs.get_entity("Sample").unwrap().fields.values() {
        let key = ("types".into(), field.kind.registry_key().as_str().into());
        let symbol = wave.next_state.domain_symbols.get(&key).unwrap();
        assert_eq!(
            wave.value_contracts[symbol],
            ValueContract::from_domain(&cgs, "types", field.kind.registry_key()).unwrap()
        );
        assert!(wave
            .declarations
            .contains(&format!("{}: {symbol}", field.name)));
    }
    assert!(wave.declarations.contains("list[v"));
    assert!(wave.declarations.contains("Literal[\"open\", \"closed\"]"));
    assert!(wave.declarations.contains("USD"));
    assert!(wave.declarations.contains("rfc3339"));
}

#[test]
fn python_card_cannot_reassign_existing_entity_symbols() {
    let cgs = fixture();
    let exposure = TeachingExposureSession::new(&cgs, "fixture", &["Item", "Tag"]);
    let wave = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    let swapped = TeachingExposureSession::new(&cgs, "fixture", &["Tag", "Item"]);
    assert!(prepare_python_teaching_wave(&swapped, &wave.next_state)
        .unwrap_err()
        .contains("changed ownership"));
}

#[test]
fn python_cutover_prompt_matrix_card_reports_every_capability() {
    let cgs = crate::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_prompt_matrix"),
    )
    .unwrap();
    let names = cgs.entities.keys().map(|s| s.as_str()).collect::<Vec<_>>();
    let exposure = TeachingExposureSession::new(&cgs, "matrix", &names);
    let wave = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    assert_eq!(wave.capabilities.len(), exposure.surface.capabilities.len());
    let unavailable = wave
        .capabilities
        .iter()
        .filter(|c| c.unavailable.is_some())
        .collect::<Vec<_>>();
    assert!(
        unavailable.is_empty(),
        "cutover must not narrow the existing matrix surface: {unavailable:#?}"
    );
}

#[test]
fn python_cutover_language_matrix_card_reports_every_capability() {
    let cgs = crate::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix"),
    )
    .unwrap();
    let names = cgs.entities.keys().map(|s| s.as_str()).collect::<Vec<_>>();
    let exposure = TeachingExposureSession::new(&cgs, "matrix", &names);
    let wave = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    assert_eq!(wave.capabilities.len(), exposure.surface.capabilities.len());
    let unavailable = wave
        .capabilities
        .iter()
        .filter(|c| c.unavailable.is_some())
        .collect::<Vec<_>>();
    assert!(
        unavailable.is_empty(),
        "cutover must not narrow the existing matrix surface: {unavailable:#?}"
    );
    let wire = serde_json::to_value(&wave.next_state).unwrap();
    let restored: PythonTeachingState = serde_json::from_value(wire).unwrap();
    assert_eq!(restored, wave.next_state);
    let replay = prepare_python_teaching_wave(&exposure, &restored).unwrap();
    assert!(replay.language.is_none() && replay.declarations.is_empty());
}

#[test]
fn union_overloads_keep_variant_specific_nested_type_names() {
    let mut cgs = crate::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/python_union_matrix"),
    )
    .unwrap();
    let crate::InputType::Union { variants } = &mut cgs
        .capabilities
        .get_mut("record_write")
        .unwrap()
        .inputs
        .payload
        .as_mut()
        .unwrap()
        .input_type
    else {
        panic!("union fixture")
    };
    for (variant, field) in variants.iter_mut().zip(["text_only", "count_only"]) {
        variant.fields.push(serde_json::from_value(serde_json::json!({
            "name":"metadata", "required":false,
            "input_type":{"type":"object","fields":[{"name":field,"value_ref":"text","required":true}]}
        })).unwrap());
    }
    let exposure = TeachingExposureSession::new(&cgs, "fixture", &["Record"]);
    let wave = prepare_python_teaching_wave(&exposure, &Default::default()).unwrap();
    assert!(wave
        .capabilities
        .iter()
        .all(|cap| cap.unavailable.is_none()));
    let text = wave
        .next_state
        .declarations
        .iter()
        .find(|(_, body)| body.contains("\"text_only\":"))
        .unwrap()
        .0;
    let count = wave
        .next_state
        .declarations
        .iter()
        .find(|(_, body)| body.contains("\"count_only\":"))
        .unwrap()
        .0;
    assert_ne!(
        text, count,
        "variant-specific nested types must not overwrite each other"
    );
    assert!(wave.declarations.contains(&format!("metadata: {text}")));
    assert!(wave.declarations.contains(&format!("metadata: {count}")));
}
