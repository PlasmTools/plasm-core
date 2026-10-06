//! Borrowed discovery evidence. This is not a second catalog or a model wire format.
//! Consumers must project transport details out before serializing model requests.
use crate::schema::{
    CapabilityInputs, CapabilitySchema, FieldSchema, NamedValueSchema, RelationSchema,
    ValueDomainSlot,
};
use crate::CGS;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StructuredDiscoveryError {
    #[error("structured discovery requires a catalog identity")]
    MissingCatalogIdentity,
    #[error("unknown discovery capability `{capability}`")]
    CapabilityNotFound { capability: String },
    #[error("unknown discovery entity `{entity}`")]
    EntityNotFound { entity: String },
    #[error("unknown provided field `{field}`")]
    ProvidedFieldNotFound { field: String },
    #[error("unknown value domain `{key}`")]
    ValueDomainNotFound { key: String },
    #[error("semantic value domain items contain a cycle at `{key}`")]
    CyclicValueDomainItems { key: String },
    #[error(transparent)]
    Schema(#[from] crate::SchemaError),
}

/// A value reference is scoped to an exact catalog revision. Equal references
/// identify a declaration, not equal runtime values or cross-service people.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct DomainReference<'a> {
    pub catalog: &'a str,
    pub revision: &'a str,
    pub key: &'a str,
}

pub struct Observation<'a> {
    pub field: &'a FieldSchema,
    pub value: &'a NamedValueSchema,
    pub domain: DomainReference<'a>,
}

/// A capability view borrows all schema objects. Only its revision digest is owned.
/// Declared inputs remain intact, including inputs acquired by prerequisites;
/// acquisition is separate evidence, never an implicit removal of a requirement.
pub struct StructuredCapability<'a> {
    cgs: &'a CGS,
    capability: &'a CapabilitySchema,
    revision: String,
}

impl<'a> StructuredCapability<'a> {
    pub fn new(cgs: &'a CGS, capability: &str) -> Result<Self, StructuredDiscoveryError> {
        if cgs.entry_id.as_deref().is_none_or(str::is_empty) {
            return Err(StructuredDiscoveryError::MissingCatalogIdentity);
        }
        let capability = cgs.capabilities.get(capability).ok_or_else(|| {
            StructuredDiscoveryError::CapabilityNotFound {
                capability: capability.to_owned(),
            }
        })?;
        let entity = cgs.entities.get(&capability.domain).ok_or_else(|| {
            StructuredDiscoveryError::EntityNotFound {
                entity: capability.domain.to_string(),
            }
        })?;
        for name in &capability.provides {
            let field = entity.fields.get(name.as_str()).ok_or_else(|| {
                StructuredDiscoveryError::ProvidedFieldNotFound {
                    field: name.to_string(),
                }
            })?;
            field.named_value(cgs)?;
        }
        Ok(Self {
            cgs,
            capability,
            revision: cgs.catalog_cgs_hash_hex(),
        })
    }

    pub fn inputs(&self) -> &CapabilityInputs {
        &self.capability.inputs
    }

    pub fn capability(&self) -> &CapabilitySchema {
        self.capability
    }

