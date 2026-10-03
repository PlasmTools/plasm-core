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
    assert!(initial.declarations.contains("e1:"));
    assert!(initial.declarations.contains("e1.get(identity:"));
    assert!(initial.declarations.contains("e1.query() -> Rows[e1]"));
    assert!(initial.declarations.contains("-> EffectAck"));
    assert!(!initial.declarations.contains("(*,"));
    assert!(
        initial
            .capabilities
            .iter()
            .filter_map(|cap| cap.signature.as_ref())
            .any(|signature| signature.contains("*,")),
        "reference compaction must preserve underlying signatures"
    );
    assert!(initial.capabilities.iter().all(|c| c.unavailable.is_none()));
    assert_eq!(initial.capabilities.len(), 5);
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
    assert!(extension.declarations.contains("e2:"));
    assert!(extension.declarations.contains("Many[e2]"));
    assert!(extension.declarations.contains("Add members to e1"));
    assert!(!extension.declarations.contains("e1.get("));
    assert!(!extension.declarations.contains("e1.query("));
    assert_eq!(
        extension,
        prepare_python_teaching_wave(&exposure, &initial.next_state).unwrap()
    );
    let replay = prepare_python_teaching_wave(&exposure, &extension.next_state).unwrap();
    assert!(replay.declarations.is_empty());
    let restored: PythonTeachingState =
        serde_json::from_str(&serde_json::to_string(&extension.next_state).unwrap()).unwrap();
    assert!(prepare_python_teaching_wave(&exposure, &restored)
        .unwrap()
        .declarations
        .is_empty());
    assert_eq!(extension.capabilities.len(), 6);
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
fn python_query_card_advertises_only_source_call_inputs() {
    let cgs = crate::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_prompt_matrix"),
    )
    .unwrap();
    let exposure = TeachingExposureSession::new(&cgs, "matrix", &["Zone"]);
    let wave = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    let signature = wave
        .capabilities
        .iter()
        .find(|cap| cap.capability == "zone_query")
        .unwrap()
        .signature
        .as_deref()
        .unwrap();
    assert!(signature.contains("account_id:"), "{signature}");
    assert!(!signature.contains("sort_by:"), "{signature}");
    assert!(!signature.contains("page:"), "{signature}");

    let mut source = crate::QueryExpr::filtered("Zone", crate::Predicate::eq("sort_by", "name"));
    source.capability_name = Some("zone_query".into());
    let rejection = crate::normalize_query_expr_to_rowset(&source, &cgs, "matrix").unwrap_err();
    assert!(rejection.contains("RA-1"), "{rejection}");
}

