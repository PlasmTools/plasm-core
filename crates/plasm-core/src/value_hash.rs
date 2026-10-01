//! Structural hashing of resolved native values; no transport encoding.
use crate::Value;
use std::hash::Hasher;

/// Feed a canonical, framed native value representation into a digest.
/// Object insertion order is not part of value equality; numeric variants are.
pub fn visit_resolved_value_bytes(value: &Value, write: impl FnMut(&[u8])) -> Result<(), String> {
    visit_value_bytes(value, write, false)
}

/// Lossless native cell digest for compute caches, including output-affecting metadata.
pub fn visit_stored_value_bytes(value: &Value, write: impl FnMut(&[u8])) -> Result<(), String> {
    visit_value_bytes(value, write, true)
}

fn visit_value_bytes(
    value: &Value,
    mut write: impl FnMut(&[u8]),
    storage: bool,
) -> Result<(), String> {
    fn bytes(value: &[u8], write: &mut impl FnMut(&[u8])) {
        write(&(value.len() as u64).to_le_bytes());
        write(value);
    }
    fn visit(
        value: &Value,
        write: &mut impl FnMut(&[u8]),
        depth: usize,
        storage: bool,
    ) -> Result<(), String> {
        if depth >= 64 {
            return Err("resolved value depth exceeded".into());
        }
        match value {
            Value::Null => write(&[0]),
            Value::Bool(v) => write(&[1, u8::from(*v)]),
            Value::Integer(v) => {
                write(&[2]);
                write(&v.to_le_bytes());
            }
            Value::Unsigned(v) => {
                write(&[3]);
                write(&v.to_le_bytes());
            }
            Value::Float(v) if v.is_finite() => {
                write(&[4]);
                write(
                    &(if !storage && *v == 0.0 { 0.0 } else { *v })
                        .to_bits()
                        .to_le_bytes(),
                );
            }
            Value::String(v) => {
                write(&[5]);
                bytes(v.as_bytes(), write);
            }
            Value::Money(v) => {
                write(&[6]);
                write(
                    &(if storage {
                        v.amount()
                    } else {
                        v.amount().normalize()
                    })
                    .serialize(),
                );
                if storage {
                    use crate::money::MoneyWireFormat;
                    match v.stored_format() {
                        None => write(&[0]),
                        Some(MoneyWireFormat::DecimalString) => write(&[1]),
                        Some(MoneyWireFormat::JsonNumber) => write(&[2]),
                        Some(MoneyWireFormat::MinorUnits { scale }) => write(&[3, scale]),
                    }
                }
                match v.currency() {
                    Some(currency) => {
                        write(&[1]);
                        if storage {
                            bytes(currency.as_bytes(), write);
                        } else {
                            bytes(currency.to_ascii_uppercase().as_bytes(), write);
                        }
                    }
                    None => write(&[0]),
                }
            }
            Value::Array(values) => {
                write(&[7]);
                write(&(values.len() as u64).to_le_bytes());
                for value in values {
                    visit(value, write, depth + 1, storage)?;
                }
            }
            Value::Object(fields) => {
                write(&[8]);
                write(&(fields.len() as u64).to_le_bytes());
                let mut fields: Vec<_> = fields.iter().collect();
                if !storage {
                    fields.sort_unstable_by(|a, b| a.0.cmp(b.0));
                }
                for (name, value) in fields {
                    bytes(name.as_bytes(), write);
                    visit(value, write, depth + 1, storage)?;
                }
            }
            _ => return Err("row computation requires finite resolved values".into()),
        }
        Ok(())
    }
    visit(value, &mut write, 0, storage)
}

pub fn hash_resolved_value(value: &Value, hash: &mut impl Hasher) -> Result<(), String> {
    visit_resolved_value_bytes(value, |bytes| hash.write(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        fixture_value,
        money::{MoneyValue, MoneyWireFormat},
    };
    use std::hash::DefaultHasher;
    fn hash(value: &Value) -> u64 {
        let mut h = DefaultHasher::new();
        hash_resolved_value(value, &mut h).unwrap();
        h.finish()
    }
    #[test]
    fn storage_digest_retains_money_decimal_scale() {
        let money = |amount: &str| {
            Value::Money(
                serde_json::from_value::<MoneyValue>(
                    serde_json::json!({"__plasm_money": amount, "currency":"USD"}),
                )
                .unwrap(),
            )
        };
        let a = money("1.0");
        let b = money("1.00");
        assert_eq!(hash(&a), hash(&b));
        let digest = |v| {
            let mut bytes = Vec::new();
            visit_stored_value_bytes(v, |chunk| bytes.extend_from_slice(chunk)).unwrap();
            bytes
        };
        assert_ne!(digest(&a), digest(&b));
    }
    #[test]
    fn semantic_hash_agrees_with_money_and_record_equality() {
        let a = Value::Money(
            MoneyValue::new("1.00".parse().unwrap(), Some("USD".into()))
                .with_format(MoneyWireFormat::DecimalString),
        );
        let b = Value::Money(
            MoneyValue::new("1".parse().unwrap(), Some("usd".into()))
                .with_format(MoneyWireFormat::MinorUnits { scale: 2 }),
        );
        assert_eq!(a, b);
        assert_eq!(hash(&a), hash(&b));
        let stored = |v| {
            let mut bytes = Vec::new();
            visit_stored_value_bytes(v, |chunk| bytes.extend_from_slice(chunk)).unwrap();
            bytes
        };
        assert_ne!(stored(&a), stored(&b));
        let a = Value::Object(indexmap::IndexMap::from([
            ("a".into(), a),
            ("b".into(), Value::Null),
        ]));
        let b = Value::Object(indexmap::IndexMap::from([
            ("b".into(), Value::Null),
            ("a".into(), b),
        ]));
        assert_eq!(a, b);
        assert_eq!(hash(&a), hash(&b));
        assert_ne!(hash(&fixture_value!({})), hash(&fixture_value!({"x":null})));
    }
}
