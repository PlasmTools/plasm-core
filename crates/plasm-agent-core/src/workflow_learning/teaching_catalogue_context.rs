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
    /// Semantic catalogue text for model context; wire metadata and provenance stay typed internally.
    pub fn teaching_text(&self) -> String {
        use std::fmt::Write as _;
        let mut text = format!(
            "{} / {} on {} ({:?}): {}",
            self.reference.catalog,
            self.reference.capability,
            self.entity,
            self.kind,
            self.description
        );
        if let Some(receiver) = &self.inputs.receiver {
            match receiver {
                plasm_core::schema::CapabilityReceiver::None => text.push_str("\n  receiver: none"),
                plasm_core::schema::CapabilityReceiver::Entity { entity } => {
                    let _ = write!(text, "\n  receiver: {entity}");
                }
            }
        }
        if self.inputs.input_validation.allow_null {
            text.push_str("\n  input permits null");
        }
        for (lane, fields) in [
            ("scope", &self.inputs.scope.0),
            ("selection", &self.inputs.selection.0),
            ("control", &self.inputs.controls.0),
        ] {
            for field in fields {
                let required = if field.required {
                    "required"
                } else {
                    "optional"
                };
                let meaning = field.description.as_deref().unwrap_or("");
                let selection = field
                    .selection_effect
                    .map(|effect| effect.semantic_gloss())
                    .unwrap_or("");
                let domain = match &field.wire {
                    InputFieldWire::Registry(key) => self
                        .value_domains
                        .get(key.as_str())
                        .map(|value| {
                            let values = value
                                .allowed_values
                                .as_ref()
                                .map(|values| format!(" [{}]", values.join(", ")))
                                .unwrap_or_default();
                            format!("{}: {}{}", value.field_type, value.description, values)
                        })
                        .unwrap_or_else(|| key.as_str().to_owned()),
                    InputFieldWire::Inline(input) => render_input_type(input),
                };
                let _ = write!(
                    text,
                    "\n  {lane} {} ({required}): {domain}; {meaning} {selection}",
                    field.name
                );
            }
        }
        for (lane, input) in [
            ("arguments", self.inputs.arguments.as_ref()),
            ("payload", self.inputs.payload.as_ref()),
        ] {
            if let Some(input) = input {
                let _ = write!(text, "\n  {lane}: {}", render_input_type(&input.input_type));
                if input.validation.allow_null {
                    text.push_str("; null allowed");
                }
                if let Some(description) = &input.description {
                    let _ = write!(text, "; {description}");
                }
                for rule in &input.validation.cross_field_rules {
                    let _ = write!(text, "\n  constraint: {}", render_rule(rule));
                }
            }
        }
        for rule in &self.inputs.input_validation.cross_field_rules {
            let _ = write!(text, "\n  constraint: {}", render_rule(rule));
        }
        for (name, value) in &self.value_domains {
            let values = value
                .allowed_values
                .as_ref()
                .map(|v| format!(" [{}]", v.join(", ")))
                .unwrap_or_default();
            let currency = value
                .currency
                .as_ref()
                .map(|v| format!("; currency {v}"))
                .unwrap_or_default();
            let _ = write!(
                text,
                "\n  value {name}: {} {}{values}{currency}",
                value.field_type, value.description
            );
            let constraints = &value.domain.constraints;
            if let Some(profile) = &value.domain.profile {
                let _ = write!(text, "; profile {profile:?}");
            }
            if let Some(v) = constraints.min_length {
                let _ = write!(text, "; minimum length {v}");
            }
            if let Some(v) = constraints.max_length {
                let _ = write!(text, "; maximum length {v}");
            }
            if let Some(v) = &constraints.pattern {
                let _ = write!(text, "; pattern {v}");
            }
            if let Some(v) = constraints.min {
                let _ = write!(text, "; minimum {v} inclusive");
            }
            if let Some(v) = constraints.max {
                let _ = write!(text, "; maximum {v} inclusive");
            }
            if let Some(v) = constraints.exclusive_min {
                let _ = write!(text, "; minimum {v} exclusive");
            }
            if let Some(v) = constraints.exclusive_max {
                let _ = write!(text, "; maximum {v} exclusive");
            }
            if let Some(v) = constraints.multiple_of {
                let _ = write!(text, "; multiple of {v}");
            }
        }
        for (name, value) in &self.provided_fields {
            let _ = write!(
                text,
                "\n  provides {name}: {} {}",
                value.field_type, value.description
            );
        }
        if let Some(output) = &self.declared_output {
            match output {
                OutputType::SideEffect { description } => {
                    let _ = write!(text, "\n  effect: {description}");
                }
                OutputType::Entity { entity_type } => {
                    let _ = write!(text, "\n  returns: {entity_type}");
                }
                OutputType::Collection {
                    entity_type,
                    max_count,
                } => {
                    let _ = write!(text, "\n  returns: list of {entity_type}");
                    if let Some(max) = max_count {
                        let _ = write!(text, " (maximum {max})");
                    }
                }
                OutputType::EntityUnion {
                    variants,
                    cardinality,
                } => {
                    let _ = write!(
                        text,
                        "\n  returns: {cardinality:?} of {}",
                        variants
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(" | ")
                    );
                }
                OutputType::Status { success_indicators } => {
                    let _ = write!(
                        text,
                        "\n  returns: status ({})",
                        success_indicators.join(", ")
                    );
                }
                OutputType::Custom { .. } => {
                    let _ = write!(
                        text,
                        "\n  returns: custom structured output; detailed schema not included"
                    );
                }
            }
        }
        if !self.invalidates_entities.is_empty() {
            let _ = write!(
                text,
                "\n  invalidates: {}",
                self.invalidates_entities.join(", ")
            );
        }
        text
    }

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

