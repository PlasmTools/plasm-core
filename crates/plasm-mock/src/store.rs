use crate::MockError;
use indexmap::IndexMap;
use plasm_compile::BackendFilter;
use plasm_core::{EntityDef, Ref, Value, CGS};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// In-memory data store for the mock backend
#[derive(Debug, Clone)]
pub struct MockStore {
    /// Schema definition
    pub schema: CGS,
    /// Entity data: entity_type -> id -> fields
    pub data: HashMap<String, HashMap<String, MockResource>>,
}

/// A resource instance in the mock store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockResource {
    pub id: String,
    pub fields: IndexMap<String, Value>,
    pub relations: IndexMap<String, Vec<String>>, // relation_name -> [related_ids]
}

impl MockStore {
    /// Create a new mock store with the given schema
    pub fn new(schema: CGS) -> Self {
        Self {
            schema,
            data: HashMap::new(),
        }
    }

    /// Add a resource to the store
    pub fn add_resource(
        &mut self,
        entity_type: &str,
        resource: MockResource,
    ) -> Result<(), MockError> {
        // Validate entity exists
        self.schema
            .get_entity(entity_type)
            .ok_or_else(|| MockError::EntityNotFound {
                entity: entity_type.to_string(),
            })?;

        let entity_data = self.data.entry(entity_type.to_string()).or_default();
        entity_data.insert(resource.id.clone(), resource);
        Ok(())
    }

    /// Get a resource by ID
    pub fn get_resource(&self, entity_type: &str, id: &str) -> Result<&MockResource, MockError> {
        let entity_data = self
            .data
            .get(entity_type)
            .ok_or_else(|| MockError::EntityNotFound {
                entity: entity_type.to_string(),
            })?;

        entity_data
            .get(id)
            .ok_or_else(|| MockError::ResourceNotFound {
                entity: entity_type.to_string(),
                id: id.to_string(),
            })
    }

    /// Query resources with optional filtering
    pub fn query_resources(
        &self,
        entity_type: &str,
        filter: Option<&BackendFilter>,
    ) -> Result<Vec<MockResource>, MockError> {
        let entity =
            self.schema
                .get_entity(entity_type)
                .ok_or_else(|| MockError::EntityNotFound {
                    entity: entity_type.to_string(),
                })?;

        let empty_data = HashMap::new();
        let entity_data = self.data.get(entity_type).unwrap_or(&empty_data);
        let mut results: Vec<MockResource> = entity_data.values().cloned().collect();

        // Apply filter if present
        if let Some(filter) = filter {
            results.retain(|resource| {
                self.matches_filter(resource, filter, entity)
                    .unwrap_or(false)
            });
        }

        Ok(results)
    }

    /// Get related resources
    pub fn get_related_resources(
        &self,
        entity_type: &str,
        id: &str,
        relation: &str,
    ) -> Result<Vec<MockResource>, MockError> {
        let entity =
            self.schema
                .get_entity(entity_type)
                .ok_or_else(|| MockError::EntityNotFound {
                    entity: entity_type.to_string(),
                })?;

        let relation_schema =
            entity
                .relations
                .get(relation)
                .ok_or_else(|| MockError::InvalidRequest {
                    message: format!(
                        "Relation '{}' not found in entity '{}'",
                        relation, entity_type
                    ),
                })?;

        let resource = self.get_resource(entity_type, id)?;
        let empty_vec = Vec::new();
        let related_ids = resource.relations.get(relation).unwrap_or(&empty_vec);

        let mut related_resources = Vec::new();
        for related_id in related_ids {
            if let Ok(related) = self.get_resource(&relation_schema.target_resource, related_id) {
                related_resources.push(related.clone());
            }
        }

        Ok(related_resources)
    }

