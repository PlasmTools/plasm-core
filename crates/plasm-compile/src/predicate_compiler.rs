use crate::{BackendFilter, BackendOp, CompileError};
use plasm_core::{
    normalize, resolve_query_capability, type_check_predicate, CapabilityKind, EntityDef,
    InputFieldSchema, Predicate, CGS,
};

/// Compile a predicate to a backend filter.
///
/// Capability parameters that are not entity fields (e.g. `q`, `t`, `s` with
/// `role: search`) are NOT compiled to `BackendFilter` nodes — they go straight
/// into the CML environment via `extract_predicate_vars`. They produce
/// `BackendFilter::True` (no-op filter) here so the BackendFilter only captures
/// genuine entity-field predicates.
pub fn compile_predicate(
    predicate: &Predicate,
    entity: &EntityDef,
    cgs: &CGS,
) -> Result<BackendFilter, CompileError> {
    compile_predicate_with_cap_params(predicate, entity, &[], cgs)
}

/// Internal version that accepts resolved capability parameters.
fn compile_predicate_with_cap_params(
    predicate: &Predicate,
    entity: &EntityDef,
    cap_params: &[InputFieldSchema],
    cgs: &CGS,
) -> Result<BackendFilter, CompileError> {
    // Normalize first
    let normalized = normalize(predicate.clone())?;

    // Type-check using both entity fields and capability parameters
    type_check_predicate(&normalized, entity, cap_params, cgs)?;

    // Compile
    compile_predicate_internal(&normalized, entity, cap_params, cgs)
}

fn compile_predicate_internal(
    predicate: &Predicate,
    entity: &EntityDef,
    cap_params: &[InputFieldSchema],
    cgs: &CGS,
) -> Result<BackendFilter, CompileError> {
    match predicate {
        Predicate::True => Ok(BackendFilter::True),

        Predicate::False => Ok(BackendFilter::False),

        Predicate::Comparison { field, op, value } => {
            compile_comparison(field, *op, value, entity, cap_params)
        }

        Predicate::And { args } => {
            let filters = args
                .iter()
                .map(|arg| compile_predicate_internal(arg, entity, cap_params, cgs))
                .collect::<Result<Vec<_>, _>>()?;

            Ok(BackendFilter::and(filters).simplify())
        }

        Predicate::Or { args } => {
            let filters = args
                .iter()
                .map(|arg| compile_predicate_internal(arg, entity, cap_params, cgs))
                .collect::<Result<Vec<_>, _>>()?;

            Ok(BackendFilter::or(filters).simplify())
        }

        Predicate::Not { predicate } => {
            let filter = compile_predicate_internal(predicate, entity, cap_params, cgs)?;
            Ok(BackendFilter::negate(filter).simplify())
        }

        Predicate::ExistsRelation {
            relation,
            predicate,
        } => compile_relation(relation, predicate.as_deref(), entity, cgs),
    }
}

fn compile_comparison(
    field: &str,
    op: plasm_core::CompOp,
    value: &plasm_core::TypedComparisonValue,
    entity: &EntityDef,
    cap_params: &[InputFieldSchema],
) -> Result<BackendFilter, CompileError> {
    let wire = value.to_value();
    // Entity field: compile to a BackendFilter field comparison.
    if entity.fields.contains_key(field) {
        let backend_op = BackendOp::from(op);
        return Ok(BackendFilter::field(field, backend_op, wire));
    }

    // Capability parameter: NOT compiled to BackendFilter.
    // These are HTTP-layer inputs (role: search, sort, response_control, etc.)
    // that go directly into the CML env via extract_predicate_vars.
    // Return BackendFilter::True (no-op) so the overall filter stays correct.
    if cap_params.iter().any(|p| p.name == field) {
        return Ok(BackendFilter::True);
    }

    Err(CompileError::CompilationFailed {
        message: format!("Field '{}' not found in entity '{}'", field, entity.name),
    })
}