fn render_input_type(input: &InputType) -> String {
    match input {
        InputType::None => "no input".into(),
        InputType::Value {
            field_type,
            allowed_values,
        } => {
            let values = allowed_values
                .as_ref()
                .map(|v| format!(" [{}]", v.join(", ")))
                .unwrap_or_default();
            format!("{field_type}{values}")
        }
        InputType::Array {
            element_type,
            min_length,
            max_length,
        } => format!(
            "list[{}] (min length: {}, max length: {})",
            render_input_type(element_type),
            min_length.map_or("unbounded".into(), |v| v.to_string()),
            max_length.map_or("unbounded".into(), |v| v.to_string())
        ),
        InputType::Object {
            fields,
            additional_fields,
        } => format!(
            "{{{}}}{}",
            fields
                .iter()
                .map(render_input_field)
                .collect::<Vec<_>>()
                .join(", "),
            if *additional_fields {
                " (additional fields allowed)"
            } else {
                ""
            }
        ),
        InputType::Union { variants } => variants
            .iter()
            .map(|variant| {
                format!(
                    "{}: {} {{{}}}",
                    variant.name,
                    variant.description.as_deref().unwrap_or(""),
                    variant
                        .fields
                        .iter()
                        .map(render_input_field)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join(" | "),
    }
}

fn render_input_field(field: &InputFieldSchema) -> String {
    let kind = match &field.wire {
        InputFieldWire::Registry(key) => key.as_str().to_owned(),
        InputFieldWire::Inline(input) => render_input_type(input),
    };
    format!(
        "{}{}: {} {}",
        field.name,
        if field.required { "" } else { "?" },
        kind,
        field.description.as_deref().unwrap_or("")
    )
}

fn render_rule(rule: &plasm_core::schema::CrossFieldRule) -> String {
    use plasm_core::schema::CrossFieldRuleType;
    let action = match rule.rule_type {
        CrossFieldRuleType::AtLeastOne => "at least one required",
        CrossFieldRuleType::ExactlyOne => "exactly one required",
        CrossFieldRuleType::AllOrNone => "all or none required",
        CrossFieldRuleType::Implies => "first requires second",
        CrossFieldRuleType::MutuallyExclusive => "mutually exclusive",
        CrossFieldRuleType::RequiredAll => "all required",
        CrossFieldRuleType::Forbidden => "forbidden",
    };
    let condition = rule
        .when
        .as_ref()
        .map(|when| format!(" when {} equals {:?}", when.field, when.equals))
        .unwrap_or_default();
    format!(
        "{action}: {}{condition}; {}",
        rule.fields.join(", "),
        rule.error_message
    )
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
