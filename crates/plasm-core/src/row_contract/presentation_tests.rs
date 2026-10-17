use super::*;

#[test]
fn invalid_nested_payload_fails_storage_and_presentation() {
    let row = RowRecord {
        identity: Ref::new("Record", "id"),
        fields: IndexMap::from([(
            "payload".into(),
            TypedFieldValue::Json(crate::Value::Array(vec![crate::Value::Float(f64::NAN)])),
        )]),
        relations: IndexMap::new(),
        unavailable_fields: Default::default(),
    };
    let codec = RowCodec::new(None);
    for result in [codec.encode(&row), codec.display(&row), codec.payload(&row)] {
        assert!(matches!(
            result,
            Err(crate::ValueCodecError::Encoding(
                crate::WireEncodingError::NonFinite
            ))
        ));
    }
}

#[test]
fn qualified_reference_projection_retains_owning_catalog_identity() {
    let mut cgs = crate::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix"),
    )
    .unwrap();
    cgs.entry_id = Some("matrix".into());
    let key = cgs.get_entity("LangItem").unwrap().fields["title"]
        .kind
        .registry_key()
        .as_str()
        .to_owned();
    cgs.values.get_mut(&key).unwrap().field_type = crate::FieldType::EntityRef {
        entry_id: "matrix".into(),
        target: "LangLine".into(),
    };
    let value = crate::json_value_to_plasm_value(&serde_json::json!({
        "_ref": {"kind":"simple","entity":"LangLine","id":"line-1"},
        "id":"untrusted", "_version":1
    }));
    assert_eq!(
        RowCodec::new(Some(&cgs))
            .public_field_value("LangItem", "title", &value)
            .unwrap(),
        crate::Value::String("line-1".into())
    );
}

#[test]
fn nested_relation_projection_uses_declared_targets_without_mutating_storage() {
    let cgs = crate::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix"),
    )
    .unwrap();
    let codec = RowCodec::new(Some(&cgs));
    let value = crate::json_value_to_plasm_value(&serde_json::json!({
        "id":"summary", "headline":"headline", "_ref":{"kind":"simple","entity":"LangSummary","id":"summary"},
        "_version":1, "detail":{"id":"detail", "body":"authored", "_last_updated":8,"_completeness":"complete"}
    }));
    let projected = codec
        .public_field_value("LangItem", "summary", &value)
        .unwrap();
    assert_eq!(
        crate::try_plasm_value_to_json(&projected).unwrap(),
        serde_json::json!({"id":"summary","headline":"headline","detail":{"id":"detail","body":"authored"}})
    );
    assert!(value.get("_ref").is_some());
    assert_eq!(
        codec
            .public_field_value("LangItem", "program_alias", &value)
            .unwrap(),
        value
    );
}

#[test]
fn observations_and_projections_have_explicit_field_ownership() {
    let cgs = crate::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix"),
    )
    .unwrap();
    let codec = RowCodec::new(Some(&cgs));
    let source = crate::json_value_to_plasm_value(&serde_json::json!({
        "_ref": {"kind":"simple","entity":"LangItem","id":"a"},
        "id":"a", "title":"domain", "derived_alias":42,
        "wire_embed":{"email":"identity","_ref":{"kind":"simple","entity":"User","id":"identity"}}
    }));
    let source = ValueRow::try_from(source).unwrap();
    let observed = codec
        .decode_values("LangItem", &source, RowFieldContract::Observation)
        .unwrap();
    let display = codec.display(&observed).unwrap();
    assert_eq!(display["title"], "domain");
    assert!(display.get("wire_embed").is_none());
    assert!(display.get("derived_alias").is_none());
    assert!(
        source.get("wire_embed").is_some(),
        "execution traversal evidence must remain intact"
    );
    let projected = codec
        .decode_values("LangItem", &source, RowFieldContract::Projection)
        .unwrap();
    assert_eq!(codec.display(&projected).unwrap()["derived_alias"], 42);
}

#[test]
fn presentation_does_not_serialize_execution_identity_or_scrub_authored_json() {
    let row = RowRecord {
        identity: Ref::new("Example", "a"),
        fields: IndexMap::from([
            ("_ref".into(), TypedFieldValue::String("authored".into())),
            (
                "payload".into(),
                TypedFieldValue::Json(crate::json_value_to_plasm_value(
                    &serde_json::json!({"_ref":"authored nested", "_version":7, "__plasm_money":"opaque", "__plasm_unsigned":"also opaque"}),
                )),
            ),
            (
                "missing".into(),
                TypedFieldValue::String("retained preview".into()),
            ),
        ]),
        relations: IndexMap::new(),
        unavailable_fields: ["missing".into()].into(),
    };
    let codec = RowCodec::new(None);
    let display = codec.display(&row).unwrap();
    assert_eq!(display["_ref"], "authored");
    assert_eq!(display["payload"]["_ref"], "authored nested");
    assert_eq!(codec.payload(&row).unwrap()["payload"], display["payload"]);
    assert_eq!(display["payload"]["__plasm_money"], "opaque");
    assert!(display.get("_unavailable_fields").is_none());
    assert!(display.get("missing").is_none());
    assert!(codec.payload(&row).unwrap().get("missing").is_none());
    assert!(
        codec.encode(&row).unwrap()["_ref"].is_object(),
        "machine serialization retains structural identity"
    );
}
