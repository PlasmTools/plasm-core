//! Python tagged records retain the selected CGS variant before wire lowering.
use plasm_core::{CapabilitySchema, InputFieldWire, InputType, TypedInvokeInput, Value, CGS};

pub(super) fn is_union_tag(cap: &CapabilitySchema, name: &str) -> bool {
    cap.invocation_input_schemas()
        .any(|schema| match &schema.input_type {
            InputType::Union { variants } => {
                variants.iter().any(|variant| variant.wire.field == name)
            }
            _ => false,
        })
}

pub(super) fn normalize(
    cap: &CapabilitySchema,
    mut object: indexmap::IndexMap<String, Value>,
    cgs: &CGS,
) -> Result<Value, String> {
    let mut scope = indexmap::IndexMap::new();
    for field in cap.scope_params() {
        if let Some(value) = object.shift_remove(&field.name) {
            scope.insert(field.name.clone(), value);
        }
    }
    let schemas = cap
        .invocation_input_schemas()
        .filter(|schema| !matches!(schema.input_type, InputType::None))
        .collect::<Vec<_>>();
    if schemas.len() <= 1 {
        let value = Value::Object(object);
        let mut value = match schemas.first() {
            Some(schema) => normalize_type(value, &schema.input_type, cgs, 0)?,
            None => value,
        };
        if let Value::Object(body) = &mut value {
            body.extend(scope);
        }
        return Ok(value);
    }
    let mut normalized = scope;
    for schema in schemas {
        let keys = match &schema.input_type {
            InputType::Object { fields, .. } => fields
                .iter()
                .map(|field| field.name.clone())
                .collect::<Vec<_>>(),
            InputType::Union { variants } => variants
                .iter()
                .flat_map(|variant| {
                    std::iter::once(variant.wire.field.clone())
                        .chain(variant.fields.iter().map(|field| field.name.clone()))
                })
                .collect(),
            _ => return Err("multiple invocation lanes require record inputs".into()),
        };
        let mut part = indexmap::IndexMap::new();
        for key in keys {
            if let Some(value) = object.shift_remove(&key) {
                part.insert(key, value);
            }
        }
        let Value::Object(part) = normalize_type(Value::Object(part), &schema.input_type, cgs, 0)?
        else {
            return Err("invocation lane must normalize to a record".into());
        };
        for (key, value) in part {
            if normalized.insert(key.clone(), value).is_some() {
                return Err(format!("input lanes overlap at {key}"));
            }
        }
    }
    // Unknown fields remain visible to the ordinary capability validator.
    normalized.extend(object);
    Ok(Value::Object(normalized))
}

fn normalize_type(value: Value, ty: &InputType, cgs: &CGS, depth: usize) -> Result<Value, String> {
    if depth >= 64 {
        return Err("input type nesting exceeds 64".into());
    }
    match (value, ty) {
        (Value::Array(items), InputType::Array { element_type, .. }) => Ok(Value::Array(
            items
                .into_iter()
                .map(|item| normalize_type(item, element_type, cgs, depth + 1))
                .collect::<Result<_, _>>()?,
        )),
        (Value::Object(mut object), InputType::Object { fields, .. }) => {
            for field in fields {
                if let (Some(value), InputFieldWire::Inline(ty)) =
                    (object.get(&field.name), &field.wire)
                {
                    let normalized = normalize_type(value.clone(), ty, cgs, depth + 1)?;
                    object.insert(field.name.clone(), normalized);
                }
            }
            Ok(Value::Object(object))
        }
        (Value::Object(mut object), InputType::Union { variants }) => {
            let matching = variants
                .iter()
                .enumerate()
                .filter(|(_, variant)| {
                    object.get(&variant.wire.field)
                        == Some(&Value::String(variant.wire.value.clone()))
                })
                .collect::<Vec<_>>();
            let [(index, variant)] = matching.as_slice() else {
                return Err("union input requires exactly one declared literal discriminator and its variant fields".into());
            };
            object.shift_remove(&variant.wire.field);
            let logical = normalize_type(
                Value::Object(object),
                &plasm_core::schema::input_variant_body_type(variant),
                cgs,
                depth + 1,
            )?;
            TypedInvokeInput::from_union_variant(variant, *index, logical, cgs)
                .map(|typed| typed.to_value())
        }
        (value, _) => Ok(value),
    }
}
