//! Semantic rows shared by decoders, cache storage and execution.
//!
//! Adapters expose data only. This module owns identity serialization, relation
//! cardinality and public projection; adapters must not implement their own codecs.
use crate::{Cardinality, Ref, RefWire, TypedFieldValue, CGS};
use indexmap::IndexMap;
use serde_json::{Map, Value as Json};
use std::collections::BTreeSet;

/// A computation row contains values, not transport encodings. Missing keys are unobserved.
pub use crate::ValueRow;

/// An observed relation and the evidence for its complete membership.
/// Reference order and duplicate occurrences are significant. Membership cannot
/// be mutated independently of its evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct RelationMembership {
    record: crate::collection_codec::RecordedCollection<Ref>,
}
impl RelationMembership {
    pub fn from_record(record: crate::collection_codec::RecordedCollection<Ref>) -> Self {
        Self { record }
    }
    pub fn record(&self) -> &crate::collection_codec::RecordedCollection<Ref> {
        &self.record
    }
    pub fn is_exhaustive(&self) -> bool {
        use crate::collection_codec::{CollectionCodec, Demand, RecordingCodec};
        RecordingCodec::new()
            .materialize(&self.record, Demand::Whole)
            .is_ok()
    }
    pub fn references(&self) -> &crate::collection_codec::SharedRows<Ref> {
        self.record.observed()
    }
    pub fn into_references(self) -> crate::collection_codec::SharedRows<Ref> {
        self.record.into_rows()
    }

    pub fn observe(
        cgs: Option<&CGS>,
        context: &impl serde::Serialize,
        references: Vec<Ref>,
        exhaustive_count: Option<usize>,
    ) -> Result<Self, crate::collection_codec::CollectionFault> {
        use crate::collection_codec::{
            CollectionCodec, CollectionFault, CollectionIdentity, Observation, RecordingCodec,
        };
        let identity = match cgs {
            Some(cgs) => CollectionIdentity::for_expression(cgs, context, 0)?,
            None if exhaustive_count.is_none() => {
                CollectionIdentity::for_untyped_observation(context)?
            }
            None => return Err(CollectionFault::Conservation),
        };
        if exhaustive_count.is_some_and(|count| count != references.len()) {
            return Err(CollectionFault::Conservation);
        }
        let observed = references.len();
        Ok(Self::from_record(RecordingCodec::new().record(
            identity,
            references,
            Observation::Embedded {
                declared_exhaustive: exhaustive_count.is_some(),
                observed,
            },
        )?))
    }
}
impl serde::Serialize for RelationMembership {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use crate::collection_codec::{CollectionCheckpoint, RecordingCodec};
        CollectionCheckpoint::capture(&RecordingCodec::new(), &self.record)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}
impl<'de> serde::Deserialize<'de> for RelationMembership {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use crate::collection_codec::{CollectionCheckpoint, RecordingCodec};
        let checkpoint = <CollectionCheckpoint as serde::Deserialize>::deserialize(deserializer)?;
        checkpoint
            .restore(&RecordingCodec::new())
            .map(Self::from_record)
            .map_err(serde::de::Error::custom)
    }
}
impl std::ops::Deref for RelationMembership {
    type Target = crate::collection_codec::SharedRows<Ref>;
    fn deref(&self) -> &Self::Target {
        self.record.observed()
    }
}
impl<'a> IntoIterator for &'a RelationMembership {
    type Item = &'a Ref;
    type IntoIter = crate::collection_codec::RowIter<'a, Ref>;
    fn into_iter(self) -> Self::IntoIter {
        self.record.observed().iter()
    }
}

/// Storage-independent access to an entity observation. Missing relation keys
/// mean unobserved; a present empty slice means an observed empty relation.
pub trait EntityRow {
    fn identity(&self) -> &Ref;
    fn fields(&self) -> impl Iterator<Item = (&str, TypedFieldValue)>;
    fn relations(&self) -> impl Iterator<Item = (&str, &RelationMembership)>;
    fn unavailable_fields(&self) -> impl Iterator<Item = &str>;
}

