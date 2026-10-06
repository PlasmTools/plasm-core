//! # Catalog-directed value coercion (RA-8)
//!
//! **Sole public law** for turning a raw [`Value`] / JSON cell into a catalog-typed cell.
//! Program literals, wire decode, invoke args, compare-unify, and dry stubs all call through
//! this module. Validate / compatible paths are coerce-then-domain — they must not maintain a
//! second, divergent type matrix.
//!
//! Modes (see `docs/plasm-language-surface-invariants.md` § RA-8):
//! - **ProgramLiteral** / query filters — [`coerce_value_for_field_type`] (default array wrap)
//! - **InvokeArg** — [`coerce_value_for_field_type_with_policy`] + [`ArrayFieldCoercionPolicy::InvokeArg`]
//! - **WireDecode** — [`decode_coerce_and_validate_field`]
//! - **CompareUnify** — [`compare_unify_json_ordered_numbers`] (residual JSON ordered compare)
//! - **DryStub** — [`dry_stub_value_for_named_value`] / [`dry_stub_json_for_named_value`]
//!
//! Relation-binding assignability helpers also live here (parent field → param after coerce).

use crate::array_field_policy::ArrayFieldCoercionPolicy;
use crate::capability_input::validate_named_value_domain_value;
use crate::{ArrayItemsSchema, FieldType, NamedValueSchema, Value, ValueWireFormat};
use indexmap::IndexMap;

