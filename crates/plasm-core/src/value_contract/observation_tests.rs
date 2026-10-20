use super::*;

fn cgs() -> CGS {
    crate::loader::load_schema_dir(
        &std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_language_matrix"),
    )
    .unwrap()
}

#[test]
fn ambiguous_projection_identifies_both_matching_contracts() {
    let left = ValueContract::record(
        BTreeMap::from([("left".into(), ValueContract::scalar(FieldType::String))]),
        Default::default(),
    );
    let right = ValueContract::record(
        BTreeMap::from([("right".into(), ValueContract::scalar(FieldType::Integer))]),
        Default::default(),
    );
    let contract = ValueContract {
        shape: ValueShape::Union {
            variants: vec![left.clone(), right.clone()],
        },
        nullable: false,
        domain: None,
    };
    let value = crate::json_value_to_plasm_value(&serde_json::json!({"left":"x","right":1}));
    let error = contract
        .observed_value(&value, &cgs(), "matrix")
        .unwrap_err();
    assert!(
        matches!(&error, ValueContractError::AmbiguousUnionProjection { first, other } if **first == left && **other == right)
    );
    let message = error.to_string();
    assert!(
        message.contains("left") && message.contains("right"),
        "{message}"
    );
}

#[test]
fn closed_record_projection_excludes_storage_fields_but_keeps_declared_names() {
    let record = ValueContract::record(
        BTreeMap::from([
            ("title".into(), ValueContract::scalar(FieldType::String)),
            ("_version".into(), ValueContract::scalar(FieldType::Integer)),
        ]),
        Default::default(),
    );
    let value = crate::json_value_to_plasm_value(&serde_json::json!({
        "title": "data", "_version": 7, "_ref": "storage", "_completeness": "complete"
    }));
    let projected = record.observed_value(&value, &cgs(), "matrix").unwrap();
    assert_eq!(
        crate::try_plasm_value_to_json(&projected).unwrap(),
        serde_json::json!({"title":"data","_version":7})
    );
    assert!(record.validate(&projected, &cgs(), "matrix", "row").is_ok());
    assert!(
        value.get("_ref").is_some(),
        "projection must not mutate storage"
    );
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(48))]
    #[test]
    fn nested_record_observation_preserves_shape_and_open_dictionary_data(
        names in proptest::collection::vec("[a-z]{0,20}", 0..16)
    ) {
        let dictionary = ValueContract { shape: ValueShape::Dictionary {
            key: Box::new(ValueContract::scalar(FieldType::String)),
            value: Box::new(ValueContract::scalar(FieldType::String)),
        }, nullable:false, domain:None };
        let row = ValueContract::record(BTreeMap::from([
            ("title".into(), ValueContract::scalar(FieldType::String)),
            ("payload".into(), dictionary),
        ]), Default::default());
        let contract = ValueContract {shape:ValueShape::Array { element: Box::new(
            ValueContract {shape:ValueShape::Array {element:Box::new(row)},nullable:false,domain:None}
        )},nullable:false,domain:None};
        let raw = serde_json::json!([names.iter().map(|name| serde_json::json!({
            "title":name,"_ref":"storage","_completeness":"complete",
            "payload":{"_ref":"authored","_custom":"data"}
        })).collect::<Vec<_>>()]);
        let value = crate::json_value_to_plasm_value(&raw);
        let projected = contract.observed_value(&value, &cgs(), "matrix").unwrap();
        let expected = serde_json::json!([names.iter().map(|name| serde_json::json!({
            "title":name,"payload":{"_ref":"authored","_custom":"data"}
        })).collect::<Vec<_>>()]);
        proptest::prop_assert_eq!(crate::try_plasm_value_to_json(&projected).unwrap(), expected);
    }
}
