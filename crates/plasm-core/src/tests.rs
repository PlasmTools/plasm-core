#[cfg(test)]
mod property_tests {
    use crate::*;
    use proptest::prelude::*;

    // Property-based test generators
    prop_compose! {
        fn arb_simple_predicate()(
            field in "[a-z]+",
            value in any::<i32>()
        ) -> Predicate {
            Predicate::eq(field, value)
        }
    }

    prop_compose! {
        fn arb_and_predicate()(
            predicates in prop::collection::vec(arb_simple_predicate(), 1..5)
        ) -> Predicate {
            Predicate::and(predicates)
        }
    }

    proptest! {
        #[test]
        fn test_normalization_idempotency(pred in arb_and_predicate()) {
            if let Ok(normalized1) = normalize(pred.clone()) {
                if let Ok(normalized2) = normalize(normalized1.clone()) {
                    prop_assert_eq!(normalized1, normalized2);
                }
            }
        }

        #[test]
        fn test_predicate_depth_bounded(pred in arb_and_predicate()) {
            prop_assert!(pred.depth() < 20); // Should stay reasonable
        }

        #[test]
        fn test_field_references_stable(pred in arb_simple_predicate()) {
            let fields1 = pred.referenced_fields();
            let fields2 = pred.referenced_fields();
            prop_assert_eq!(fields1, fields2);
        }

        #[test]
        fn test_serialization_round_trip(pred in arb_simple_predicate()) {
            if let Ok(json) = serde_json::to_string(&pred) {
                if let Ok(parsed) = serde_json::from_str::<Predicate>(&json) {
                    prop_assert_eq!(pred, parsed);
                }
            }
        }
    }

    #[test]
    fn test_type_checking_stability() {
        // Create a known schema and predicate
        let mut cgs = CGS::new();
        cgs.values.insert(
            "fx_str".into(),
            crate::schema::NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
                currency: None,
            },
        );
        cgs.values.insert(
            "fx_region".into(),
            crate::schema::NamedValueSchema {
                description: String::new(),
                field_type: FieldType::Select,
                value_format: None,
                allowed_values: Some(vec!["EMEA".to_string(), "APAC".to_string()]),
                string_semantics: None,
                array_items: None,
                currency: None,
            },
        );
        let account = ResourceSchema {
            name: "Account".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                crate::schema::registry_test_util::entity_field_from_values(
                    &cgs, "fx_str", "id", true, "",
                ),
                crate::schema::registry_test_util::entity_field_from_values(
                    &cgs,
                    "fx_region",
                    "region",
                    false,
                    "",
                ),
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        };
        cgs.add_resource(account).unwrap();

        let predicate = Predicate::eq("region", "EMEA");
        let entity = cgs.get_entity("Account").unwrap();

        // Type check multiple times - should be consistent
        let result1 = type_check_predicate(&predicate, entity, &[], &cgs);
        let result2 = type_check_predicate(&predicate, entity, &[], &cgs);
        let result3 = type_check_predicate(&predicate, entity, &[], &cgs);

        assert!(result1.is_ok());
        assert!(result2.is_ok());
        assert!(result3.is_ok());
    }
}