mod digit_id;
mod dry_stub;
mod relation_binding;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoercionError {
    #[error("string template cannot bind a {field_type} operand")]
    StringTemplateUnsupported { field_type: String },
    #[error("array operand cannot accept scalar value of type {actual}")]
    ArrayScalarRejected { actual: &'static str },
    #[error(transparent)]
    Temporal(#[from] crate::TemporalNormalizationError),
    #[error("date field is missing a temporal value format")]
    MissingTemporalFormat,
    #[error("cannot coerce {actual} to {field_type}")]
    UnsupportedValue {
        field_type: &'static str,
        actual: &'static str,
    },
    #[error("invalid integer literal")]
    InvalidIntegerLiteral {
        raw: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("invalid integer value of type {actual}")]
    InvalidIntegerValue { actual: &'static str },
    #[error("invalid number literal")]
    InvalidNumberLiteral {
        raw: String,
        #[source]
        source: std::num::ParseFloatError,
    },
    #[error("invalid number value of type {actual}")]
    InvalidNumberValue { actual: &'static str },
    #[error("invalid boolean literal")]
    InvalidBooleanLiteral { raw: String },
    #[error("invalid boolean value of type {actual}")]
    InvalidBooleanValue { actual: &'static str },
    #[error("JSON value must be a top-level object or array")]
    InvalidJsonLiteral,
    #[error("cannot coerce {actual} to JSON")]
    InvalidJsonValue { actual: &'static str },
    #[error(transparent)]
    DigitId(#[from] DigitIdCoercionError),
    #[error(transparent)]
    Money(#[from] crate::MoneyError),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireEncodingError {
    #[error("unbound string template reached wire encoding")]
    UnboundStringTemplate,
    #[error(transparent)]
    Money(#[from] crate::MoneyError),
}

pub use digit_id::DigitIdCoercionError;
pub(crate) use digit_id::{coerce_digit_id, digit_id_json_to_plasm, encode_digit_id_identity};
pub use dry_stub::{
    dry_stub_entity_rows, dry_stub_json_for_named_value, dry_stub_value_for_named_value,
};
pub use relation_binding::{
    apply_identity_slots_to_row, binding_value_as_plasm_value, collect_relation_binding_proofs,
    field_type_assignable_for_relation_binding, identity_slot_to_json, identity_slot_to_value,
    parent_entity_field_type, relation_binding_assignable, restore_id_field_from_compound_ref,
    ParentFieldTypeError, RelationBindingProof, RelationBindingProofError,
};

pub(crate) fn stringish(val: &Value) -> Option<&str> {
    match val {
        Value::String(s) | Value::PhraseIdent(s) => Some(s.as_str()),
        _ => None,
    }
}

fn phrase_ident_to_string(val: Value) -> Value {
    match val {
        Value::PhraseIdent(s) => Value::String(s),
        other => other,
    }
}

/// Coerce a parsed predicate / env token for typecheck and downstream HTTP binding (ProgramLiteral).
///
/// Same entry point for **read** filters (`{field=…}`) and (via
/// [`coerce_value_for_field_type_with_policy`]) **write** capability inputs — do not add
/// read-only or write-only scalar coercions outside this module.
pub fn coerce_value_for_field_type(
    ft: &FieldType,
    value_format: Option<ValueWireFormat>,
    array_items: Option<&ArrayItemsSchema>,
    val: Value,
) -> Result<Value, CoercionError> {
    coerce_value_for_field_type_with_policy(
        ft,
        value_format,
        array_items,
        val,
        ArrayFieldCoercionPolicy::QueryFilter,
    )
}

/// Coerce with explicit array policy (invoke args use [`ArrayFieldCoercionPolicy::InvokeArg`]).
///
/// Scalar / typed literal rules are **identical** for query and invoke; only array scalar-wrap
/// differs by [`ArrayFieldCoercionPolicy`]. Failed scalar coercions **Err** (no soft leave-as-string).
pub fn coerce_value_for_field_type_with_policy(
    ft: &FieldType,
    value_format: Option<ValueWireFormat>,
    array_items: Option<&ArrayItemsSchema>,
    val: Value,
    array_policy: ArrayFieldCoercionPolicy,
) -> Result<Value, CoercionError> {
    // Teaching-table bare `$` is a fill-in slot, not a wire token — pass through for every field
    // type (including temporal `Date`) so optional params can appear as `p#=$` in method rows.
    if val.is_domain_example_placeholder() {
        return Ok(val);
    }
    if matches!(val, Value::StringTemplate(_)) {
        return if matches!(ft, FieldType::String | FieldType::Blob) {
            Ok(val)
        } else {
            Err(CoercionError::StringTemplateUnsupported {
                field_type: format!("{ft:?}"),
            })
        };
    }
    if matches!(
        val,
        Value::Null | Value::PlasmInputRef(_) | Value::GetScalarExtract(_)
    ) {
        return Ok(val);
    }
    match ft {
        FieldType::Array => {
            let coerce_elem = |v: Value| -> Result<Value, CoercionError> {
                match array_items {
                    Some(items) => {
                        coerce_value_for_field_type(&items.field_type, items.value_format, None, v)
                    }
                    None => Ok(v),
                }
            };
            match val {
                Value::Array(elements) => {
                    let mut out = Vec::with_capacity(elements.len());
                    for e in elements {
                        out.push(coerce_elem(e)?);
                    }
                    Ok(Value::Array(out))
                }
                Value::PlasmInputRef(_)
                    if ArrayFieldCoercionPolicy::accepts_deferred_value(&val) =>
                {
                    Ok(val)
                }
                other if array_policy.allows_scalar_wrap() => {
                    Ok(Value::Array(vec![coerce_elem(other)?]))
                }
                other => Err(CoercionError::ArrayScalarRejected {
                    actual: other.type_name(),
                }),
            }
        }
        FieldType::Date => {
            // Internal literal tokens cross the same strict transport boundary.
            let val = phrase_ident_to_string(val);
            match value_format {
                Some(ValueWireFormat::Temporal(fmt)) => {
                    Ok(crate::temporal::normalize_temporal_value(val, fmt)?)
                }
                None if matches!(&val, Value::Object(o) if o.contains_key("__plasm_temporal")) => {
                    Ok(crate::temporal::normalize_temporal_value(
                        val,
                        crate::TemporalWireFormat::Rfc3339,
                    )?)
                }
                None => match val {
                    Value::String(_) | Value::Integer(_) | Value::Float(_) => Ok(val),
                    other => Err(CoercionError::UnsupportedValue {
                        field_type: "date",
                        actual: other.type_name(),
                    }),
                },
                Some(ValueWireFormat::Money(_)) => Err(CoercionError::MissingTemporalFormat),
            }
        }
        FieldType::DigitId => Ok(coerce_digit_id(val)?),
        FieldType::String | FieldType::Uuid | FieldType::Select => Ok(match val {
            Value::Integer(n) => Value::String(n.to_string()),
            Value::Unsigned(n) => Value::String(n.to_string()),
            Value::Float(f) => Value::String(normalize_numeric_id_float(f)),
            Value::PhraseIdent(s) => Value::String(s),
            Value::String(s) => Value::String(s),
            other => {
                return Err(CoercionError::UnsupportedValue {
                    field_type: match ft {
                        FieldType::String => "string",
                        FieldType::Uuid => "uuid",
                        FieldType::Select => "select",
                        _ => unreachable!(),
                    },
                    actual: other.type_name(),
                });
            }
        }),
        // Opaque payloads have JSON representation; normalization must not erase
        // object/array contents or replace a valid blob with null.
        FieldType::Blob => Ok(val),
        FieldType::MultiSelect => match val {
            Value::Array(_) => Ok(val),
            Value::PhraseIdent(s) => Ok(Value::String(s)),
            Value::String(s) => Ok(Value::String(s)),
            other => Err(CoercionError::UnsupportedValue {
                field_type: "multi_select",
                actual: other.type_name(),
            }),
        },
        FieldType::Integer => {
            if let Some(s) = stringish(&val) {
                return s.parse::<i64>().map(Value::Integer).map_err(|source| {
                    CoercionError::InvalidIntegerLiteral {
                        raw: s.into(),
                        source,
                    }
                });
            }
            match val {
                Value::Integer(n) => Ok(Value::Integer(n)),
                Value::Float(f)
                    if f.fract() == 0.0
                        && f.is_finite()
                        && f >= i64::MIN as f64
                        && f < -(i64::MIN as f64) =>
                {
                    Ok(Value::Integer(f as i64))
                }
                other => Err(CoercionError::InvalidIntegerValue {
                    actual: other.type_name(),
                }),
            }
        }
        FieldType::Number => {
            if let Some(s) = stringish(&val) {
                return s.parse::<f64>().map(Value::Float).map_err(|source| {
                    CoercionError::InvalidNumberLiteral {
                        raw: s.into(),
                        source,
                    }
                });
            }
            match val {
                Value::Integer(n) => Ok(Value::Float(n as f64)),
                Value::Unsigned(n) => Ok(Value::Float(n as f64)),
                Value::Float(f) => Ok(Value::Float(f)),
                other => Err(CoercionError::InvalidNumberValue {
                    actual: other.type_name(),
                }),
            }
        }
        FieldType::EntityRef { .. } => Ok(match val {
            Value::Integer(n) => Value::String(n.to_string()),
            Value::Unsigned(n) => Value::String(n.to_string()),
            Value::Float(f) => Value::String(normalize_numeric_id_float(f)),
            Value::PhraseIdent(s) => Value::String(s),
            Value::String(s) => Value::String(s),
            Value::Object(o) => Value::Object(o),
            other => {
                return Err(CoercionError::UnsupportedValue {
                    field_type: "entity_ref",
                    actual: other.type_name(),
                });
            }
        }),
        FieldType::Boolean => match stringish(&val) {
            // RA-8: reject `"1"` / `"0"` — only true/false tokens.
            Some(s) if s.eq_ignore_ascii_case("true") => Ok(Value::Bool(true)),
            Some(s) if s.eq_ignore_ascii_case("false") => Ok(Value::Bool(false)),
            Some(s) => Err(CoercionError::InvalidBooleanLiteral { raw: s.into() }),
            None => match val {
                Value::Bool(b) => Ok(Value::Bool(b)),
                other => Err(CoercionError::InvalidBooleanValue {
                    actual: other.type_name(),
                }),
            },
        },
        FieldType::Json => match val {
            Value::String(ref s) if s.as_str() == "$" => Ok(val),
            Value::String(s) => crate::value::parse_json_subtree_str(&s)
                .ok_or_else(|| CoercionError::InvalidJsonLiteral),
            Value::PhraseIdent(s) => crate::value::parse_json_subtree_str(&s)
                .ok_or_else(|| CoercionError::InvalidJsonLiteral),
            Value::Object(_) | Value::Array(_) => Ok(val),
            other => Err(CoercionError::InvalidJsonValue {
                actual: other.type_name(),
            }),
        },
        FieldType::Money => {
            // Doctrine: money wire is decimal string only.
            let fmt = crate::money::MoneyWireFormat::DecimalString;
            let val = phrase_ident_to_string(val);
            crate::money::normalize(val, fmt, None).map_err(CoercionError::Money)
        }
    }
}

/// Whether `value` can occupy `field_type` under RA-8 (successful coerce, optional formats absent).
///
/// Prefer [`coerce_value_for_field_type`] + domain validate when a [`NamedValueSchema`] is available.
pub fn value_compatible_with_field_type(value: &Value, field_type: &FieldType) -> bool {
    let Ok(coerced) = coerce_value_for_field_type(field_type, None, None, value.clone()) else {
        return false;
    };
    if matches!(field_type, FieldType::EntityRef { .. }) {
        return matches!(
            &coerced,
            Value::String(_)
                | Value::Integer(_)
                | Value::Float(_)
                | Value::Null
                | Value::PlasmInputRef(_)
        ) || crate::entity_ref_value::EntityRefPayload::value_is_legal_shape(&coerced);
    }
    true
}

/// CompareUnify residual: parse JSON cells as ordered numbers (wire string decimals allowed).
///
/// Returns `None` when either side is not numeric under RA-8 ordered-compare rules (no bool/`"1"`).
pub fn compare_unify_json_ordered_numbers(
    lhs: &serde_json::Value,
    rhs: &serde_json::Value,
) -> Option<(f64, f64)> {
    Some((json_ordered_number(lhs)?, json_ordered_number(rhs)?))
}

fn json_ordered_number(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n
            .as_f64()
            .or_else(|| n.as_i64().map(|i| i as f64))
            .or_else(|| n.as_u64().map(|u| u as f64)),
        serde_json::Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                return None;
            }
            t.parse::<f64>().ok().filter(|f| f.is_finite())
        }
        _ => None,
    }
}

pub fn coerce_json_value_for_field_type(
    ft: &FieldType,
    value_format: Option<ValueWireFormat>,
    array_items: Option<&ArrayItemsSchema>,
    value: serde_json::Value,
) -> serde_json::Value {
    if matches!(ft, FieldType::DigitId) {
        return match coerce_digit_id(digit_id_json_to_plasm(&value)) {
            Ok(v) => try_plasm_value_to_json(&v).unwrap_or(serde_json::Value::Null),
            Err(_) => serde_json::Value::Null,
        };
    }
    if let serde_json::Value::Number(number) = &value {
        if matches!(
            ft,
            FieldType::String | FieldType::Select | FieldType::EntityRef { .. }
        ) {
            return serde_json::Value::String(number.to_string());
        }
        // Preserve out-of-domain integers for the response-contract validator; never round them.
        if number.is_u64() && number.as_i64().is_none() {
            return value;
        }
    }
    let plasm = json_to_plasm_for_field(ft, &value);
    match coerce_value_for_field_type(ft, value_format, array_items, plasm) {
        Ok(v) => match try_plasm_value_to_json(&v) {
            Ok(j) => j,
            Err(_) => value,
        },
        Err(_) => value,
    }
}

pub(crate) fn json_to_plasm_for_field(ft: &FieldType, value: &serde_json::Value) -> Value {
    if matches!(ft, FieldType::Money) {
        crate::money::json_amount_to_value(value)
    } else if matches!(ft, FieldType::DigitId) {
        digit_id_json_to_plasm(value)
    } else {
        json_value_to_plasm_value(value)
    }
}

pub fn json_value_to_plasm_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(n) = n.as_u64() {
                Value::Unsigned(n)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(items) => {
            Value::Array(items.iter().map(json_value_to_plasm_value).collect())
        }
        serde_json::Value::Object(map) => {
            if let Some(m) = crate::money::try_from_json_object(map) {
                Value::Money(m)
            } else {
                Value::Object(
                    map.iter()
                        .map(|(k, v)| (k.clone(), json_value_to_plasm_value(v)))
                        .collect(),
                )
            }
        }
    }
}

pub fn try_plasm_value_to_json(v: &Value) -> Result<serde_json::Value, WireEncodingError> {
    if let Some(s) = v.as_string_or_phrase() {
        return Ok(serde_json::Value::String(s.to_string()));
    }
    match v {
        Value::StringTemplate(_) => Err(WireEncodingError::UnboundStringTemplate),
        Value::Null => Ok(serde_json::Value::Null),
        Value::Bool(b) => Ok(serde_json::Value::Bool(*b)),
        Value::Integer(i) => Ok(serde_json::json!(i)),
        Value::Unsigned(i) => Ok(serde_json::json!(i)),
        Value::Float(f) => Ok(serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null)),
        Value::Array(items) => Ok(serde_json::Value::Array(
            items
                .iter()
                .map(try_plasm_value_to_json)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Value::Object(map) => Ok(serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), try_plasm_value_to_json(v)?)))
                .collect::<Result<_, WireEncodingError>>()?,
        )),
        Value::PlasmInputRef(_)
        | Value::GetScalarExtract(_)
        | Value::UnionCtor { .. }
        | Value::String(_)
        | Value::PhraseIdent(_) => Ok(serde_json::Value::Null),
        Value::Money(m) => m.encode_stored().map_err(WireEncodingError::Money),
    }
}

pub fn plasm_value_to_json(v: &Value) -> serde_json::Value {
    try_plasm_value_to_json(v).unwrap_or(serde_json::Value::Null)
}

fn normalize_numeric_id_float(f: f64) -> String {
    if f.fract() == 0.0 && f.is_finite() {
        format!("{}", f as i64)
    } else {
        f.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("response field `{field}` violates its declared type: {source}")]
pub struct DecodeFieldDiagnostic {
    pub field: String,
    #[source]
    pub source: DecodeFieldCause,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeFieldCause {
    #[error(transparent)]
    Money(#[from] crate::MoneyError),
    #[error(transparent)]
    Coercion(#[from] CoercionError),
    #[error(transparent)]
    Domain(#[from] crate::ValueDomainViolation),
    #[error("money currency field `{field}` must be a string (got {actual})")]
    CurrencyFieldType { field: String, actual: &'static str },
}

/// Coerce and validate a present wire value. Invalid data is never nullable absence.
pub fn decode_coerce_and_validate_field(
    field_name: &str,
    nv: &NamedValueSchema,
    val: Value,
) -> Result<Value, DecodeFieldDiagnostic> {
    let error = |source: DecodeFieldCause| DecodeFieldDiagnostic {
        field: field_name.to_owned(),
        source,
    };
    let coerced = if matches!(nv.field_type, FieldType::Money) {
        crate::money::normalize(
            phrase_ident_to_string(val),
            crate::money::MoneyWireFormat::DecimalString,
            nv.currency.as_deref(),
        )
        .map_err(|cause| error(DecodeFieldCause::Money(cause)))?
    } else {
        coerce_value_for_field_type(
            &nv.field_type,
            nv.value_format,
            nv.array_items.as_ref(),
            val,
        )
        .map_err(|cause| error(DecodeFieldCause::Coercion(cause)))?
    };
    validate_named_value_domain_value(&coerced, nv)
        .map_err(|cause| error(DecodeFieldCause::Domain(cause)))?;
    Ok(coerced)
}

/// Decode money with sibling currency. Invalid amounts/currency fail the response contract.
pub fn decode_coerce_money_fields(
    fields: &mut IndexMap<String, Value>,
    specs: impl IntoIterator<Item = (String, crate::MoneyDecodeSpec)>,
) -> Result<(), DecodeFieldDiagnostic> {
    for (field, spec) in specs {
        let Some(raw) = fields.get(&field).cloned() else {
            continue;
        };
        if matches!(raw, Value::Null) {
            continue;
        }
        let error = |source: DecodeFieldCause| DecodeFieldDiagnostic {
            field: field.clone(),
            source,
        };
        let mut coerced = crate::money::normalize(
            raw,
            crate::money::MoneyWireFormat::DecimalString,
            spec.default_currency(),
        )
        .map_err(|cause| error(DecodeFieldCause::Money(cause)))?;
        if let Value::Money(ref mut money) = coerced {
            if let Some(currency_field) = spec.currency_field() {
                match fields.get(currency_field) {
                    None | Some(Value::Null) => {}
                    Some(sibling) => {
                        let currency = sibling.as_str().ok_or_else(|| {
                            error(DecodeFieldCause::CurrencyFieldType {
                                field: currency_field.to_owned(),
                                actual: sibling.type_name(),
                            })
                        })?;
                        money.attach_currency_if_absent(Some(currency));
                    }
                }
            }
        }
        fields.insert(field, coerced);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array_field_policy::ArrayFieldCoercionPolicy;
    use crate::EntityDef;
    use crate::EntityFieldName;
    use crate::FieldValueKind;
    use crate::ValueDomainKey;
    use indexmap::IndexMap;

    #[test]
    fn numeric_coercion_preserves_parser_sources() {
        let integer = coerce_value_for_field_type(
            &FieldType::Integer,
            None,
            None,
            Value::String("bad".into()),
        )
        .unwrap_err();
        assert!(std::error::Error::source(&integer)
            .unwrap()
            .downcast_ref::<std::num::ParseIntError>()
            .is_some());
        let number = coerce_value_for_field_type(
            &FieldType::Number,
            None,
            None,
            Value::String("bad".into()),
        )
        .unwrap_err();
        assert!(std::error::Error::source(&number)
            .unwrap()
            .downcast_ref::<std::num::ParseFloatError>()
            .is_some());
    }

    #[test]
    fn opaque_json_preserves_unsigned_boundaries_and_blob_structure() {
        let wire = serde_json::json!({"items": [
            i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX,
            {"bytes": "AAEC", "nested": [null, false, "000123"]}
        ]});
        let value = json_value_to_plasm_value(&wire);
        assert_eq!(try_plasm_value_to_json(&value).unwrap(), wire);
        let encoded = serde_json::to_string(&value).unwrap();
        let decoded: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(try_plasm_value_to_json(&decoded).unwrap(), wire);
        let resolved = crate::operand_binding::ResolvedValue::from_wire(wire.clone()).unwrap();
        assert_eq!(resolved.to_wire().unwrap(), wire);
        let row_value = crate::typed_row::TypedFieldValue::from_value(value.clone());
        assert_eq!(
            try_plasm_value_to_json(&row_value.to_value()).unwrap(),
            wire
        );
        for kind in [FieldType::Json, FieldType::Blob] {
            let coerced = coerce_value_for_field_type(&kind, None, None, value.clone()).unwrap();
            assert_eq!(try_plasm_value_to_json(&coerced).unwrap(), wire);
        }
        let unsigned = json_value_to_plasm_value(&serde_json::json!(u64::MAX));
        assert!(matches!(unsigned, Value::Unsigned(u64::MAX)));
        assert!(coerce_value_for_field_type(&FieldType::Integer, None, None, unsigned).is_err());
    }

    #[test]
    fn integer_param_accepts_integer_and_string_parent() {
        assert!(field_type_assignable_for_relation_binding(
            &FieldType::Integer,
            &FieldType::Integer,
        ));
        assert!(field_type_assignable_for_relation_binding(
            &FieldType::String,
            &FieldType::Integer,
        ));
        assert!(!field_type_assignable_for_relation_binding(
            &FieldType::Boolean,
            &FieldType::Integer,
        ));
    }

    #[test]
    fn entity_ref_param_accepts_parent_id_scalar() {
        let zone = EntityDef {
            name: "Zone".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: IndexMap::new(),
            relations: IndexMap::new(),
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        };
        assert!(relation_binding_assignable(
            &zone,
            "id",
            &FieldType::String,
            &FieldType::EntityRef {
                entry_id: Default::default(),
                target: "Zone".into(),
            },
        ));
        assert!(!relation_binding_assignable(
            &zone,
            "name",
            &FieldType::String,
            &FieldType::EntityRef {
                entry_id: Default::default(),
                target: "Zone".into(),
            },
        ));
    }

    #[test]
    fn entity_ref_param_accepts_repository_full_name_slug() {
        let repo = EntityDef {
            name: "Repository".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: IndexMap::new(),
            relations: IndexMap::new(),
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![
                EntityFieldName::from("owner"),
                EntityFieldName::from("repo"),
            ],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        };
        assert!(relation_binding_assignable(
            &repo,
            "full_name",
            &FieldType::String,
            &FieldType::EntityRef {
                entry_id: Default::default(),
                target: "Repository".into(),
            },
        ));
        assert!(!relation_binding_assignable(
            &repo,
            "description",
            &FieldType::String,
            &FieldType::EntityRef {
                entry_id: Default::default(),
                target: "Repository".into(),
            },
        ));
    }

    #[test]
    fn coerce_json_string_to_integer() {
        let out = coerce_json_value_for_field_type(
            &FieldType::Integer,
            None,
            None,
            serde_json::json!("42"),
        );
        assert_eq!(out, serde_json::json!(42));
    }

    #[test]
    fn teaching_placeholder_passes_through_temporal_date() {
        use crate::TemporalWireFormat;
        let out = coerce_value_for_field_type_with_policy(
            &FieldType::Date,
            Some(ValueWireFormat::Temporal(TemporalWireFormat::Rfc3339)),
            None,
            Value::String("$".into()),
            ArrayFieldCoercionPolicy::InvokeArg,
        )
        .expect("teaching $ must not fail temporal coerce");
        assert!(out.is_domain_example_placeholder());
    }

    #[test]
    fn array_field_rejects_scalar_on_invoke_arg_policy() {
        let err = coerce_value_for_field_type_with_policy(
            &FieldType::Array,
            None,
            Some(&ArrayItemsSchema {
                kind: FieldValueKind::Registry(ValueDomainKey::new("nv_test").expect("key")),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
            }),
            Value::String("a,b".into()),
            ArrayFieldCoercionPolicy::InvokeArg,
        )
        .expect_err("scalar must not auto-wrap for invoke args");
        assert!(matches!(
            err,
            CoercionError::ArrayScalarRejected { actual: "string" }
        ));
    }

    #[test]
    fn boolean_coerces_string_and_phrase_ident_for_query_and_invoke() {
        for policy in [
            ArrayFieldCoercionPolicy::QueryFilter,
            ArrayFieldCoercionPolicy::InvokeArg,
        ] {
            for (raw, expect) in [("true", true), ("false", false), ("TRUE", true)] {
                let from_string = coerce_value_for_field_type_with_policy(
                    &FieldType::Boolean,
                    None,
                    None,
                    Value::String(raw.into()),
                    policy,
                )
                .expect("string bool");
                let from_phrase = coerce_value_for_field_type_with_policy(
                    &FieldType::Boolean,
                    None,
                    None,
                    Value::PhraseIdent(raw.into()),
                    policy,
                )
                .expect("phrase bool");
                assert_eq!(from_string, Value::Bool(expect), "policy={policy:?} string");
                assert_eq!(from_phrase, Value::Bool(expect), "policy={policy:?} phrase");
                assert_eq!(
                    from_string, from_phrase,
                    "read/write coerce must agree for `{raw}`"
                );
            }
        }
    }

    #[test]
    fn integer_coerces_phrase_ident_same_as_string_for_invoke_and_query() {
        for policy in [
            ArrayFieldCoercionPolicy::QueryFilter,
            ArrayFieldCoercionPolicy::InvokeArg,
        ] {
            let from_string = coerce_value_for_field_type_with_policy(
                &FieldType::Integer,
                None,
                None,
                Value::String("42".into()),
                policy,
            )
            .unwrap();
            let from_phrase = coerce_value_for_field_type_with_policy(
                &FieldType::Integer,
                None,
                None,
                Value::PhraseIdent("42".into()),
                policy,
            )
            .unwrap();
            assert_eq!(from_string, Value::Integer(42));
            assert_eq!(from_phrase, from_string);
        }
    }

    #[test]
    fn array_field_passes_plasm_input_ref_for_invoke_args() {
        use crate::PlasmInputRef;
        let out = coerce_value_for_field_type_with_policy(
            &FieldType::Array,
            None,
            Some(&ArrayItemsSchema {
                kind: FieldValueKind::Registry(ValueDomainKey::new("nv_test").expect("key")),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
            }),
            Value::PlasmInputRef(PlasmInputRef::node_output("labels", vec!["name".into()])),
            ArrayFieldCoercionPolicy::InvokeArg,
        )
        .expect("column projection ref must pass through");
        assert!(matches!(out, Value::PlasmInputRef(_)));
    }

    #[test]
    fn array_field_wraps_scalar_for_query_predicate_coercion() {
        let out = coerce_value_for_field_type(
            &FieldType::Array,
            None,
            Some(&ArrayItemsSchema {
                kind: FieldValueKind::Registry(ValueDomainKey::new("nv_test").expect("key")),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
            }),
            Value::String("tag".into()),
        )
        .expect("query filter scalar wrap");
        assert_eq!(
            out,
            Value::Array(vec![Value::String("tag".into())]),
            "predicate coercion may still wrap single scalar"
        );
    }

    #[test]
    fn string_integer_compatible_via_coerce_law() {
        assert!(value_compatible_with_field_type(
            &Value::String("42".into()),
            &FieldType::Integer
        ));
        assert!(!value_compatible_with_field_type(
            &Value::String("nope".into()),
            &FieldType::Integer
        ));
        assert!(!value_compatible_with_field_type(
            &Value::String("1".into()),
            &FieldType::Boolean
        ));
    }

    #[test]
    fn dry_stub_integer_is_numeric_not_string() {
        use crate::schema::NamedValueSchema;
        use crate::value_domain::{Constraints, KernelKind, ValueDomain};

        let nv = NamedValueSchema::from_domain(
            String::new(),
            ValueDomain::new(
                KernelKind::Integer,
                None,
                Constraints::default(),
                None,
                None,
            )
            .expect("integer domain"),
            None,
        );
        let v = dry_stub_value_for_named_value(&nv, 3);
        assert_eq!(v, Value::Integer(3));
        let j = dry_stub_json_for_named_value(&nv, 3);
        assert_eq!(j, serde_json::json!(3));
    }

    #[test]
    fn compare_unify_parses_numeric_strings() {
        let (l, r) =
            compare_unify_json_ordered_numbers(&serde_json::json!("5"), &serde_json::json!(0))
                .expect("string vs number");
        assert!(l > r);
        assert!(compare_unify_json_ordered_numbers(
            &serde_json::json!("nope"),
            &serde_json::json!(0),
        )
        .is_none());
    }

    #[test]
    fn decode_invalid_profile_string_returns_field_error() {
        use crate::schema::NamedValueSchema;
        use crate::value_domain::{Constraints, KernelKind, ProfileId, ValueDomain};

        let nv = NamedValueSchema::from_domain(
            String::new(),
            ValueDomain::new(
                KernelKind::String,
                Some(ProfileId::Email),
                Constraints::default(),
                None,
                None,
            )
            .expect("email domain"),
            None,
        );
        let d =
            decode_coerce_and_validate_field("email", &nv, Value::String("not-an-email".into()))
                .expect_err("invalid present field must fail decoding");
        assert_eq!(d.field, "email");
        assert!(matches!(
            d.source,
            DecodeFieldCause::Domain(crate::ValueDomainViolation::InvalidProfile(
                ProfileId::Email
            ))
        ));
    }
}