/// Owned semantic row. Cache epochs, timestamps and transport state are not data.
#[derive(Debug, Clone, PartialEq)]
pub struct RowRecord {
    identity: Ref,
    fields: IndexMap<String, TypedFieldValue>,
    relations: IndexMap<String, RelationMembership>,
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
    fn relations(&self) -> impl Iterator<Item = (&str, &RelationMembership)> {
        self.relations
            .iter()
            .map(|(key, refs)| (key.as_str(), refs))
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
                .map(|(key, refs)| (key.to_string(), refs.clone()))
                .collect(),
            unavailable_fields: row.unavailable_fields().map(str::to_string).collect(),
        }
    }

    // Decomposition preserves the public row contract components.
    #[allow(clippy::type_complexity)]
    pub fn into_parts(
        self,
    ) -> (
        Ref,
        IndexMap<String, TypedFieldValue>,
        IndexMap<String, RelationMembership>,
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

    pub fn identity_values(&self, reference: &Ref) -> ValueRow {
        let mut row = ValueRow::new();
        self.apply_identity(&mut row, reference);
        row.insert("_ref".into(), RefWire::from_ref(reference).to_value());
        row
    }
    pub fn identity_row(&self, reference: &Ref) -> Json {
        crate::plasm_value_to_json(&self.identity_values(reference).into_value())
    }
    fn apply_identity(&self, row: &mut ValueRow, reference: &Ref) {
        use crate::{EntityKey, Value};
        let needed = |value: Option<&Value>| match value {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.is_empty(),
            _ => false,
        };
        let entity = self
            .cgs
            .and_then(|c| c.get_entity(reference.entity_type.as_str()));
        match &reference.key {
            EntityKey::Simple(slot) => {
                let key = entity.map(|e| e.id_field.as_str()).unwrap_or("id");
                if let Some(id) = slot.as_lit_str().filter(|id| !id.is_empty()) {
                    if needed(row.get(key)) {
                        row.insert(key.into(), Value::String(id.into()));
                    }
                }
            }
            EntityKey::Compound(parts) => {
                for (key, slot) in parts {
                    let Some(text) = slot.as_lit_str().filter(|s| !s.is_empty()) else {
                        continue;
                    };
                    if !needed(row.get(key)) {
                        continue;
                    }
                    let raw = Value::String(text.into());
                    let value = entity
                        .and_then(|e| e.fields.get(key.as_str()))
                        .and_then(|field| self.cgs.and_then(|cgs| field.named_value(cgs).ok()))
                        .and_then(|nv| {
                            crate::coerce_value_for_field_type(
                                &nv.field_type,
                                nv.value_format,
                                nv.array_items.as_ref(),
                                raw.clone(),
                            )
                            .ok()
                        })
                        .unwrap_or(raw);
                    row.insert(key.clone(), value);
                }
            }
        }
    }
    /// Materialize native values for execution. JSON belongs to `encode` only.
    pub fn values(&self, row: &impl EntityRow) -> ValueRow {
        use crate::Value;
        let mut object: ValueRow = row
            .fields()
            .map(|(key, value)| (key.to_owned(), value.into_value()))
            .collect();
        self.apply_identity(&mut object, row.identity());
        for (name, references) in row.relations() {
            let values: Vec<_> = references
                .iter()
                .map(|r| self.identity_values(r).into_value())
                .collect();
            let single = self
                .cgs
                .and_then(|c| c.get_entity(row.identity().entity_type.as_str()))
                .and_then(|e| e.relations.get(name))
                .is_some_and(|r| r.cardinality == Cardinality::One);
            object.insert(
                name.into(),
                if single {
                    values.into_iter().next().unwrap_or(Value::Null)
                } else {
                    Value::Array(values)
                },
            );
        }
        object.insert("_ref".into(), RefWire::from_ref(row.identity()).to_value());
        let unavailable: Vec<_> = row
            .unavailable_fields()
            .map(|name| Value::String(name.into()))
            .collect();
        if !unavailable.is_empty() {
            object.insert("_unavailable_fields".into(), Value::Array(unavailable));
        }
        object
    }
    pub fn encode(&self, row: &impl EntityRow) -> Json {
        crate::plasm_value_to_json(&self.values(row).into_value())
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
        let value = crate::json_value_to_plasm_value(wire);
        self.decode_values(entity, &ValueRow::try_from(value)?)
    }
    pub fn decode_values(&self, entity: &str, object: &ValueRow) -> Result<RowRecord, String> {
        use crate::Value as V;
        let definition = self.cgs.and_then(|cgs| cgs.get_entity(entity));
        let identity = match object.get("_ref") {
            Some(reference) => RefWire::from_value(reference)
                .ok_or("row requires a structural _ref")?
                .into_ref(),
            None => {
                let definition = definition
                    .ok_or_else(|| format!("row without _ref requires schema for `{entity}`"))?;
                let scalar = |name: &str| -> Result<String, String> {
                    match object.get(name) {
                        Some(V::String(value)) => Ok(value.clone()),
                        Some(V::Integer(value)) => Ok(value.to_string()),
                        Some(V::Unsigned(value)) => Ok(value.to_string()),
                        Some(V::Float(value)) => {
                            crate::operand_binding::encode_float_identity(*value)
                        }
                        Some(V::Bool(value)) => Ok(value.to_string()),
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
                let values: Vec<&crate::Value> = match (relation.cardinality, value) {
                    (Cardinality::Many, V::Array(values)) => values.iter().collect(),
                    (Cardinality::One, V::Object(_)) => vec![value],
                    (Cardinality::One, V::Null) => vec![],
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
                        let reference = RefWire::from_value(reference)
                            .ok_or_else(|| format!("relation `{key}` has invalid _ref"))?
                            .into_ref();
                        if reference.entity_type.as_str() != relation.target_resource.as_str() {
                            return Err(format!("relation `{key}` has wrong target identity"));
                        }
                        Ok(reference)
                    })
                    .collect::<Result<_, String>>()?;
                relations.insert(
                    key.clone(),
                    RelationMembership::observe(
                        self.cgs,
                        &("value_ingress", &identity, key, value),
                        references,
                        None,
                    )
                    .map_err(|e| e.to_string())?,
                );
            } else {
                fields.insert(key.clone(), TypedFieldValue::from(value.clone()));
            }
        }
        if let Some(cgs) = self.cgs {
            crate::restore_id_field_from_compound_ref(&mut fields, &identity, definition, cgs);
        }
        let unavailable_fields = object
            .get("_unavailable_fields")
            .map(|value| {
                value
                    .as_array()
                    .ok_or_else(|| "unavailable fields must be an array".to_owned())?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| "unavailable field must be a string".to_owned())
                    })
                    .collect::<Result<BTreeSet<_>, String>>()
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
    pub fn project(&self, row: &ValueRow) -> Result<ValueRow, String> {
        let mut projected = ValueRow::new();
        for field in &self.schema.fields {
            match row.get(field.name.as_str()) {
                Some(value) => {
                    projected.insert(field.name.to_string(), value.clone());
                }
                None if self.schema.optional_fields.contains(field.name.as_str()) => {}
                None => return Err(format!("row missing declared column `{}`", field.name)),
            }
        }
        Ok(projected)
    }
    pub fn union(&self, left: &[ValueRow], right: &[ValueRow]) -> Result<Vec<ValueRow>, String> {
        self.union_with_occurrences(left, right)
            .map(|(rows, _)| rows)
    }
    /// Stable public-value union and the retained occurrence in left ++ right.
    pub fn union_with_occurrences(
        &self,
        left: &[ValueRow],
        right: &[ValueRow],
    ) -> Result<(Vec<ValueRow>, Vec<usize>), String> {
        use std::hash::Hasher;
        let mut occurrences = Vec::new();
        let mut seen = std::collections::HashMap::<u64, Vec<usize>>::new();
        let mut result = Vec::new();
        for (occurrence, row) in left.iter().chain(right).enumerate() {
            let projected = self.project(row)?;
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            crate::hash_resolved_value(&projected, &mut hash)?;
            let candidates = seen.entry(hash.finish()).or_default();
            if candidates.iter().any(|index| result[*index] == projected) {
                continue;
            }
            candidates.push(result.len());
            result.push(projected);
            occurrences.push(occurrence);
        }
        Ok((result, occurrences))
    }
}

#[cfg(test)]
mod presence_tests {
    use super::*;
    use crate::fixture_row as json;
    use crate::value_contract::ValueContract;
    use crate::{OutputName, SyntheticFieldSchema, SyntheticResultSchema};

    proptest::proptest! {
        #[test]
        fn union_survivors_are_stable_first_occurrences(
            left in proptest::collection::vec(-4i64..5, 0..25),
            right in proptest::collection::vec(-4i64..5, 0..25),
        ) {
            let kind = ValueContract::scalar(crate::FieldType::Integer);
            let schema = SyntheticResultSchema {
                entity: None, optional_fields: Default::default(),
                fields: vec![SyntheticFieldSchema { name: OutputName::new("value").unwrap(), value_kind: kind.summary(), value_type: Some(kind), source: None }],
            };
            let to_rows = |values: &[i64]| values.iter().map(|value| json!({"value":value})).collect::<Vec<_>>();
            let (rows, occurrences) = PublicRowSchema::new(&schema).union_with_occurrences(&to_rows(&left), &to_rows(&right)).unwrap();
            let input: Vec<_> = left.iter().chain(&right).copied().collect();
            let mut expected = Vec::new();
            for (index, value) in input.iter().enumerate() {
                if !input[..index].contains(value) { expected.push(index); }
            }
            proptest::prop_assert_eq!(&occurrences, &expected);
            proptest::prop_assert_eq!(rows, expected.iter().map(|index| json!({"value":input[*index]})).collect::<Vec<_>>());
        }
    }

    #[test]
    fn observed_union_distinguishes_absent_from_null_and_keeps_first() {
        let kind = ValueContract::scalar(crate::FieldType::String);
        let mut schema = SyntheticResultSchema {
            entity: None,
            optional_fields: BTreeSet::from(["value".into()]),
            fields: vec![SyntheticFieldSchema {
                name: OutputName::new("value").unwrap(),
                value_kind: kind.summary(),
                value_type: Some(kind),
                source: None,
            }],
        };
        let left = [
            json!({"_ref":"a"}),
            json!({"value":null}),
            json!({"value":"x"}),
        ];
        let right = [json!({"_ref":"b"}), json!({"value":null})];
        assert_eq!(
            PublicRowSchema::new(&schema).union(&left, &right).unwrap(),
            vec![json!({}), json!({"value":null}), json!({"value":"x"})]
        );
        let (_, occurrences) = PublicRowSchema::new(&schema)
            .union_with_occurrences(&left, &right)
            .unwrap();
        assert_eq!(occurrences, [0, 1, 2]);
        let (_, occurrences) = PublicRowSchema::new(&schema)
            .union_with_occurrences(&right, &left)
            .unwrap();
        assert_eq!(occurrences, [0, 1, 4]);
        schema.optional_fields.clear();
        assert!(PublicRowSchema::new(&schema).project(&json!({})).is_err());
    }
}

#[cfg(test)]
mod relation_ownership_tests {
    use super::*;

    #[test]
    fn cloning_membership_shares_ordered_occurrences() {
        let membership = {
            use crate::collection_codec::{
                CollectionCodec, CollectionIdentity, Observation, RecordingCodec,
            };
            let references = vec![Ref::new("Child", "x"), Ref::new("Child", "x")];
            let observation = Observation::ExactOutput {
                decoded: references.len(),
            };
            crate::row_contract::RelationMembership::from_record(
                RecordingCodec::new()
                    .record(
                        CollectionIdentity::for_untyped_observation(&"relation_fixture").unwrap(),
                        references,
                        observation,
                    )
                    .unwrap(),
            )
        };
        let copy = membership.clone();
        assert!(std::ptr::eq(&membership[0], &copy[0]));
        assert_eq!(copy.len(), 2);
        drop(membership);
        assert!(copy.is_exhaustive());
        assert_eq!(copy[0], copy[1]);
    }

    #[test]
    fn relation_roundtrip_preserves_evidence_and_duplicate_occurrences() {
        for exhaustive in [true, false] {
            let refs = vec![Ref::new("Child", "x"), Ref::new("Child", "x")];
            let membership = if exhaustive {
                {
                    use crate::collection_codec::{
                        CollectionCodec, CollectionIdentity, Observation, RecordingCodec,
                    };
                    let references = refs;
                    let observation = Observation::ExactOutput {
                        decoded: references.len(),
                    };
                    crate::row_contract::RelationMembership::from_record(
                        RecordingCodec::new()
                            .record(
                                CollectionIdentity::for_untyped_observation(&"relation_fixture")
                                    .unwrap(),
                                references,
                                observation,
                            )
                            .unwrap(),
                    )
                }
            } else {
                {
                    use crate::collection_codec::{
                        CollectionCodec, CollectionIdentity, Observation, RecordingCodec,
                    };
                    let references = refs;
                    let observation = Observation::UnprovenPage;
                    crate::row_contract::RelationMembership::from_record(
                        RecordingCodec::new()
                            .record(
                                CollectionIdentity::for_untyped_observation(&"relation_fixture")
                                    .unwrap(),
                                references,
                                observation,
                            )
                            .unwrap(),
                    )
                }
            };
            let bytes = serde_json::to_vec(&membership).unwrap();
            let restored: RelationMembership = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(membership, restored);
            assert_eq!(restored.len(), 2);
            assert_eq!(restored.is_exhaustive(), exhaustive);
        }
    }
}
