//! Semantic rows shared by decoders, cache storage and execution.
//!
//! Adapters expose data only. This module owns identity serialization, relation
//! cardinality and public projection; adapters must not implement their own codecs.
use crate::{Cardinality, Ref, RefWire, TypedFieldValue, CGS};
use indexmap::IndexMap;
use serde_json::{Map, Value as Json};
use std::collections::BTreeSet;

/// Storage-independent access to an entity observation. Missing relation keys
/// mean unobserved; a present empty slice means an observed empty relation.
pub trait EntityRow {
    fn identity(&self) -> &Ref;
    fn fields(&self) -> impl Iterator<Item = (&str, TypedFieldValue)>;
    fn relations(&self) -> impl Iterator<Item = (&str, &[Ref])>;
    fn unavailable_fields(&self) -> impl Iterator<Item = &str>;
}

/// Owned semantic row. Cache epochs, timestamps and transport state are not data.
#[derive(Debug, Clone, PartialEq)]
pub struct RowRecord {
    identity: Ref,
    fields: IndexMap<String, TypedFieldValue>,
    relations: IndexMap<String, Vec<Ref>>,
    unavailable_fields: BTreeSet<String>,
}

impl EntityRow for RowRecord {
    fn identity(&self) -> &Ref {
        &self.identity
    }
    fn fields(&self) -> impl Iterator<Item = (&str, TypedFieldValue)> {
        self.fields
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
    }
    fn relations(&self) -> impl Iterator<Item = (&str, &[Ref])> {
        self.relations
            .iter()
            .map(|(key, refs)| (key.as_str(), refs.as_slice()))
    }
    fn unavailable_fields(&self) -> impl Iterator<Item = &str> {
        self.unavailable_fields.iter().map(String::as_str)
    }
}

impl RowRecord {
    pub fn capture(row: &impl EntityRow) -> Self {
        Self {
            identity: row.identity().clone(),
            fields: row
                .fields()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
            relations: row
                .relations()
                .map(|(key, refs)| (key.to_string(), refs.to_vec()))
                .collect(),
            unavailable_fields: row.unavailable_fields().map(str::to_string).collect(),
        }
    }

    pub fn into_parts(
        self,
    ) -> (
        Ref,
        IndexMap<String, TypedFieldValue>,
        IndexMap<String, Vec<Ref>>,
        BTreeSet<String>,
    ) {
        (
            self.identity,
            self.fields,
            self.relations,
            self.unavailable_fields,
        )
    }
}

/// The schema is pinned by the caller's catalog context. The same codec is used
/// for decoded observations, cache rows and persisted rows.
pub struct RowCodec<'a> {
    cgs: Option<&'a CGS>,
}

impl<'a> RowCodec<'a> {
    pub fn new(cgs: Option<&'a CGS>) -> Self {
        Self { cgs }
    }

    pub fn identity_row(&self, reference: &Ref) -> Json {
        let mut row = Map::new();
        crate::apply_identity_slots_to_row(&mut row, reference, self.cgs);
        row.insert(
            "_ref".into(),
            serde_json::to_value(RefWire::from_ref(reference)).expect("RefWire serializes"),
        );
        Json::Object(row)
    }

    /// Machine-readable row. Relation values always contain structural identity,
    /// including when the target payload is unavailable or has been evicted.
    pub fn encode(&self, row: &impl EntityRow) -> Json {
        let mut object: Map<String, Json> = row
            .fields()
            .map(|(key, value)| {
                (
                    key.to_string(),
                    serde_json::to_value(value).expect("field serializes"),
                )
            })
            .collect();
        crate::apply_identity_slots_to_row(&mut object, row.identity(), self.cgs);
        for (name, references) in row.relations() {
            let values: Vec<_> = references
                .iter()
                .map(|reference| self.identity_row(reference))
                .collect();
            let single = self
                .cgs
                .and_then(|cgs| cgs.get_entity(row.identity().entity_type.as_str()))
                .and_then(|entity| entity.relations.get(name))
                .is_some_and(|relation| relation.cardinality == Cardinality::One);
            object.insert(
                name.into(),
                if single {
                    values.into_iter().next().unwrap_or(Json::Null)
                } else {
                    Json::Array(values)
                },
            );
        }
        object.insert(
            "_ref".into(),
            serde_json::to_value(RefWire::from_ref(row.identity())).expect("RefWire serializes"),
        );
        let unavailable: Vec<_> = row
            .unavailable_fields()
            .map(|field| Json::String(field.into()))
            .collect();
        if !unavailable.is_empty() {
            object.insert("_unavailable_fields".into(), Json::Array(unavailable));
        }
        Json::Object(object)
    }

    /// Presentation payload without synthetic identity or storage metadata.
    pub fn payload(&self, row: &impl EntityRow) -> Json {
        let mut object: Map<String, Json> = row
            .fields()
            .map(|(key, value)| {
                (
                    key.to_string(),
                    serde_json::to_value(value).expect("field serializes"),
                )
            })
            .collect();
        for (name, references) in row.relations() {
            object.insert(
                name.into(),
                Json::Array(
                    references
                        .iter()
                        .map(|reference| Json::String(reference.to_string()))
                        .collect(),
                ),
            );
        }
        Json::Object(object)
    }

