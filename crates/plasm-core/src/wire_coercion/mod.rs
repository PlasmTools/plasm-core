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

use crate::array_field_policy::{invoke_array_scalar_error, ArrayFieldCoercionPolicy};
use crate::capability_input::validate_named_value_domain_value;
use crate::{ArrayItemsSchema, FieldType, NamedValueSchema, Value, ValueWireFormat};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

mod digit_id;
mod dry_stub;
mod relation_binding;

pub(crate) use digit_id::{coerce_digit_id, digit_id_json_to_plasm, encode_digit_id_identity};
pub use dry_stub::{
    dry_stub_entity_row_json, dry_stub_json_for_named_value, dry_stub_value_for_named_value,
};
pub use relation_binding::{
    apply_identity_slots_to_row, binding_value_as_plasm_value, collect_relation_binding_proofs,
    field_type_assignable_for_relation_binding, identity_slot_to_json, parent_entity_field_type,
    relation_binding_assignable, restore_id_field_from_compound_ref, RelationBindingProof,
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
) -> Result<Value, String> {
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
) -> Result<Value, String> {
    // Teaching-table bare `$` is a fill-in slot, not a wire token — pass through for every field
    // type (including temporal `Date`) so optional params can appear as `p#=$` in method rows.
    if val.is_domain_example_placeholder() {
        return Ok(val);
    }
    if matches!(val, Value::StringTemplate(_)) {
        return if matches!(ft, FieldType::String | FieldType::Blob) {
            Ok(val)
        } else {
            Err(format!("string template cannot bind a {ft:?} operand"))
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
            let coerce_elem = |v: Value| -> Result<Value, String> {
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
                other => Err(invoke_array_scalar_error(other.type_name())),
            }
        }
        FieldType::Date => {
            // Relative tokens (`now`, `now-1h`) often arrive as PhraseIdent in program mode.
            let val = phrase_ident_to_string(val);
            match value_format {
                Some(ValueWireFormat::Temporal(fmt)) => {
                    crate::temporal::normalize_temporal_value(val, fmt)
                }
                None => match val {
                    Value::String(_) | Value::Integer(_) | Value::Float(_) => Ok(val),
                    other => Err(format!(
                        "cannot coerce {} to date (missing value_format)",
                        other.type_name()
                    )),
                },
                Some(ValueWireFormat::Money(_)) => {
                    Err("Date field missing value_format in schema".to_string())
                }
            }
        }
        FieldType::DigitId => coerce_digit_id(val),
        FieldType::String | FieldType::Uuid | FieldType::Select => Ok(match val {
            Value::Integer(n) => Value::String(n.to_string()),
            Value::Float(f) => Value::String(normalize_numeric_id_float(f)),
            Value::PhraseIdent(s) => Value::String(s),
            Value::String(s) => Value::String(s),
            other => return Err(format!("cannot coerce {} to {ft:?}", other.type_name())),
        }),
        FieldType::Blob => {
            if val.is_plasm_attachment_object() {
                return Ok(val);
            }
            Ok(match val {
                Value::Integer(n) => Value::String(n.to_string()),
                Value::Float(f) => Value::String(normalize_numeric_id_float(f)),
                Value::PhraseIdent(s) => Value::String(s),
                Value::String(s) => Value::String(s),
                other => return Err(format!("cannot coerce {} to blob", other.type_name())),
            })
        }
        FieldType::MultiSelect => match val {
            Value::Array(_) => Ok(val),
            Value::PhraseIdent(s) => Ok(Value::String(s)),
            Value::String(s) => Ok(Value::String(s)),
            other => Err(format!(
                "cannot coerce {} to multi_select",
                other.type_name()
            )),
        },
        FieldType::Integer => {
            if let Some(s) = stringish(&val) {
                return s
                    .parse::<i64>()
                    .map(Value::Integer)
                    .map_err(|_| format!("cannot coerce {s:?} to integer"));
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
                other => Err(format!("cannot coerce {} to integer", other.type_name())),
            }
        }
        FieldType::Number => {
            if let Some(s) = stringish(&val) {
                return s
                    .parse::<f64>()
                    .map(Value::Float)
                    .map_err(|_| format!("cannot coerce {s:?} to number"));
            }
            match val {
                Value::Integer(n) => Ok(Value::Float(n as f64)),
                Value::Float(f) => Ok(Value::Float(f)),
                other => Err(format!("cannot coerce {} to number", other.type_name())),
            }
        }
        FieldType::EntityRef { .. } => Ok(match val {
            Value::Integer(n) => Value::String(n.to_string()),
            Value::Float(f) => Value::String(normalize_numeric_id_float(f)),
            Value::PhraseIdent(s) => Value::String(s),
            Value::String(s) => Value::String(s),
            Value::Object(o) => Value::Object(o),
            other => return Err(format!("cannot coerce {} to entity_ref", other.type_name())),
        }),
        FieldType::Boolean => match stringish(&val) {
            // RA-8: reject `"1"` / `"0"` — only true/false tokens.
            Some(s) if s.eq_ignore_ascii_case("true") => Ok(Value::Bool(true)),
            Some(s) if s.eq_ignore_ascii_case("false") => Ok(Value::Bool(false)),
            Some(s) => Err(format!(
                "cannot coerce {s:?} to boolean (expected true/false)"
            )),
            None => match val {
                Value::Bool(b) => Ok(Value::Bool(b)),
                other => Err(format!("cannot coerce {} to boolean", other.type_name())),
            },
        },
        FieldType::Json => match val {
            Value::String(ref s) if s.as_str() == "$" => Ok(val),
            Value::String(s) => crate::value::parse_json_subtree_str(&s).ok_or_else(|| {
                "Json parameter: string must be valid JSON with a top-level object or array"
                    .to_string()
            }),
            Value::PhraseIdent(s) => crate::value::parse_json_subtree_str(&s).ok_or_else(|| {
                "Json parameter: string must be valid JSON with a top-level object or array"
                    .to_string()
            }),
            Value::Object(_) | Value::Array(_) => Ok(val),
            other => Err(format!("cannot coerce {} to json", other.type_name())),
        },
        FieldType::Money => {
            // Doctrine: money wire is decimal string only.
            let fmt = crate::money::MoneyWireFormat::DecimalString;
            let val = phrase_ident_to_string(val);
            crate::money::normalize(val, fmt, None).map_err(String::from)
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

pub fn try_plasm_value_to_json(v: &Value) -> Result<serde_json::Value, String> {
    if let Some(s) = v.as_string_or_phrase() {
        return Ok(serde_json::Value::String(s.to_string()));
    }
    match v {
        Value::StringTemplate(_) => Err("unbound string template reached wire encoding".into()),
        Value::Null => Ok(serde_json::Value::Null),
        Value::Bool(b) => Ok(serde_json::Value::Bool(*b)),
        Value::Integer(i) => Ok(serde_json::json!(i)),
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
                .collect::<Result<_, String>>()?,
        )),
        Value::PlasmInputRef(_)
        | Value::GetScalarExtract(_)
        | Value::UnionCtor { .. }
        | Value::String(_)
        | Value::PhraseIdent(_) => Ok(serde_json::Value::Null),
        Value::Money(m) => m.encode_stored().map_err(String::from),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodeFieldDiagnostic {
    pub field: String,
    pub message: String,
}

/// Shape-coerce a decoded wire scalar, then validate profile/constraints.
///
/// On failure the returned value is [`Value::Null`] and a diagnostic is emitted; the row decode continues.
pub fn decode_coerce_and_validate_field(
    field_name: &str,
    nv: &NamedValueSchema,
    val: Value,
) -> (Value, Option<DecodeFieldDiagnostic>) {
    let fail = |message: String| {
        (
            Value::Null,
            Some(DecodeFieldDiagnostic {
                field: field_name.to_string(),
                message,
            }),
        )
    };

    if matches!(nv.field_type, FieldType::Money) {
        let val = phrase_ident_to_string(val);
        match crate::money::normalize(
            val,
            crate::money::MoneyWireFormat::DecimalString,
            nv.currency.as_deref(),
        ) {
            Ok(coerced) => match validate_named_value_domain_value(&coerced, nv) {
                Ok(()) => (coerced, None),
                Err(msg) => fail(msg),
            },
            Err(e) => fail(e.to_string()),
        }
    } else {
        match coerce_value_for_field_type(
            &nv.field_type,
            nv.value_format,
            nv.array_items.as_ref(),
            val,
        ) {
            Ok(coerced) => {
                // Enum membership / profile / constraints are all owned by `domain`.
                if let Err(msg) = validate_named_value_domain_value(&coerced, nv) {
                    return fail(msg);
                }
                (coerced, None)
            }
            Err(msg) => fail(msg),
        }
    }
}

/// Attach sibling currency to decoded money fields; soft-fail normalize and currency slot shape errors.
pub fn decode_coerce_money_fields(
    fields: &mut IndexMap<String, Value>,
    specs: impl IntoIterator<Item = (String, crate::MoneyDecodeSpec)>,
    diagnostics: &mut Vec<DecodeFieldDiagnostic>,
) {
    for (field, spec) in specs {
        let Some(raw) = fields.get(&field).cloned() else {
            continue;
        };
        if matches!(raw, Value::Null) {
            continue;
        }
        let fmt = crate::money::MoneyWireFormat::DecimalString;
        match crate::money::normalize(raw, fmt, spec.default_currency()) {
            Ok(mut coerced) => {
                if let Value::Money(ref mut m) = coerced {
                    if let Some(cf) = spec.currency_field() {
                        match fields.get(cf) {
                            None | Some(Value::Null) => {}
                            Some(sibling) => {
                                let Some(s) = sibling.as_str() else {
                                    diagnostics.push(DecodeFieldDiagnostic {
                                        field: field.clone(),
                                        message: format!(
                                            "money currency field `{cf}` must be a string (got {})",
                                            sibling.type_name()
                                        ),
                                    });
                                    fields.insert(field, Value::Null);
                                    continue;
                                };
                                m.attach_currency_if_absent(Some(s));
                            }
                        }
                    }
                }
                fields.insert(field, coerced);
            }
            Err(e) => {
                diagnostics.push(DecodeFieldDiagnostic {
                    field: field.clone(),
                    message: e.to_string(),
                });
                fields.insert(field, Value::Null);
            }
        }
    }
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
        assert!(err.contains("expected array"), "unexpected error: {err}");
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
    fn decode_invalid_profile_string_becomes_null_with_diagnostic() {
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
        let (value, diag) =
            decode_coerce_and_validate_field("email", &nv, Value::String("not-an-email".into()));
        assert!(matches!(value, Value::Null));
        let d = diag.expect("diagnostic");
        assert_eq!(d.field, "email");
        assert!(d.message.contains("email") || d.message.contains("invalid"));
    }
}
