//! RA-8 / RA-18: one DigitId coerce law.
//!
//! Identity is exact ASCII digits. IEEE float is rejected. Unquoted i64 residual
//! stringifies via `i64` (not `f64`) — RA-8 residual, not a taught literal form.

use crate::value_domain::validate_digit_id;
use crate::Value;

/// JSON number → exact decimal digits (i64/u64 only). IEEE float is not digit_id identity.
pub(crate) fn digit_id_json_to_plasm(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::String(i.to_string())
            } else if let Some(u) = n.as_u64() {
                Value::String(u.to_string())
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        other => super::json_value_to_plasm_value(other),
    }
}

/// Exact digit-string identity. Integer literals stringify via `i64` (not `f64`).
/// Float is rejected so IEEE rounding cannot mint a neighbor id.
pub(crate) fn coerce_digit_id(val: Value) -> Result<Value, String> {
    let s = match val {
        Value::Integer(n) if n >= 0 => n.to_string(),
        Value::String(s) | Value::PhraseIdent(s) => s,
        Value::Float(_) => {
            return Err(
                "cannot coerce float to digit_id (IEEE identity is not exact; use quoted digits)"
                    .into(),
            );
        }
        other => {
            return Err(format!(
                "cannot coerce {} to digit_id (use quoted digits)",
                other.type_name()
            ));
        }
    };
    validate_digit_id(&s)?;
    Ok(Value::String(s))
}

/// IdentityCodec / JSON identity cells: one coerce, then the digit string.
pub(crate) fn encode_digit_id_identity(value: &serde_json::Value) -> Result<String, String> {
    match coerce_digit_id(digit_id_json_to_plasm(value))? {
        Value::String(s) => Ok(s),
        other => Err(format!(
            "digit_id identity did not coerce to a digit string, got {}",
            other.type_name()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire_coercion::{
        coerce_json_value_for_field_type, coerce_value_for_field_type, json_to_plasm_for_field,
    };
    use crate::FieldType;

    #[test]
    fn coerce_digit_id_accepts_i64_and_quoted_digits_rejects_float() {
        let pan = coerce_value_for_field_type(
            &FieldType::DigitId,
            None,
            None,
            Value::Integer(6_419_671_322_388_907),
        )
        .expect("i64 → exact digits");
        assert_eq!(pan, Value::String("6419671322388907".into()));
        let quoted = coerce_value_for_field_type(
            &FieldType::DigitId,
            None,
            None,
            Value::String("6419671322388907".into()),
        )
        .expect("quoted digits");
        assert_eq!(quoted, Value::String("6419671322388907".into()));
        let wide = coerce_value_for_field_type(
            &FieldType::DigitId,
            None,
            None,
            Value::Integer(9_007_199_254_740_993),
        )
        .expect("beyond-f64 i64 stays exact");
        assert_eq!(wide, Value::String("9007199254740993".into()));
        let err = coerce_value_for_field_type(
            &FieldType::DigitId,
            None,
            None,
            Value::Float(9_007_199_254_740_993i64 as f64),
        )
        .expect_err("IEEE float is not digit_id");
        assert!(
            err.contains("float") || err.contains("IEEE") || err.contains("digit_id"),
            "{err}"
        );
        assert!(
            coerce_value_for_field_type(&FieldType::DigitId, None, None, Value::Integer(-1),)
                .is_err()
        );
        assert!(coerce_value_for_field_type(
            &FieldType::DigitId,
            None,
            None,
            Value::String("64-19".into()),
        )
        .is_err());
    }

    #[test]
    fn digit_id_json_number_is_exact_i64_digits_not_float() {
        let from_i64 = json_to_plasm_for_field(
            &FieldType::DigitId,
            &serde_json::json!(6_419_671_322_388_907i64),
        );
        assert_eq!(from_i64, Value::String("6419671322388907".into()));
        let from_text =
            json_to_plasm_for_field(&FieldType::DigitId, &serde_json::json!("6419671322388907"));
        assert_eq!(from_text, Value::String("6419671322388907".into()));
        let ieee = json_to_plasm_for_field(
            &FieldType::DigitId,
            &serde_json::Value::Number(
                serde_json::Number::from_f64(9_007_199_254_740_993i64 as f64).expect("finite f64"),
            ),
        );
        assert!(
            matches!(ieee, Value::Float(_)),
            "IEEE JSON number must not mint digit_id identity"
        );
        assert!(coerce_digit_id(ieee).is_err());
        assert_eq!(
            coerce_json_value_for_field_type(
                &FieldType::DigitId,
                None,
                None,
                serde_json::json!(6_419_671_322_388_907i64),
            ),
            serde_json::json!("6419671322388907")
        );
        assert_eq!(
            coerce_json_value_for_field_type(
                &FieldType::DigitId,
                None,
                None,
                serde_json::json!(u64::MAX),
            ),
            serde_json::json!(u64::MAX.to_string())
        );
        let ieee_json = coerce_json_value_for_field_type(
            &FieldType::DigitId,
            None,
            None,
            serde_json::Value::Number(
                serde_json::Number::from_f64(9_007_199_254_740_993i64 as f64).expect("finite f64"),
            ),
        );
        assert!(
            ieee_json.is_null(),
            "IEEE JSON number must not survive coerce_json as digit_id identity, got {ieee_json}"
        );
    }

    #[test]
    fn encode_digit_id_identity_is_the_same_coerce() {
        assert_eq!(
            encode_digit_id_identity(&serde_json::json!("6419671322388907")).expect("quoted"),
            "6419671322388907"
        );
        assert_eq!(
            encode_digit_id_identity(&serde_json::json!(6_419_671_322_388_907i64)).expect("i64"),
            "6419671322388907"
        );
        let err = encode_digit_id_identity(&serde_json::json!(9_007_199_254_740_993i64 as f64))
            .expect_err("IEEE");
        assert!(err.contains("IEEE"), "{err}");
        assert!(encode_digit_id_identity(&serde_json::json!("64-19")).is_err());
        assert!(encode_digit_id_identity(&serde_json::json!(-1)).is_err());
    }
}
