use super::*;

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
    let display = codec.display(&observed);
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
    assert_eq!(codec.display(&projected)["derived_alias"], 42);
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
                    &serde_json::json!({"_ref":"authored nested", "_version":7}),
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
    let display = codec.display(&row);
    assert_eq!(display["_ref"], "authored");
    assert_eq!(display["payload"]["_ref"], "authored nested");
    assert!(display.get("_unavailable_fields").is_none());
    assert!(display.get("missing").is_none());
    assert!(codec.payload(&row).get("missing").is_none());
    assert!(
        codec.encode(&row)["_ref"].is_object(),
        "machine serialization retains structural identity"
    );
}
