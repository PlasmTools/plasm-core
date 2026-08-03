//! Schema-derived placeholder values used by compile-only runtime paths.
//!
//! These values must satisfy the declared domain shape closely enough for CML to parse, format,
//! or descend into them. They are never transport values.

use indexmap::IndexMap;
use plasm_core::{FieldType, NamedValueSchema, TemporalWireFormat, Value, ValueWireFormat, CGS};

pub(crate) const ZERO_UUID: &str = "00000000-0000-0000-0000-000000000000";
const STUB_RFC3339: &str = "1970-01-01T00:00:00Z";
const STUB_ISO_DATE: &str = "1970-01-01";
pub(crate) const STUB_STRING: &str = "preflight-stub";

/// Produce a valid, deterministic placeholder for a named value during static compilation.
pub(crate) fn preflight_compile_stub_value(named_value: &NamedValueSchema, cgs: &CGS) -> Value {
    stub_value(named_value, cgs, 0)
}

fn stub_value(named_value: &NamedValueSchema, cgs: &CGS, depth: usize) -> Value {
    match &named_value.field_type {
        FieldType::Boolean => Value::Bool(false),
        FieldType::Number => Value::Float(0.0),
        FieldType::Integer => Value::Integer(0),
        FieldType::MultiSelect | FieldType::Array => Value::Array(Vec::new()),
        FieldType::Json => Value::Object(IndexMap::new()),
        FieldType::Uuid => Value::String(ZERO_UUID.to_string()),
        FieldType::Date => match named_value.value_format {
            Some(ValueWireFormat::Temporal(
                TemporalWireFormat::UnixMs | TemporalWireFormat::UnixSec,
            )) => Value::Integer(0),
            Some(ValueWireFormat::Temporal(TemporalWireFormat::Iso8601Date)) => {
                Value::String(STUB_ISO_DATE.to_string())
            }
            Some(ValueWireFormat::Temporal(TemporalWireFormat::Rfc3339)) | None => {
                Value::String(STUB_RFC3339.to_string())
            }
        },
        FieldType::EntityRef { target } => entity_ref_stub_value(cgs, target, depth),
        FieldType::String | FieldType::Select | FieldType::Blob => {
            Value::String(STUB_STRING.to_string())
        }
    }
}

fn entity_ref_stub_value(cgs: &CGS, target_name: &str, depth: usize) -> Value {
    let Some(target) = cgs.get_entity(target_name) else {
        return Value::String(STUB_STRING.to_string());
    };

    let key_stub = |key_name: &str| {
        if depth < 2 {
            target
                .fields
                .get(key_name)
                .and_then(|field| field.named_value(cgs).ok())
                .map_or_else(
                    || Value::String(STUB_STRING.to_string()),
                    |key_type| stub_value(key_type, cgs, depth + 1),
                )
        } else {
            Value::String(STUB_STRING.to_string())
        }
    };

    // Entity refs use an object only for compound identities. Unary key_vars and the ordinary
    // id_field identity are normalized to their scalar atom by live CML environment handling.
    if target.key_vars.len() >= 2 {
        return Value::Object(
            target
                .key_vars
                .iter()
                .map(|key| (key.to_string(), key_stub(key.as_str())))
                .collect(),
        );
    }

    let key_name = target
        .key_vars
        .first()
        .map(|key| key.as_str())
        .unwrap_or_else(|| target.id_field.as_str());
    key_stub(key_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hydrate_fixture() -> CGS {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/hydrate_invoke_target");
        plasm_core::load_schema(&dir).expect("load hydrate_invoke_target fixture")
    }

    #[test]
    fn emits_semantically_valid_scalar_stubs() {
        let cgs = hydrate_fixture();
        assert_eq!(
            preflight_compile_stub_value(&cgs.values["nv_request_id"], &cgs),
            Value::String(ZERO_UUID.to_string())
        );
        assert_eq!(
            preflight_compile_stub_value(&cgs.values["nv_observed_at"], &cgs),
            Value::String(STUB_RFC3339.to_string())
        );
        assert_eq!(
            preflight_compile_stub_value(&cgs.values["nv_observed_at_ms"], &cgs),
            Value::Integer(0)
        );
    }

    #[test]
    fn emits_scalar_entity_ref_stub_for_single_identity() {
        let cgs = hydrate_fixture();
        assert_eq!(
            preflight_compile_stub_value(&cgs.values["nv_team_ref"], &cgs),
            Value::String(STUB_STRING.to_string())
        );
    }
}