    /// Presentation is a terminal projection, never an execution or storage input.
    pub fn display(&self, row: &impl EntityRow) -> Json {
        let mut result = self.encode(row);
        let object = result.as_object_mut().expect("row object");
        object.remove("_ref");
        for (name, references) in row.relations() {
            object.insert(
                name.into(),
                Json::Array(
                    references
                        .iter()
                        .map(|reference| Json::String(reference.to_string()))
                        .collect(),
                ),
            );
        }
        result
    }

    /// Decode an execution row. Storage metadata must be removed by its owning
    /// adapter before calling this method; it is never inferred from a prefix.
    pub fn decode(&self, entity: &str, wire: &Json) -> Result<RowRecord, String> {
        let object = wire.as_object().ok_or("row must be an object")?;
        let definition = self.cgs.and_then(|cgs| cgs.get_entity(entity));
        let identity = match object.get("_ref") {
            Some(reference) => serde_json::from_value::<RefWire>(reference.clone())
                .map_err(|_| "row requires a structural _ref".to_string())?
                .into_ref(),
            None => {
                let definition = definition
                    .ok_or_else(|| format!("row without _ref requires schema for `{entity}`"))?;
                let scalar = |name: &str| -> Result<String, String> {
                    match object.get(name) {
                        Some(Json::String(value)) => Ok(value.clone()),
                        Some(Json::Number(value)) => Ok(value.to_string()),
                        Some(Json::Bool(value)) => Ok(value.to_string()),
                        _ => Err(format!("row missing scalar identity field `{name}`")),
                    }
                };
                if definition.key_vars.len() > 1 {
                    Ref::compound(
                        entity,
                        definition
                            .key_vars
                            .iter()
                            .map(|key| Ok((key.to_string(), scalar(key.as_str())?)))
                            .collect::<Result<_, String>>()?,
                    )
                } else {
                    Ref::new(entity, scalar(definition.id_field.as_str())?)
                }
            }
        };
        if identity.entity_type.as_str() != entity {
            return Err(format!(
                "row identity belongs to {}, expected {entity}",
                identity.entity_type
            ));
        }
        let mut fields = IndexMap::new();
        let mut relations = IndexMap::new();
        for (key, value) in object {
            if key == "_ref" || key == "_unavailable_fields" {
                continue;
            }
            if let Some(relation) =
                definition.and_then(|definition| definition.relations.get(key.as_str()))
            {
                let values: Vec<&Json> = match (relation.cardinality, value) {
                    (Cardinality::Many, Json::Array(values)) => values.iter().collect(),
                    (Cardinality::One, Json::Object(_)) => vec![value],
                    (Cardinality::One, Json::Null) => vec![],
                    _ => {
                        return Err(format!(
                            "relation `{key}` has invalid cardinality or identity representation"
                        ))
                    }
                };
                let references = values
                    .into_iter()
                    .map(|value| {
                        let reference = value.get("_ref").ok_or_else(|| {
                            format!("relation `{key}` row requires a structural _ref")
                        })?;
                        let reference = serde_json::from_value::<RefWire>(reference.clone())
                            .map_err(|_| format!("relation `{key}` has invalid _ref"))?
                            .into_ref();
                        if reference.entity_type.as_str() != relation.target_resource.as_str() {
                            return Err(format!("relation `{key}` has wrong target identity"));
                        }
                        Ok(reference)
                    })
                    .collect::<Result<_, String>>()?;
                relations.insert(key.clone(), references);
            } else {
                fields.insert(
                    key.clone(),
                    serde_json::from_value(value.clone())
                        .map_err(|error| format!("row field `{key}`: {error}"))?,
                );
            }
        }
        if let Some(cgs) = self.cgs {
            crate::restore_id_field_from_compound_ref(&mut fields, &identity, definition, cgs);
        }
        let unavailable_fields = object
            .get("_unavailable_fields")
            .map(|value| {
                serde_json::from_value(value.clone())
                    .map_err(|error| format!("unavailable fields: {error}"))
            })
            .transpose()?
            .unwrap_or_default();
        Ok(RowRecord {
            identity,
            fields,
            relations,
            unavailable_fields,
        })
    }
}

/// A schema-bound rowset projection. Only declared columns contribute to set
/// equality. The projection is shared by every storage adapter and execution host.
pub struct PublicRowSchema<'a> {
    schema: &'a crate::plasm_monad::SyntheticResultSchema,
}

impl<'a> PublicRowSchema<'a> {
    pub fn new(schema: &'a crate::plasm_monad::SyntheticResultSchema) -> Self {
        Self { schema }
    }
    pub fn project(&self, row: &Json) -> Result<Json, String> {
        let object = row.as_object().ok_or("rowset row must be an object")?;
        Ok(Json::Object(
            self.schema
                .fields
                .iter()
                .map(|field| {
                    (
                        field.name.to_string(),
                        object
                            .get(field.name.as_str())
                            .cloned()
                            .unwrap_or(Json::Null),
                    )
                })
                .collect(),
        ))
    }
    pub fn union(&self, left: &[Json], right: &[Json]) -> Result<Vec<Json>, String> {
        let project = |rows: &[Json]| {
            rows.iter()
                .map(|row| self.project(row))
                .collect::<Result<Vec<_>, _>>()
        };
        crate::union_rowsets(&project(left)?, &project(right)?)
    }
}
