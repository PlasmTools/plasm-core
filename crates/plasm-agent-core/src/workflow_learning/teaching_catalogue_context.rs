//! Catalogue-owned semantics for normalization, without transport configuration or input defaults.
use super::*;
use plasm_core::schema::{
    CapabilityInputs, CapabilityKind, CapabilitySchema, InputFieldSchema, InputFieldWire,
    InputType, NamedValueSchema, OutputType,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityTeachingContract {
    pub reference: plasm_core::prerequisites::CapabilityRef,
    pub entity: String,
    pub kind: CapabilityKind,
    pub description: String,
    pub inputs: CapabilityInputs,
    pub value_domains: BTreeMap<String, NamedValueSchema>,
    pub declared_output: Option<OutputType>,
    pub provided_fields: BTreeMap<String, NamedValueSchema>,
    pub invalidates_entities: Vec<String>,
}

impl CapabilityTeachingContract {
    pub(super) fn from_catalog(
        reference: &plasm_core::prerequisites::CapabilityRef,
        cap: &CapabilitySchema,
        cgs: &plasm_core::CGS,
    ) -> Result<Self, TeachingError> {
        let mut inputs = cap.inputs.clone();
        let mut value_domains = BTreeMap::new();
        for field in inputs
            .scope
            .0
            .iter_mut()
            .chain(inputs.selection.0.iter_mut())
            .chain(inputs.controls.0.iter_mut())
        {
            project_field(field, cgs, &mut value_domains)?;
        }
        for input in [&mut inputs.arguments, &mut inputs.payload]
            .into_iter()
            .flatten()
        {
            project_input_type(&mut input.input_type, cgs, &mut value_domains)?;
        }
        let entity = cgs
            .get_entity(cap.domain.as_str())
            .ok_or_else(|| TeachingError::CatalogDrift(reference.catalog.clone()))?;
        let mut provided_fields = BTreeMap::new();
        for name in cgs.effective_provides(cap) {
            if let Some(field) = entity.fields.get(name.as_str()) {
                provided_fields.insert(
                    name,
                    field
                        .named_value(cgs)
                        .map_err(|error| TeachingError::Schema(Box::new(error)))?
                        .clone(),
                );
            }
        }
        Ok(Self {
            reference: reference.clone(),
            entity: cap.domain.to_string(),
            kind: cap.kind,
            description: cap.description.clone(),
            inputs,
            value_domains,
            declared_output: cap
                .output_schema
                .as_ref()
                .map(|schema| schema.output_type.clone()),
            provided_fields,
            invalidates_entities: cap.invalidates_entities.clone(),
        })
    }
}

fn project_field(
    field: &mut InputFieldSchema,
    cgs: &plasm_core::CGS,
    domains: &mut BTreeMap<String, NamedValueSchema>,
) -> Result<(), TeachingError> {
    field.default = None;
    field.wire_json_path = None;
    field.wire_array_element_key = None;
    match &mut field.wire {
        InputFieldWire::Registry(key) => {
            let value = cgs.values.get(key.as_str()).ok_or_else(|| {
                TeachingError::CatalogDrift(format!("value domain {}", key.as_str()))
            })?;
            domains.insert(key.as_str().to_owned(), value.clone());
        }
        InputFieldWire::Inline(input) => project_input_type(input, cgs, domains)?,
    }
    Ok(())
}
fn project_input_type(
    input: &mut InputType,
    cgs: &plasm_core::CGS,
    domains: &mut BTreeMap<String, NamedValueSchema>,
) -> Result<(), TeachingError> {
    match input {
        InputType::Object { fields, .. } => {
            for field in fields {
                project_field(field, cgs, domains)?;
            }
        }
        InputType::Array { element_type, .. } => project_input_type(element_type, cgs, domains)?,
        InputType::Union { variants } => {
            for variant in variants {
                variant.constructor_symbol = None;
                for field in &mut variant.fields {
                    project_field(field, cgs, domains)?;
                }
            }
        }
        InputType::None | InputType::Value { .. } => {}
    }
    Ok(())
}