#[test]
fn python_language_distinguishes_dag_results_from_compute_inputs() {
    for shape in [PythonRowShape::Rows, PythonRowShape::Singleton] {
        assert!(LANGUAGE.contains(&shape.card_annotation("eN")));
        assert!(LANGUAGE.contains(&shape.materialized_annotation("eN")));
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
    assert!(wave.declarations.contains("e4:"));
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
    assert_eq!(wave.capabilities.len(), 12);
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
    let temporal = wave
        .declarations
        .lines()
        .find(|line| line.contains("rfc3339"))
        .unwrap();
    assert!(temporal.starts_with('v'), "{temporal}");
    assert!(temporal.contains("@rfc3339"), "{temporal}");
    assert!(!temporal.contains("types::"), "{temporal}");
    assert!(temporal.contains(": datetime"), "{temporal}");
    let date = wave
        .declarations
        .lines()
        .find(|line| line.contains("@iso8601_date"))
        .unwrap();
    assert!(date.contains(": date"), "{date}");
    assert!(!wave.declarations.contains("evaluation_now"));
    assert!(!wave.declarations.contains("str | int"));
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

#[test]
fn compact_domain_card_retains_types_without_repeating_shared_descriptions() {
    let cgs = fixture();
    let exposure = TeachingExposureSession::new(&cgs, "fixture", &["Item", "Tag"]);
    let wave = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    assert_eq!(wave.declarations.matches("Human label.").count(), 1);
    assert!(wave.declarations.contains("Literal[\"open\", \"closed\"]"));
    assert!(wave.declarations.contains("content: v"));
    assert!(wave.declarations.contains("-> EffectAck"));
    assert!(wave.declarations.contains("-> Singleton[e1]"));
    assert!(!wave.declarations.contains("@classmethod"));
    assert!(!wave.declarations.contains("provides:"));
}

#[test]
fn compact_card_groups_identical_cgs_comments_with_explicit_owners() {
    let mut cgs = fixture();
    for name in ["item_touch", "item_publish"] {
        let cap = cgs.capabilities.get_mut(name).unwrap();
        if let OutputType::SideEffect { description } =
            &mut cap.output_schema.as_mut().unwrap().output_type
        {
            *description = "Shared effect contract.".into();
        }
    }
    let exposure = TeachingExposureSession::new(&cgs, "fixture", &["Item"]);
    let wave = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    assert_eq!(
        wave.declarations.matches("Shared effect contract.").count(),
        1
    );
    let shared = wave
        .declarations
        .lines()
        .find(|line| line.contains("Shared effect contract."))
        .unwrap();
    assert!(shared.split("#").next().unwrap().contains(','), "{shared}");
    assert!(wave
        .declarations
        .contains("Record a root effect before or after correlated child reads."));
    assert!(wave.declarations.contains("Store a derived document."));
    assert!(!wave.declarations.contains("def "));
    assert!(!wave.declarations.contains("class e"));
    assert_eq!(wave.declarations.matches("-> EffectAck").count(), 3);
}

#[test]
fn compact_card_omits_internal_provides_metadata() {
    let mut cgs = fixture();
    cgs.capabilities.get_mut("item_query").unwrap().provides = vec!["id".into()];
    let exposure = TeachingExposureSession::new(&cgs, "fixture", &["Item"]);
    let wave = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    assert!(!wave.declarations.contains("provides:"));
    assert_eq!(cgs.capabilities["item_query"].provides, vec!["id"]);
    assert!(!wave.declarations.contains("provides: id, title, state"));
}

#[test]
fn compact_comments_preserve_prose_and_contain_catalog_control_characters() {
    assert_eq!(comment_text("say \"hello\""), "say \"hello\"");
    let escaped = comment_text("first\n```\tlast");
    assert!(!escaped.contains('\n'));
    assert!(!escaped.contains('`'));
    assert!(!escaped.contains('\t'));
    let cgs = fixture();
    let exposure = TeachingExposureSession::new(&cgs, "fixture", &["Item", "Tag"]);
    let wave = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    assert!(!wave.declarations.contains("# CGS"));
    for symbol in wave.value_contracts.keys() {
        assert!(wave
            .declarations
            .lines()
            .any(|line| line.starts_with(&format!("{symbol}: "))));
    }
}

#[test]
fn compact_card_keeps_semantics_and_structured_ownership_without_internal_names() {
    let cgs = fixture();
    let exposure = TeachingExposureSession::new(&cgs, "fixture", &["Item", "Tag"]);
    let wave = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    assert!(!wave.declarations.contains("materialize:"));
    assert!(!wave.declarations.contains("\n\n"));
    assert!(!wave.declarations.contains("provides:"));
    assert!(!wave.declarations.contains("# item_query:"));
    assert!(!wave.declarations.contains("(*,"));
    assert!(!wave.declarations.contains("length None"));
    for (key, symbol) in &wave.next_state.domain_symbols {
        assert_eq!(key.0, "fixture");
        assert!(wave.value_contracts.contains_key(symbol));
        assert!(!wave.declarations.contains(&format!("fixture::{}", key.1)));
    }
    let next = prepare_python_teaching_wave(&exposure, &wave.next_state).unwrap();
    assert!(next.declarations.is_empty());
    assert!(next.language.is_none());
}

#[test]
fn payload_defaults_and_secondary_effects_survive_teaching() {
    let mut cgs = fixture();
    let cap = cgs.capabilities.get_mut("item_publish").unwrap();
    let crate::InputType::Object { fields, .. } =
        &mut cap.inputs.payload.as_mut().unwrap().input_type
    else {
        panic!("object fixture");
    };
    for (name, default) in [
        ("disabled", serde_json::json!(false)),
        ("enabled", serde_json::json!(true)),
        ("count", serde_json::json!(0)),
        ("label", serde_json::json!("")),
    ] {
        fields.push(serde_json::from_value(serde_json::json!({
            "name": name, "required": false, "default": default,
            "input_type": {"type": "value", "field_type": if default.is_boolean() {"boolean"} else if default.is_number() {"integer"} else {"string"}},
            "description": "Omission uses the service default and updates revision metadata."
        })).unwrap());
    }
    let exposure = TeachingExposureSession::new(&cgs, "fixture", &["Item"]);
    let wave = prepare_python_teaching_wave(&exposure, &Default::default()).unwrap();
    for expected in [
        "disabled default: Bool(false)",
        "enabled default: Bool(true)",
        "count default: Integer(0)",
        "label default: String(\"\")",
        "updates revision metadata",
    ] {
        assert!(
            wave.declarations.contains(expected),
            "missing {expected}: {}",
            wave.declarations
        );
    }
    assert!(!wave.declarations.contains("content default:"));
}

#[test]
fn python_card_operation_extension_is_additive_and_complete() {
    let cgs = fixture();
    let select = |name: &str| {
        crate::capability_exposure::selected_capability_surface(&cgs, "fixture", &[name.to_owned()])
            .unwrap()
    };
    let mut exposure = TeachingExposureSession::new_with_intent_delta(
        &cgs,
        "fixture",
        &["Item"],
        select("item_query"),
    );
    let initial = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    exposure.expose_surface(
        &[&cgs],
        std::sync::Arc::new(cgs.clone()),
        "fixture",
        &["Item"],
        select("item_mark"),
    );
    let extension = prepare_python_teaching_wave(&exposure, &initial.next_state).unwrap();
    assert!(extension.declarations.contains("Add members to e1"));
    assert!(!extension.declarations.contains("e1.query("));
    assert!(!extension.declarations.contains("    id:"));
    assert!(extension.declarations.contains("-> EffectAck"));
    assert_eq!(
        extension.capabilities.len(),
        2,
        "coverage retains all selected operations"
    );
    assert!(extension.capabilities.iter().all(|c| c.signature.is_some()));
    let complete = &extension.next_state.declarations["e1"];
    assert!(complete.contains("e1.query("));
    assert!(complete.contains("-> EffectAck"));
    assert!(
        extension.declarations.len() < complete.len(),
        "extension must not resend the entity"
    );
    println!(
        "member delta: {} bytes versus {} bytes for the full entity",
        extension.declarations.len(),
        complete.len()
    );
    assert!(
        prepare_python_teaching_wave(&exposure, &extension.next_state)
            .unwrap()
            .declarations
            .is_empty()
    );
}
