//! Catalogue-owned declarations and a separate, deduplicated reference index.
//! Never inspect transport mappings or invent a return type for an opaque action.
use super::DiscoveryMatcherError;
use crate::discovery_store::RetrievalReceipt;
use plasm_core::{
    prerequisites::CapabilityRef,
    schema::{
        CapabilityInputs, EntityDef, InputFieldSchema, InputFieldWire, InputType, NamedValueSchema,
        OutputSchema, OutputType,
    },
    FieldType, CGS,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize)]
pub struct Declaration {
    #[serde(flatten)]
    inputs: CapabilityInputs,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<OutputSchema>,
    provides: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Definitions {
    values: BTreeMap<String, NamedValueSchema>,
    records: BTreeMap<String, EntityDef>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct References {
    values: BTreeSet<String>,
    records: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct CapabilityEvidence {
    pub declaration: Declaration,
    pub references: References,
}

#[derive(Debug, Clone, Default)]
pub struct CatalogEvidence {
    pub definitions: Definitions,
    pub capabilities: BTreeMap<CapabilityRef, CapabilityEvidence>,
}

// Escape separator and escape character so catalogue/name pairs cannot collide.
fn key(catalog: &str, name: &str) -> String {
    fn escape(s: &str) -> String {
        s.replace('%', "%25").replace('/', "%2F")
    }
    format!("{}/{}", escape(catalog), escape(name))
}

type Result<T> = std::result::Result<T, DiscoveryMatcherError>;

/// The visited reference sets also bound recursive named-array/entity relationships.
struct DefinitionCollector<'a> {
    catalogs: &'a BTreeMap<String, CGS>,
    definitions: &'a mut Definitions,
    references: References,
}
impl DefinitionCollector<'_> {
    fn catalog(&self, catalog: &str, name: &str) -> Result<&CGS> {
        self.catalogs
            .get(catalog)
            .ok_or_else(|| missing(catalog, name))
    }

    fn record(&mut self, catalog: &str, name: &str) -> Result<()> {
        let id = key(catalog, name);
        if !self.references.records.insert(id.clone()) {
            return Ok(());
        }
        let record = self
            .catalog(catalog, name)?
            .entities
            .get(name)
            .ok_or_else(|| missing(catalog, name))?
            .clone();
        for field in record.fields.values() {
            self.value(catalog, field.kind.registry_key().as_str())?;
        }
        self.definitions.records.entry(id).or_insert(record);
        Ok(())
    }

    fn value(&mut self, catalog: &str, name: &str) -> Result<()> {
        let id = key(catalog, name);
        if !self.references.values.insert(id.clone()) {
            return Ok(());
        }
        let value = self
            .catalog(catalog, name)?
            .values
            .get(name)
            .ok_or_else(|| missing(catalog, name))?
            .clone();
        self.field_type(catalog, &value.field_type)?;
        if let Some(items) = &value.array_items {
            self.value(catalog, items.kind.registry_key().as_str())?;
            self.field_type(catalog, &items.field_type)?;
        }
        self.definitions.values.entry(id).or_insert(value);
        Ok(())
    }

    fn field_type(&mut self, catalog: &str, ty: &FieldType) -> Result<()> {
        if let FieldType::EntityRef { entry_id, target } = ty {
            let owner = if entry_id.as_str().is_empty() {
                catalog
            } else {
                entry_id.as_str()
            };
            self.record(owner, target.as_str())?;
        }
        Ok(())
    }

    fn fields(&mut self, catalog: &str, fields: &[InputFieldSchema]) -> Result<()> {
        for field in fields {
            match &field.wire {
                InputFieldWire::Registry(name) => self.value(catalog, name.as_str())?,
                InputFieldWire::Inline(input) => self.input(catalog, input)?,
            }
        }
        Ok(())
    }

    fn input(&mut self, catalog: &str, input: &InputType) -> Result<()> {
        match input {
            InputType::None => {}
            InputType::Value { field_type, .. } => self.field_type(catalog, field_type)?,
            InputType::Object { fields, .. } => self.fields(catalog, fields)?,
            InputType::Array { element_type, .. } => self.input(catalog, element_type)?,
            InputType::Union { variants } => {
                for variant in variants {
                    self.fields(catalog, &variant.fields)?;
                }
            }
        }
        Ok(())
    }
}

fn missing(catalog: &str, name: &str) -> DiscoveryMatcherError {
    DiscoveryMatcherError::MissingEvidenceDefinition {
        catalog: catalog.into(),
        name: name.into(),
    }
}

impl CatalogEvidence {
    /// Use only retrieved capabilities and their typed definitions from the authorized,
    /// pinned generation. No inferred compatibility or acquisition executes here.
    pub fn from_catalogs(
        retrieval: &RetrievalReceipt,
        catalogs: &BTreeMap<String, CGS>,
    ) -> Result<Self> {
        let mut result = Self::default();
        for candidate in &retrieval.candidates {
            let reference = &candidate.reference;
            let cap = catalogs
                .get(&reference.catalog)
                .and_then(|cgs| cgs.capabilities.get(reference.capability.as_str()))
                .ok_or_else(|| {
                    DiscoveryMatcherError::MissingCatalogueEvidence(reference.clone())
                })?;
            let catalog = &reference.catalog;
            let mut collector = DefinitionCollector {
                catalogs,
                definitions: &mut result.definitions,
                references: References::default(),
            };
            collector.record(catalog, cap.domain.as_str())?;
            if let Some(plasm_core::CapabilityReceiver::Entity { entity }) = &cap.inputs.receiver {
                collector.record(catalog, entity.as_str())?;
            }
            if let Some(output) = &cap.output_schema {
                match &output.output_type {
                    OutputType::Entity { entity_type }
                    | OutputType::Collection { entity_type, .. } => {
                        collector.record(catalog, entity_type)?
                    }
                    OutputType::EntityUnion { variants, .. } => {
                        for entity in variants {
                            collector.record(catalog, entity.as_str())?;
                        }
                    }
                    OutputType::SideEffect { .. }
                    | OutputType::Status { .. }
                    | OutputType::Custom { .. } => {}
                }
            }
            collector.fields(catalog, &cap.inputs.scope.0)?;
            collector.fields(catalog, &cap.inputs.selection.0)?;
            collector.fields(catalog, &cap.inputs.controls.0)?;
            for input in [&cap.inputs.arguments, &cap.inputs.payload]
                .into_iter()
                .flatten()
            {
                collector.input(catalog, &input.input_type)?;
            }
            let mut inputs = cap.inputs.clone();
            // Examples are not contracts. Preserve validation/defaults, omit demonstrations.
            for input in [&mut inputs.arguments, &mut inputs.payload]
                .into_iter()
                .flatten()
            {
                input.examples.clear();
            }
            result.capabilities.insert(
                reference.clone(),
                CapabilityEvidence {
                    declaration: Declaration {
                        inputs,
                        output: cap.output_schema.clone(),
                        provides: cap.provides.clone(),
                    },
                    references: collector.references,
                },
            );
        }
        Ok(result)
    }

    #[cfg(test)]
    pub(super) fn protocol_fixture(retrieval: &RetrievalReceipt) -> Self {
        // Packet/scheduling tests use synthetic narrative documents without a catalogue.
        // Projection is tested separately against actual abstract CGS fixtures.
        Self {
            definitions: Definitions::default(),
            capabilities: retrieval
                .candidates
                .iter()
                .map(|c| {
                    (
                        c.reference.clone(),
                        CapabilityEvidence {
                            declaration: Declaration {
                                inputs: CapabilityInputs::default(),
                                output: None,
                                provides: vec![],
                            },
                            references: References::default(),
                        },
                    )
                })
                .collect(),
        }
    }
}