    /// Resolve any nested input or array-element value slot against this catalog.
    pub fn value<'s>(
        &'s self,
        slot: &'s impl ValueDomainSlot,
    ) -> Result<(DomainReference<'s>, &'s NamedValueSchema), StructuredDiscoveryError> {
        self.value_by_key(slot.value_domain_key())
    }

    pub fn value_by_key<'s>(
        &'s self,
        key: &'s crate::ValueDomainKey,
    ) -> Result<(DomainReference<'s>, &'s NamedValueSchema), StructuredDiscoveryError> {
        let value = self.cgs.values.get(key.as_str()).ok_or_else(|| {
            StructuredDiscoveryError::ValueDomainNotFound {
                key: key.to_string(),
            }
        })?;
        Ok((
            DomainReference {
                catalog: self
                    .cgs
                    .entry_id
                    .as_deref()
                    .expect("validated catalog identity"),
                revision: &self.revision,
                key: key.as_str(),
            },
            value,
        ))
    }

    /// Exactly `provides`, including fields without prose and optional fields.
    pub fn observations(&self) -> impl Iterator<Item = Observation<'_>> {
        let entity = &self.cgs.entities[&self.capability.domain];
        self.capability.provides.iter().map(move |name| {
            let field = &entity.fields[name.as_str()];
            let (domain, value) = self.value(field).expect("validated observation");
            Observation {
                field,
                value,
                domain,
            }
        })
    }

    /// All declared relationships are available with their source and original
    /// materialization/bindings. Presence is not evidence that this read supplies them.
    pub fn relationships(&self) -> impl Iterator<Item = (&str, &RelationSchema)> {
        self.cgs.entities.values().flat_map(|entity| {
            entity
                .relations
                .values()
                .map(move |relation| (entity.name.as_str(), relation))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> CGS {
        let mut cgs = crate::load_schema(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/prerequisite_matrix"),
        )
        .unwrap();
        cgs.entry_id = Some("abstract-a".into());
        cgs
    }

    #[test]
    fn structured_discovery_preserves_borrowed_fields_and_acquired_inputs() {
        let cgs = fixture();
        let view = StructuredCapability::new(&cgs, "read").unwrap();
        assert!(std::ptr::eq(
            view.inputs(),
            &cgs.capabilities["read"].inputs
        ));
        assert!(view
            .inputs()
            .selection
            .0
            .iter()
            .any(|f| f.name == "credential"));
        let actual: Vec<_> = view.observations().collect();
        assert_eq!(actual.len(), cgs.capabilities["read"].provides.len());
        for observation in actual {
            let field = &cgs.entities["BusinessRecord"].fields[observation.field.name.as_str()];
            assert!(std::ptr::eq(observation.field, field));
            assert!(std::ptr::eq(
                observation.value,
                field.named_value(&cgs).unwrap()
            ));
        }
    }

    #[test]
    fn structured_discovery_domain_identity_includes_catalog_and_revision() {
        let a = fixture();
        let mut b = fixture();
        b.entry_id = Some("abstract-b".into());
        let va = StructuredCapability::new(&a, "read").unwrap();
        let vb = StructuredCapability::new(&b, "read").unwrap();
        assert_ne!(
            va.observations().next().unwrap().domain,
            vb.observations().next().unwrap().domain
        );
        b.entry_id = a.entry_id.clone();
        b.entities
            .get_mut("BusinessRecord")
            .unwrap()
            .description
            .push_str(" revised");
        let vb = StructuredCapability::new(&b, "read").unwrap();
        assert_ne!(
            va.observations().next().unwrap().domain,
            vb.observations().next().unwrap().domain
        );
    }

    #[test]
    fn structured_discovery_keeps_optional_undescribed_outputs() {
        let mut cgs = fixture();
        let field = cgs
            .entities
            .get_mut("BusinessRecord")
            .unwrap()
            .fields
            .get_mut("id")
            .unwrap();
        field.required = false;
        field.description.clear();
        let view = StructuredCapability::new(&cgs, "read").unwrap();
        let observation = view.observations().next().unwrap();
        assert_eq!(observation.field.name.as_str(), "id");
        assert!(!observation.field.required);
        assert!(observation.field.description.is_empty());
    }

    #[test]
    fn structured_discovery_borrows_relationships_without_inferred_edges() {
        let mut cgs = fixture();
        let relation = crate::schema::RelationSchema {
            name: "related".into(),
            description: "Explicit abstract relation".into(),
            target_resource: "ProviderResult".into(),
            cardinality: crate::schema::Cardinality::Many,
            materialize: None,
            discovery: None,
        };
        cgs.entities
            .get_mut("BusinessRecord")
            .unwrap()
            .relations
            .insert("related".into(), relation);
        let view = StructuredCapability::new(&cgs, "read").unwrap();
        let relations: Vec<_> = view.relationships().collect();
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].0, "BusinessRecord");
        assert!(std::ptr::eq(
            relations[0].1,
            &cgs.entities["BusinessRecord"].relations["related"]
        ));
    }

    #[test]
    fn structured_discovery_rejects_missing_observation() {
        let mut cgs = fixture();
        cgs.capabilities
            .get_mut("read")
            .unwrap()
            .provides
            .push("missing".into());
        assert!(matches!(
            StructuredCapability::new(&cgs, "read"),
            Err(StructuredDiscoveryError::ProvidedFieldNotFound { field }) if field == "missing"
        ));
    }

    #[test]
    fn structured_discovery_rejects_missing_catalog_identity_semantically() {
        let mut cgs = fixture();
        cgs.entry_id = None;
        assert!(matches!(
            StructuredCapability::new(&cgs, "read"),
            Err(StructuredDiscoveryError::MissingCatalogIdentity)
        ));
    }
}
