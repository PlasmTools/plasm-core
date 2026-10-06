//! Encode typed temporal values at the shared CML input boundary.
use crate::{
    CapabilitySchema, FieldType, InputFieldSchema, InputFieldWire, InputType, NamedValueSchema,
    Value, CGS,
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TemporalInputError {
    #[error("temporal input nesting exceeded the supported limit")]
    NestingLimitExceeded,
    #[error("array domain has no element contract")]
    ArrayElementContractMissing,
    #[error("input domain `{domain}` is absent")]
    InputDomainMissing { domain: String },
    #[error("temporal union requires exactly one discriminator match")]
    UnionDiscriminatorMismatch,
    #[error("wrapped input `{field}` does not reference an array domain")]
    WrappedInputNotArray { field: String },
    #[error("array element domain `{domain}` is absent")]
    ArrayElementDomainMissing { domain: String },
    #[error("wrapped input `{field}` requires an array contract")]
    WrappedInputContractMismatch { field: String },
    #[error("temporal input domain declares a money wire format")]
    UnexpectedMoneyFormat,
    #[error(transparent)]
    Temporal(#[from] crate::TemporalNormalizationError),
}

pub fn encode_capability_temporals(
    input: &mut Value,
    cap: &CapabilitySchema,
    cgs: &CGS,
) -> Result<(), TemporalInputError> {
    for schema in cap.invocation_input_schemas() {
        encode_type(input, &schema.input_type, cgs, 0)?;
    }
    if let Value::Object(fields) = input {
        for field in cap
            .scope_params()
            .iter()
            .chain(cap.selection_params())
            .chain(cap.control_params())
        {
            if let Some(value) = fields.get_mut(&field.name) {
                encode_field(value, field, cgs, 0)?;
            }
        }
    }
    Ok(())
}

/// Encode a bound domain value, including nested arrays, without guessing a wire type.
pub fn encode_domain_temporals(
    value: &mut Value,
    domain: &NamedValueSchema,
    cgs: &CGS,
) -> Result<(), TemporalInputError> {
    encode_domain(value, domain, cgs, 0)
}

fn encode_domain(
    value: &mut Value,
    domain: &NamedValueSchema,
    cgs: &CGS,
    depth: usize,
) -> Result<(), TemporalInputError> {
    if depth >= 64 {
        return Err(TemporalInputError::NestingLimitExceeded);
    }
    if domain.field_type == FieldType::Date
        && matches!(&*value, Value::Object(o) if o.contains_key("__plasm_temporal"))
    {
        let wire = match domain.value_format {
            Some(crate::ValueWireFormat::Temporal(wire)) => wire,
            None => crate::TemporalWireFormat::Rfc3339,
            Some(crate::ValueWireFormat::Money(_)) => {
                return Err(TemporalInputError::UnexpectedMoneyFormat);
            }
        };
        *value = crate::temporal::normalize_temporal_value(value.clone(), wire)?;
    } else if domain.field_type == FieldType::Array {
        if let Value::Array(items) = value {
            let key = domain
                .array_items
                .as_ref()
                .ok_or(TemporalInputError::ArrayElementContractMissing)?
                .kind
                .registry_key();
            let element = cgs.values.get(key.as_str()).ok_or_else(|| {
                TemporalInputError::InputDomainMissing {
                    domain: key.to_string(),
                }
            })?;
            for item in items {
                encode_domain(item, element, cgs, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn encode_field(
    value: &mut Value,
    field: &InputFieldSchema,
    cgs: &CGS,
    depth: usize,
) -> Result<(), TemporalInputError> {
    match &field.wire {
        InputFieldWire::Registry(key) => encode_domain(
            value,
            cgs.values
                .get(key.as_str())
                .ok_or_else(|| TemporalInputError::InputDomainMissing {
                    domain: key.to_string(),
                })?,
            cgs,
            depth + 1,
        ),
        InputFieldWire::Inline(ty) => encode_type(value, ty, cgs, depth + 1),
    }
}

fn encode_type(
    value: &mut Value,
    ty: &InputType,
    cgs: &CGS,
    depth: usize,
) -> Result<(), TemporalInputError> {
    if depth >= 64 {
        return Err(TemporalInputError::NestingLimitExceeded);
    }
    match (value, ty) {
        (Value::Object(object), InputType::Object { fields, .. }) => {
            for field in fields {
                if let Some(value) = object.get_mut(&field.name) {
                    encode_field(value, field, cgs, depth + 1)?;
                }
            }
        }
        (Value::Array(items), InputType::Array { element_type, .. }) => {
            for item in items {
                encode_type(item, element_type, cgs, depth + 1)?;
            }
        }
        (Value::Object(object), InputType::Union { variants }) => {
            let matching = variants
                .iter()
                .filter(|variant| {
                    object.get(&variant.wire.field)
                        == Some(&Value::String(variant.wire.value.clone()))
                })
                .collect::<Vec<_>>();
            let [variant] = matching.as_slice() else {
                return Err(TemporalInputError::UnionDiscriminatorMismatch);
            };
            for field in &variant.fields {
                let mut slot = None;
                if let Some(path) = &field.wire_json_path {
                    let segments = path.iter().map(String::as_str).collect::<Vec<_>>();
                    if let Some((head, tail)) = segments.split_first() {
                        slot = object.get_mut(*head);
                        for part in tail {
                            slot = slot.and_then(|v| {
                                if let Value::Object(o) = v {
                                    o.get_mut(*part)
                                } else {
                                    None
                                }
                            });
                        }
                    }
                } else {
                    slot = object.get_mut(&field.name);
                }
                if let Some(value) = slot {
                    if let Some(key) = &field.wire_array_element_key {
                        if let Value::Array(items) = value {
                            for item in items {
                                if let Value::Object(wrapper) = item {
                                    if let Some(inner) = wrapper.get_mut(key) {
                                        // The wire wrapper belongs to the array, not its element domain.
                                        match &field.wire {
                                            InputFieldWire::Inline(ty)
                                                if matches!(&**ty, InputType::Array { .. }) =>
                                            {
                                                if let InputType::Array { element_type, .. } = &**ty
                                                {
                                                    encode_type(
                                                        inner,
                                                        element_type,
                                                        cgs,
                                                        depth + 1,
                                                    )?;
                                                }
                                            }
                                            InputFieldWire::Registry(k) => {
                                                let domain =
                                                    cgs.values.get(k.as_str()).ok_or_else(
                                                        || TemporalInputError::InputDomainMissing {
                                                            domain: k.to_string(),
                                                        },
                                                    )?;
                                                let element = domain
                                                    .array_items
                                                    .as_ref()
                                                    .ok_or_else(|| {
                                                        TemporalInputError::WrappedInputNotArray {
                                                            field: field.name.clone(),
                                                        }
                                                    })?
                                                    .kind
                                                    .registry_key();
                                                encode_domain(
                                                    inner,
                                                    cgs.values
                                                        .get(element.as_str())
                                                        .ok_or_else(|| {
                                                            TemporalInputError::ArrayElementDomainMissing {
                                                                domain: element.to_string(),
                                                            }
                                                        })?,
                                                    cgs,
                                                    depth + 1,
                                                )?;
                                            }
                                            _ => return Err(
                                                TemporalInputError::WrappedInputContractMismatch {
                                                    field: field.name.clone(),
                                                },
                                            ),
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        encode_field(value, field, cgs, depth + 1)?;
                    }
                }
            }
        }
        (
            value,
            InputType::Value {
                field_type: FieldType::Date,
                ..
            },
        ) if matches!(&*value, Value::Object(o) if o.contains_key("__plasm_temporal")) => {
            *value = crate::temporal::normalize_temporal_value(
                value.clone(),
                crate::TemporalWireFormat::Rfc3339,
            )?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::temporal_value::{tagged, TemporalKind};
    use serde_json::json;

    #[test]
    fn temporal_input_recurses_through_records_arrays_and_remapped_unions() {
        let cgs = crate::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_value_contract"),
        )
        .unwrap();
        let cap = cgs.get_capability("sample_update_times").unwrap();
        let date = tagged(
            TemporalKind::Date,
            crate::fixture_value!({"year":2024,"month":2,"day":29}),
        );
        let mut input = crate::json_value_to_plasm_value(
            &json!({"date":date,"timestamp":"2024-02-29T00:00:00Z","epoch":-1}),
        );
        encode_capability_temporals(&mut input, cap, &cgs).unwrap();
        assert_eq!(
            serde_json::to_value(input).unwrap(),
            json!({"date":"2024-02-29","timestamp":"2024-02-29T00:00:00Z","epoch":-1})
        );

        let mut date_field = cap
            .invocation_object_fields()
            .find(|field| field.name == "date")
            .unwrap()
            .clone();
        let record = InputType::Object {
            fields: vec![date_field.clone()],
            additional_fields: false,
        };
        date_field.name = "events".into();
        date_field.wire = InputFieldWire::Inline(Box::new(InputType::Array {
            element_type: Box::new(record),
            min_length: None,
            max_length: None,
        }));
        date_field.wire_json_path = Some(vec!["payload".into(), "events".into()]);
        date_field.wire_array_element_key = Some("entry".into());
        let union = InputType::Union {
            variants: vec![crate::schema::InputVariantSchema {
                name: "scheduled".into(),
                description: None,
                constructor_symbol: None,
                fields: vec![date_field],
                wire: crate::schema::WireVariantDiscriminator {
                    field: "kind".into(),
                    value: "scheduled".into(),
                },
            }],
        };
        let mut value = crate::json_value_to_plasm_value(
            &json!({"kind":"scheduled","payload":{"events":[{"entry":{"date":date}}]}}),
        );
        encode_type(&mut value, &union, &cgs, 0).unwrap();
        assert_eq!(
            serde_json::to_value(value).unwrap(),
            json!({"kind":"scheduled","payload":{"events":[{"entry":{"date":"2024-02-29"}}]}})
        );
    }

    #[test]
    fn temporal_input_encodes_each_parameter_lane_recursively() {
        let cgs = crate::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_value_contract"),
        )
        .unwrap();
        let prototype = cgs.get_capability("sample_update_times").unwrap();
        let date_field = prototype
            .invocation_object_fields()
            .find(|field| field.name == "date")
            .unwrap()
            .clone();
        let date = tagged(
            TemporalKind::Date,
            crate::fixture_value!({"year":2024,"month":2,"day":29}),
        );
        for lane in 0..3 {
            let mut cap = prototype.clone();
            cap.inputs = Default::default();
            let mut field = date_field.clone();
            field.name = "windows".into();
            field.wire = InputFieldWire::Inline(Box::new(InputType::Array {
                element_type: Box::new(InputType::Object {
                    fields: vec![date_field.clone()],
                    additional_fields: false,
                }),
                min_length: None,
                max_length: None,
            }));
            match lane {
                0 => cap.inputs.scope.0.push(field),
                1 => cap.inputs.selection.0.push(field),
                _ => cap.inputs.controls.0.push(field),
            }
            let mut input =
                crate::json_value_to_plasm_value(&json!({"windows":[{"date":date},{"date":null}]}));
            encode_capability_temporals(&mut input, &cap, &cgs).unwrap();
            assert_eq!(
                serde_json::to_value(input).unwrap(),
                json!({"windows":[{"date":"2024-02-29"},{"date":null}]}),
                "lane {lane}"
            );
        }
    }

    #[test]
    fn temporal_input_rejects_loss_before_transport() {
        let cgs = crate::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_value_contract"),
        )
        .unwrap();
        let cap = cgs.get_capability("sample_update_times").unwrap();
        for offset in [json!(null), json!(0)] {
            let instant = tagged(
                TemporalKind::Datetime,
                crate::fixture_value!({"year":2024,"month":1,"day":1,"hour":0,"minute":0,"second":0,"microsecond":1,"offset_seconds":offset}),
            );
            let mut input = crate::json_value_to_plasm_value(&json!({"epoch":instant}));
            assert!(encode_capability_temporals(&mut input, cap, &cgs).is_err());
        }
    }
}
