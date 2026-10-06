//! Contract-directed semantic keys. Payloads remain borrowed; only recursive key
//! structure and normalized temporal coordinates are materialized.
use crate::{
    temporal_value::TemporalKind,
    value_contract::{ValueContract, ValueShape},
    FieldType, Value,
};
use std::hash::Hasher;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValueEqualityError {
    #[error("equality contract nesting exceeds the maximum depth")]
    ContractDepthExceeded,
    #[error("equality value nesting exceeds the maximum depth")]
    ValueDepthExceeded,
    #[error("invalid container representation: {0}")]
    InvalidContainer(&'static str),
    #[error("required record fields are not observed")]
    RequiredFieldUnobserved,
    #[error("record contains an undeclared field")]
    UndeclaredField,
    #[error("value inhabits no equality union variant")]
    NoUnionVariant,
    #[error("value has an ambiguous semantic union key")]
    AmbiguousUnion,
    #[error("value does not inhabit the declared equality domain")]
    InvalidRepresentation,
    #[error("equality domain is uninhabited")]
    UninhabitedDomain,
    #[error(transparent)]
    Ordering(#[from] crate::value_order::OrderingError),
    #[error(transparent)]
    Temporal(#[from] crate::value_order::TemporalValueError),
    #[error(transparent)]
    Hash(#[from] crate::value_hash::ValueHashError),
}

pub trait Equatable {
    fn equality(&self) -> Result<ValueEquality<'_>, ValueEqualityError>;
}
#[derive(Clone, Copy)]
pub struct ValueEquality<'a>(&'a ValueContract);

#[derive(Debug, PartialEq)]
pub struct ValueKey<'a>(KeyKind<'a>);

#[derive(Debug, PartialEq)]
enum KeyKind<'a> {
    Native(&'a Value),
    Temporal(TemporalKind, bool, i128),
    Array(Vec<ValueKey<'a>>),
    Set(UnorderedKeys<'a>),
    Record(Vec<(&'a str, ValueKey<'a>)>),
}
#[derive(Debug)]
struct UnorderedKeys<'a>(Vec<ValueKey<'a>>);
impl PartialEq for UnorderedKeys<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len() && self.0.iter().all(|key| other.0.contains(key))
    }
}
impl Equatable for ValueContract {
    fn equality(&self) -> Result<ValueEquality<'_>, ValueEqualityError> {
        fn check(c: &ValueContract, depth: usize) -> Result<(), ValueEqualityError> {
            if depth >= 64 {
                return Err(ValueEqualityError::ContractDepthExceeded);
            }
            match &c.shape {
                ValueShape::Array { element } | ValueShape::Set { element } => {
                    check(element, depth + 1)?
                }
                ValueShape::MappingRecord { record } => check(record, depth + 1)?,
                ValueShape::Dictionary { key, value } => {
                    check(key, depth + 1)?;
                    check(value, depth + 1)?;
                }
                ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } => {
                    for c in fields.values() {
                        check(c, depth + 1)?;
                    }
                }
                ValueShape::Union { variants } => {
                    for c in variants {
                        check(c, depth + 1)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        check(self, 0)?;
        Ok(ValueEquality(self))
    }
}
impl ValueEquality<'_> {
    pub fn key<'a>(&self, value: &'a Value) -> Result<ValueKey<'a>, ValueEqualityError> {
        key(self.0, value, 0)
    }
    pub fn equivalent(&self, left: &Value, right: &Value) -> Result<bool, ValueEqualityError> {
        Ok(self.key(left)? == self.key(right)?)
    }
}
fn key<'a>(
    c: &ValueContract,
    v: &'a Value,
    depth: usize,
) -> Result<ValueKey<'a>, ValueEqualityError> {
    if depth >= 64 {
        return Err(ValueEqualityError::ValueDepthExceeded);
    }
    // Null placement/presence is an operator concern; null is one distinct key.
    if v.is_null() {
        return Ok(ValueKey(KeyKind::Native(v)));
    }
    Ok(ValueKey(match &c.shape {
        ValueShape::MappingRecord { record } => return key(record, v, depth + 1),
        ValueShape::Set { element } => KeyKind::Set(UnorderedKeys(
            v.as_array()
                .ok_or(ValueEqualityError::InvalidContainer("set"))?
                .iter()
                .map(|v| key(element, v, depth + 1))
                .collect::<Result<_, _>>()?,
        )),
        ValueShape::Temporal { kind, .. } => {
            let (aware, coordinate) = crate::value_order::temporal_key(c, v, *kind)?;
            KeyKind::Temporal(*kind, aware, coordinate)
        }
        ValueShape::Array { element } => KeyKind::Array(
            v.as_array()
                .ok_or(ValueEqualityError::InvalidContainer("array"))?
                .iter()
                .map(|v| key(element, v, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        ValueShape::Dictionary { value: element, .. } => {
            let mut keys = v
                .as_object()
                .ok_or(ValueEqualityError::InvalidContainer("dictionary"))?
                .iter()
                .map(|(name, value)| {
                    key(element, value, depth + 1).map(|value| (name.as_str(), value))
                })
                .collect::<Result<Vec<_>, _>>()?;
            keys.sort_by_key(|(name, _)| *name);
            KeyKind::Record(keys)
        }
        ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } => {
            let values = v
                .as_object()
                .ok_or(ValueEqualityError::InvalidContainer("record"))?;
            let optional = match &c.shape {
                ValueShape::ObservedRecord {
                    optional_fields, ..
                } => Some(optional_fields),
                _ => None,
            };
            if fields.keys().any(|name| {
                !values.contains_key(name) && !optional.is_some_and(|fields| fields.contains(name))
            }) {
                return Err(ValueEqualityError::RequiredFieldUnobserved);
            }
            let mut keys = values
                .iter()
                .map(|(name, v)| {
                    let c = fields
                        .get(name)
                        .ok_or(ValueEqualityError::UndeclaredField)?;
                    Ok((name.as_str(), key(c, v, depth + 1)?))
                })
                .collect::<Result<Vec<_>, ValueEqualityError>>()?;
            keys.sort_by_key(|(name, _)| *name);
            KeyKind::Record(keys)
        }
        ValueShape::Union { variants } => {
            let mut candidates = variants.iter().filter_map(|c| key(c, v, depth + 1).ok());
            let first = candidates
                .next()
                .ok_or(ValueEqualityError::NoUnionVariant)?;
            if candidates.any(|k| k != first) {
                return Err(ValueEqualityError::AmbiguousUnion);
            }
            first.0
        }
        ValueShape::Scalar { field_type } => {
            let valid = match field_type {
                FieldType::Integer => matches!(v, Value::Integer(_) | Value::Unsigned(_)),
                FieldType::Number => v.is_number(),
                FieldType::Boolean => matches!(v, Value::Bool(_)),
                FieldType::Money => matches!(v, Value::Money(_)),
                FieldType::String | FieldType::Uuid | FieldType::DigitId | FieldType::Select => {
                    v.as_str().is_some()
                }
                FieldType::Array | FieldType::MultiSelect => v.as_array().is_some(),
                FieldType::Date => false,
                FieldType::Json | FieldType::Blob | FieldType::EntityRef { .. } => true,
            };
            if !valid {
                return Err(ValueEqualityError::InvalidRepresentation);
            }
            // Validate finite/resolved leaves before establishing reflexive equality.
            crate::hash_resolved_value(v, &mut std::hash::DefaultHasher::new())?;
            KeyKind::Native(v)
        }
        ValueShape::Null | ValueShape::Never => return Err(ValueEqualityError::UninhabitedDomain),
    }))
}
impl ValueKey<'_> {
    pub fn hash_into(&self, h: &mut impl Hasher) -> Result<(), ValueEqualityError> {
        match &self.0 {
            KeyKind::Native(v) => {
                h.write_u8(0);
                crate::hash_resolved_value(v, h)?;
            }
            KeyKind::Temporal(kind, aware, coordinate) => {
                h.write_u8(1);
                h.write_u8(*kind as u8);
                h.write_u8(u8::from(*aware));
                h.write(&coordinate.to_le_bytes());
            }
            KeyKind::Array(items) => {
                h.write_u8(2);
                h.write_usize(items.len());
                for item in items {
                    item.hash_into(h)?;
                }
            }
            KeyKind::Set(items) => {
                h.write_u8(4);
                let mut hashes = items
                    .0
                    .iter()
                    .map(|item| {
                        let mut hash = std::collections::hash_map::DefaultHasher::new();
                        item.hash_into(&mut hash)?;
                        Ok(hash.finish())
                    })
                    .collect::<Result<Vec<_>, ValueEqualityError>>()?;
                hashes.sort_unstable();
                h.write_usize(hashes.len());
                for hash in hashes {
                    h.write_u64(hash);
                }
            }
            KeyKind::Record(fields) => {
                h.write_u8(3);
                h.write_usize(fields.len());
                for (name, v) in fields {
                    h.write_usize(name.len());
                    h.write(name.as_bytes());
                    v.hash_into(h)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::hash::{DefaultHasher, Hasher};
    fn digest(k: &ValueKey<'_>) -> u64 {
        let mut h = DefaultHasher::new();
        k.hash_into(&mut h).unwrap();
        h.finish()
    }
    proptest! {
        #[test]
        fn offset_equivalence_is_transitive_and_hash_congruent(seconds in 0i64..2_000_000_000, a in -1439i32..1439, b in -1439i32..1439) {
            let t = TemporalKind::Datetime.contract();
            let eq = t.equality().unwrap();
            let dt = chrono::DateTime::from_timestamp(seconds, 0).unwrap();
            let wire = |offset| Value::String(dt.with_timezone(&chrono::FixedOffset::east_opt(offset * 60).unwrap()).to_rfc3339());
            let (x,y,z) = (wire(0),wire(a),wire(b));
            let (kx,ky,kz) = (eq.key(&x).unwrap(),eq.key(&y).unwrap(),eq.key(&z).unwrap());
            prop_assert_eq!(&kx,&ky); prop_assert_eq!(&ky,&kz);
            prop_assert_eq!(digest(&kx),digest(&kz));
        }
    }
    #[test]
    fn nested_semantics_preserve_presence_and_payload() {
        let t = ValueContract::record(
            [("when".into(), TemporalKind::Datetime.contract())].into(),
            ["when".into()].into(),
        );
        let eq = t.equality().unwrap();
        let a = crate::fixture_value!({"when":"2024-01-01T01:00:00+01:00"});
        let b = crate::fixture_value!({"when":"2024-01-01T00:00:00Z"});
        assert!(eq.equivalent(&a, &b).unwrap());
        assert_eq!(digest(&eq.key(&a).unwrap()), digest(&eq.key(&b).unwrap()));
        assert!(!eq
            .equivalent(
                &crate::fixture_value!({}),
                &crate::fixture_value!({"when":null})
            )
            .unwrap());
        assert_eq!(a["when"].as_str(), Some("2024-01-01T01:00:00+01:00"));
        let array = ValueContract {
            shape: ValueShape::Array {
                element: Box::new(t),
            },
            domain: None,
            nullable: false,
        };
        let (a, b) = (Value::Array(vec![a]), Value::Array(vec![b]));
        let equality = array.equality().unwrap();
        assert!(equality.equivalent(&a, &b).unwrap());
        assert_eq!(
            digest(&equality.key(&a).unwrap()),
            digest(&equality.key(&b).unwrap())
        );
    }
    #[test]
    fn ambiguous_temporal_union_never_chooses_first_wire() {
        let t = |wire| ValueContract {
            shape: ValueShape::Temporal {
                kind: TemporalKind::Datetime,
                wire: Some(wire),
            },
            domain: None,
            nullable: false,
        };
        let a = t(crate::TemporalWireFormat::UnixSec);
        let b = t(crate::TemporalWireFormat::UnixMs);
        for union in [
            ValueContract::join(a.clone(), b.clone()),
            ValueContract::join(b, a),
        ] {
            assert!(union
                .equality()
                .unwrap()
                .key(&Value::Integer(1000))
                .is_err());
            assert!(crate::value_order::Orderable::ordering(&union)
                .unwrap()
                .validate(&Value::Integer(1000))
                .is_err());
            assert!(union.equality().unwrap().key(&Value::Integer(0)).is_ok());
        }
    }
    #[test]
    fn timezone_names_are_not_offset_identity_and_invalid_values_fail() {
        let t = TemporalKind::Timezone.contract();
        let a = crate::temporal_value::tagged(
            TemporalKind::Timezone,
            crate::fixture_value!({"offset_seconds":3600,"name":"A"}),
        );
        let b = crate::temporal_value::tagged(
            TemporalKind::Timezone,
            crate::fixture_value!({"offset_seconds":3600,"name":"B"}),
        );
        assert!(t.equality().unwrap().equivalent(&a, &b).unwrap());
        assert!(ValueContract::scalar(FieldType::Number)
            .equality()
            .unwrap()
            .key(&Value::Float(f64::NAN))
            .is_err());
    }
}

#[cfg(test)]
mod awareness_tests {
    use super::*;
    use crate::value_order::Orderable;
    #[test]
    fn naive_and_aware_are_distinct_and_not_orderable_together() {
        let aware = TemporalKind::Datetime.contract();
        let naive = ValueContract {
            shape: ValueShape::Temporal {
                kind: TemporalKind::Datetime,
                wire: Some(crate::TemporalWireFormat::Iso8601NaiveDatetime),
            },
            domain: None,
            nullable: false,
        };
        let c = ValueContract::join(aware, naive);
        let a = Value::String("2024-01-01T00:00:00Z".into());
        let b = Value::String("2024-01-01T00:00:00".into());
        assert!(!c.equality().unwrap().equivalent(&a, &b).unwrap());
        assert!(c.ordering().unwrap().compare(&a, &b).is_err());
        assert!(!c.ordering().unwrap().equal_literal(&a, &b).unwrap());
    }
}

#[cfg(test)]
mod union_presence_tests {
    use super::*;
    #[test]
    fn union_key_resolution_requires_each_variants_required_fields() {
        let temporal = ValueContract::record(
            [
                ("when".into(), TemporalKind::Datetime.contract()),
                ("required".into(), ValueContract::scalar(FieldType::Integer)),
            ]
            .into(),
            Default::default(),
        );
        let text = ValueContract::record(
            [("when".into(), ValueContract::scalar(FieldType::String))].into(),
            Default::default(),
        );
        let input = crate::fixture_value!({"when":"2024-01-01T00:00:00Z"});
        assert!(temporal.equality().unwrap().key(&input).is_err());
        let union = ValueContract::join(temporal, text.clone());
        assert_eq!(
            union.equality().unwrap().key(&input).unwrap(),
            text.equality().unwrap().key(&input).unwrap()
        );
    }
    #[test]
    fn sets_preserve_order_independent_equality_and_hashes() {
        use std::hash::Hasher;
        let contract = ValueContract {
            shape: ValueShape::Set {
                element: Box::new(ValueContract::scalar(FieldType::String)),
            },
            domain: None,
            nullable: false,
        };
        let left = crate::fixture_value!(["a", "b"]);
        let right = crate::fixture_value!(["b", "a"]);
        let eq = contract.equality().unwrap();
        assert!(contract
            .validate(&left, &crate::CGS::default(), "fixture", "set")
            .is_ok());
        assert!(contract
            .validate(
                &crate::fixture_value!(["a", "a"]),
                &crate::CGS::default(),
                "fixture",
                "set"
            )
            .is_err());
        assert!(eq.equivalent(&left, &right).unwrap());
        let mut hashes = Vec::new();
        for value in [&left, &right] {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            eq.key(value).unwrap().hash_into(&mut hash).unwrap();
            hashes.push(hash.finish());
        }
        assert_eq!(hashes[0], hashes[1]);
    }
}