fn compile_relation(
    relation: &str,
    predicate: Option<&Predicate>,
    entity: &EntityDef,
    cgs: &CGS,
) -> Result<BackendFilter, CompileError> {
    // Get relation schema
    let relation_schema =
        entity
            .relations
            .get(relation)
            .ok_or_else(|| CompileError::CompilationFailed {
                message: format!(
                    "Relation '{}' not found in entity '{}'",
                    relation, entity.name
                ),
            })?;

    // Get target entity
    let target_entity = cgs
        .get_entity(&relation_schema.target_resource)
        .ok_or_else(|| CompileError::CompilationFailed {
            message: format!(
                "Target entity '{}' not found",
                relation_schema.target_resource
            ),
        })?;

    // Compile nested predicate if present
    let nested_filter = if let Some(pred) = predicate {
        Some(compile_predicate_internal(pred, target_entity, &[], cgs)?)
    } else {
        None
    };

    Ok(BackendFilter::relation(relation, nested_filter))
}

/// Compile a query expression to a backend filter.
///
/// The resulting `BackendFilter` covers only entity-field predicates.
/// Capability-parameter predicates (search, sort, response_control params)
/// produce `BackendFilter::True` entries and are handled separately by
/// `extract_predicate_vars` → CML env.
pub fn compile_query(
    query: &plasm_core::QueryExpr,
    cgs: &CGS,
) -> Result<Option<BackendFilter>, CompileError> {
    let entity = cgs
        .get_entity(&query.entity)
        .ok_or_else(|| CompileError::CompilationFailed {
            message: format!("Entity '{}' not found", query.entity),
        })?;

    // Resolve capability parameters so the compiler can distinguish
    // entity-field predicates from capability-param pass-throughs.
    // Same resolution as runtime execution (`resolve_query_capability`).
    // Fixtures with no Query/Search capabilities (tests) use empty cap params.
    let cap_params: Vec<InputFieldSchema> = if cgs
        .find_capabilities(&query.entity, CapabilityKind::Query)
        .is_empty()
        && cgs
            .find_capabilities(&query.entity, CapabilityKind::Search)
            .is_empty()
    {
        Vec::new()
    } else {
        let cap =
            resolve_query_capability(query, cgs).map_err(|e| CompileError::CompilationFailed {
                message: e.to_string(),
            })?;
        cap.object_params().map(|f| f.to_vec()).unwrap_or_default()
    };

    if let Some(predicate) = &query.predicate {
        let filter = compile_predicate_with_cap_params(predicate, entity, &cap_params, cgs)?;
        Ok(Some(filter))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::{
        Cardinality, FieldSchema, FieldType, FieldValueKind, NamedValueSchema, Predicate,
        QueryExpr, RelationSchema, ResourceSchema, StringSemantics, Value, ValueDomainKey,
    };

    fn registry_field(wire_name: &str, values_key: &str, required: bool) -> FieldSchema {
        FieldSchema {
            name: wire_name.into(),
            kind: FieldValueKind::Registry(
                ValueDomainKey::new(values_key).expect("test values key"),
            ),
            description: String::new(),
            required,
            agent_presentation: None,
            mime_type_hint: None,
            attachment_media: None,
            wire_path: None,
            derive: None,
            data_class: None,
        }
    }

    fn create_test_cgs() -> CGS {
        let mut cgs = CGS::new();

        cgs.values.insert(
            "nv_pred_account_id".to_string(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
                array_items: None,
            },
        );
        cgs.values.insert(
            "nv_pred_account_name".to_string(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
                array_items: None,
            },
        );
        cgs.values.insert(
            "nv_pred_account_revenue".to_string(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::Number,
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
            },
        );
        cgs.values.insert(
            "nv_pred_account_region".to_string(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::Select,
                value_format: None,
                allowed_values: Some(vec![
                    "EMEA".to_string(),
                    "APAC".to_string(),
                    "AMER".to_string(),
                ]),
                string_semantics: None,
                array_items: None,
            },
        );
        cgs.values.insert(
            "nv_pred_contact_id".to_string(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
                array_items: None,
            },
        );
        cgs.values.insert(
            "nv_pred_contact_name".to_string(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
                array_items: None,
            },
        );
        cgs.values.insert(
            "nv_pred_contact_role".to_string(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::Select,
                value_format: None,
                allowed_values: Some(vec!["Manager".to_string(), "Employee".to_string()]),
                string_semantics: None,
                array_items: None,
            },
        );

        // Account entity
        let account = ResourceSchema {
            name: "Account".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                registry_field("id", "nv_pred_account_id", true),
                registry_field("name", "nv_pred_account_name", true),
                registry_field("revenue", "nv_pred_account_revenue", false),
                registry_field("region", "nv_pred_account_region", false),
            ],
            relations: vec![RelationSchema {
                name: "contacts".into(),
                description: String::new(),
                target_resource: "Contact".into(),
                cardinality: Cardinality::Many,
                materialize: None,
                discovery: None,
            }],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        };

        // Contact entity
        let contact = ResourceSchema {
            name: "Contact".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                registry_field("id", "nv_pred_contact_id", true),
                registry_field("name", "nv_pred_contact_name", true),
                registry_field("role", "nv_pred_contact_role", false),
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
        cgs.add_resource(contact).unwrap();

        cgs
    }

    #[test]
    fn test_compile_simple_comparison() {
        let cgs = create_test_cgs();
        let entity = cgs.get_entity("Account").unwrap();
        let predicate = Predicate::eq("name", "Acme Corp");

        let result = compile_predicate(&predicate, entity, &cgs).unwrap();

        if let BackendFilter::Field {
            field,
            operator,
            value,
        } = result
        {
            assert_eq!(field, "name");
            assert_eq!(operator, BackendOp::Equals);
            assert_eq!(value, Value::String("Acme Corp".to_string()));
        } else {
            panic!("Expected field filter");
        }
    }

    #[test]
    fn test_compile_and_predicate() {
        let cgs = create_test_cgs();
        let entity = cgs.get_entity("Account").unwrap();
        let predicate = Predicate::and(vec![
            Predicate::eq("region", "EMEA"),
            Predicate::gt("revenue", 1000.0),
        ]);

        let result = compile_predicate(&predicate, entity, &cgs).unwrap();

        if let BackendFilter::And { filters } = result {
            assert_eq!(filters.len(), 2);
        } else {
            panic!("Expected And filter");
        }
    }

    #[test]
    fn test_compile_relation_predicate() {
        let cgs = create_test_cgs();
        let entity = cgs.get_entity("Account").unwrap();
        let predicate =
            Predicate::exists_relation("contacts", Some(Predicate::eq("role", "Manager")));

        let result = compile_predicate(&predicate, entity, &cgs).unwrap();

        if let BackendFilter::Relation { relation, filter } = result {
            assert_eq!(relation, "contacts");
            assert!(filter.is_some());

            if let Some(nested) = filter {
                if let BackendFilter::Field { field, .. } = *nested {
                    assert_eq!(field, "role");
                } else {
                    panic!("Expected nested field filter");
                }
            }
        } else {
            panic!("Expected Relation filter");
        }
    }

    #[test]
    fn test_compile_query_with_predicate() {
        let cgs = create_test_cgs();
        let query = QueryExpr::filtered(
            "Account",
            Predicate::and(vec![
                Predicate::eq("region", "EMEA"),
                Predicate::gt("revenue", 1000.0),
            ]),
        );

        let result = compile_query(&query, &cgs).unwrap();
        assert!(result.is_some());

        let filter = result.unwrap();
        if let BackendFilter::And { filters } = filter {
            assert_eq!(filters.len(), 2);
        } else {
            panic!("Expected And filter");
        }
    }

    #[test]
    fn test_compile_query_without_predicate() {
        let cgs = create_test_cgs();
        let query = QueryExpr::all("Account");

        let result = compile_query(&query, &cgs).unwrap();
        assert!(result.is_none()); // No filter means get all
    }

    #[test]
    fn test_normalization_during_compilation() {
        let cgs = create_test_cgs();
        let entity = cgs.get_entity("Account").unwrap();

        // Create a predicate with nested Ands that should be flattened
        let predicate = Predicate::And {
            args: vec![
                Predicate::eq("region", "EMEA"),
                Predicate::And {
                    args: vec![
                        Predicate::gt("revenue", 1000.0),
                        Predicate::eq("name", "Test"),
                    ],
                },
            ],
        };

        let result = compile_predicate(&predicate, entity, &cgs).unwrap();

        // Should be flattened to a single And with 3 filters
        if let BackendFilter::And { filters } = result {
            assert_eq!(filters.len(), 3);
        } else {
            panic!("Expected flattened And filter");
        }
    }
}