    /// Check if a resource matches a backend filter
    fn matches_filter(
        &self,
        resource: &MockResource,
        filter: &BackendFilter,
        entity: &EntityDef,
    ) -> Result<bool, MockError> {
        match filter {
            BackendFilter::True => Ok(true),
            BackendFilter::False => Ok(false),

            BackendFilter::Field {
                field,
                operator,
                value,
            } => self.matches_field_filter(resource, field, operator, value, entity),

            BackendFilter::And { filters } => {
                for f in filters {
                    if !self.matches_filter(resource, f, entity)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }

            BackendFilter::Or { filters } => {
                for f in filters {
                    if self.matches_filter(resource, f, entity)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }

            BackendFilter::Not { filter } => Ok(!self.matches_filter(resource, filter, entity)?),

            BackendFilter::Relation { relation, filter } => {
                self.matches_relation_filter(resource, relation, filter.as_deref(), entity)
            }
        }
    }

    /// Check if a resource matches a field filter
    fn matches_field_filter(
        &self,
        resource: &MockResource,
        field: &str,
        operator: &plasm_compile::BackendOp,
        filter_value: &Value,
        _entity: &EntityDef,
    ) -> Result<bool, MockError> {
        let resource_value = resource.fields.get(field).unwrap_or(&Value::Null);

        use plasm_compile::BackendOp;

        let result = match operator {
            BackendOp::Equals => resource_value == filter_value,
            BackendOp::NotEquals => resource_value != filter_value,
            BackendOp::Exists => !matches!(resource_value, Value::Null),

            BackendOp::GreaterThan => {
                if let (Some(rv), Some(fv)) = (resource_value.as_number(), filter_value.as_number())
                {
                    rv > fv
                } else {
                    false
                }
            }

            BackendOp::LessThan => {
                if let (Some(rv), Some(fv)) = (resource_value.as_number(), filter_value.as_number())
                {
                    rv < fv
                } else {
                    false
                }
            }

            BackendOp::GreaterThanOrEqual => {
                if let (Some(rv), Some(fv)) = (resource_value.as_number(), filter_value.as_number())
                {
                    rv >= fv
                } else {
                    false
                }
            }

            BackendOp::LessThanOrEqual => {
                if let (Some(rv), Some(fv)) = (resource_value.as_number(), filter_value.as_number())
                {
                    rv <= fv
                } else {
                    false
                }
            }

            BackendOp::Contains => resource_value.contains(filter_value),

            BackendOp::In => {
                if let Some(array) = filter_value.as_array() {
                    array.contains(resource_value)
                } else {
                    false
                }
            }

            BackendOp::StartsWith => {
                if let (Some(rv), Some(fv)) = (resource_value.as_str(), filter_value.as_str()) {
                    rv.starts_with(fv)
                } else {
                    false
                }
            }

            BackendOp::EndsWith => {
                if let (Some(rv), Some(fv)) = (resource_value.as_str(), filter_value.as_str()) {
                    rv.ends_with(fv)
                } else {
                    false
                }
            }
        };

        Ok(result)
    }

    /// Check if a resource matches a relation filter
    fn matches_relation_filter(
        &self,
        resource: &MockResource,
        relation: &str,
        filter: Option<&BackendFilter>,
        entity: &EntityDef,
    ) -> Result<bool, MockError> {
        let relation_schema =
            entity
                .relations
                .get(relation)
                .ok_or_else(|| MockError::InvalidRequest {
                    message: format!("Relation '{}' not found", relation),
                })?;

        let empty_vec = Vec::new();
        let related_ids = resource.relations.get(relation).unwrap_or(&empty_vec);

        if related_ids.is_empty() {
            return Ok(false);
        }

        // If no nested filter, just check if relation has any items
        let Some(nested_filter) = filter else {
            return Ok(!related_ids.is_empty());
        };

        let target_entity = self
            .schema
            .get_entity(relation_schema.target_resource.as_str())
            .ok_or_else(|| MockError::EntityNotFound {
                entity: relation_schema.target_resource.to_string(),
            })?;

        // Check if any related resource matches the nested filter
        for related_id in related_ids {
            if let Ok(related_resource) =
                self.get_resource(relation_schema.target_resource.as_str(), related_id)
            {
                if self.matches_filter(related_resource, nested_filter, target_entity)? {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// Get all resources of a given type
    pub fn get_all_resources(&self, entity_type: &str) -> Result<Vec<MockResource>, MockError> {
        self.query_resources(entity_type, None)
    }

    /// Load test data from JSON
    pub fn load_test_data(&mut self, data: serde_json::Value) -> Result<(), MockError> {
        if let Some(entities) = data.as_object() {
            for (entity_type, resources) in entities {
                if let Some(resource_array) = resources.as_array() {
                    for resource_data in resource_array {
                        let resource: MockResource = serde_json::from_value(resource_data.clone())?;
                        self.add_resource(entity_type, resource)?;
                    }
                }
            }
        }
        Ok(())
    }
}

impl MockResource {
    /// Create a new mock resource
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            fields: IndexMap::new(),
            relations: IndexMap::new(),
        }
    }

    /// Add a field
    pub fn with_field(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.fields.insert(name.into(), value.into());
        self
    }

    /// Add a relation
    pub fn with_relation(mut self, name: impl Into<String>, ids: Vec<String>) -> Self {
        self.relations.insert(name.into(), ids);
        self
    }

    /// Get the reference for this resource
    pub fn reference(&self, entity_type: &str) -> Ref {
        Ref::new(entity_type, &self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_compile::BackendOp;
    use plasm_core::{
        Cardinality, FieldSchema, FieldType, FieldValueKind, NamedValueSchema, RelationSchema,
        ResourceSchema, StringSemantics, ValueDomainKey,
    };

    fn create_test_schema() -> CGS {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "mock_nv_str_id".into(),
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
            "mock_nv_str_name".into(),
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
            "mock_nv_number".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::Number,
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
            },
        );

        let account = ResourceSchema {
            name: "Account".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                FieldSchema {
                    name: "id".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("mock_nv_str_id").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                    data_class: None,
                },
                FieldSchema {
                    name: "name".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("mock_nv_str_name").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                    data_class: None,
                },
                FieldSchema {
                    name: "revenue".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("mock_nv_number").expect("key"),
                    ),
                    description: String::new(),
                    required: false,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                    data_class: None,
                },
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

        let contact = ResourceSchema {
            name: "Contact".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                FieldSchema {
                    name: "id".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("mock_nv_str_id").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                    data_class: None,
                },
                FieldSchema {
                    name: "name".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("mock_nv_str_name").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                    data_class: None,
                },
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
    fn test_add_and_get_resource() {
        let schema = create_test_schema();
        let mut store = MockStore::new(schema);

        let account = MockResource::new("acc-1")
            .with_field("name", "Acme Corp")
            .with_field("revenue", 1200.0);

        store.add_resource("Account", account).unwrap();

        let retrieved = store.get_resource("Account", "acc-1").unwrap();
        assert_eq!(retrieved.id, "acc-1");
        assert_eq!(
            retrieved.fields.get("name"),
            Some(&Value::String("Acme Corp".to_string()))
        );
    }

    #[test]
    fn test_query_with_filter() {
        let schema = create_test_schema();
        let mut store = MockStore::new(schema);

        let acc1 = MockResource::new("acc-1")
            .with_field("name", "Acme Corp")
            .with_field("revenue", 1200.0);

        let acc2 = MockResource::new("acc-2")
            .with_field("name", "Beta Inc")
            .with_field("revenue", 800.0);

        store.add_resource("Account", acc1).unwrap();
        store.add_resource("Account", acc2).unwrap();

        let filter = BackendFilter::field("revenue", BackendOp::GreaterThan, 1000.0);
        let results = store.query_resources("Account", Some(&filter)).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "acc-1");
    }

    #[test]
    fn test_relation_filter() {
        let schema = create_test_schema();
        let mut store = MockStore::new(schema);

        let contact = MockResource::new("c-1").with_field("name", "John Doe");

        let account = MockResource::new("acc-1")
            .with_field("name", "Acme Corp")
            .with_relation("contacts", vec!["c-1".to_string()]);

        store.add_resource("Contact", contact).unwrap();
        store.add_resource("Account", account).unwrap();

        let filter = BackendFilter::relation("contacts", None);
        let results = store.query_resources("Account", Some(&filter)).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "acc-1");
    }
}
