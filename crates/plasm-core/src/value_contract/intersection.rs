//! Symmetric conjunction of already established type constraints.
//!
//! Unlike `refined_by`, neither operand is privileged as the original value.
//! This combines contracts; it does not validate a value or grant domain authority.
use super::{ValueContract as T, ValueShape as S};
use crate::FieldType;

impl T {
    /// Intersect established constraints, retaining metadata recursively. Conflicting
    /// nominal identities or wire encodings are explicit representation errors.
    /// Use `refined_by` when only the original operand supplies authority.
    pub fn intersect_constraints(&self, other: &Self) -> Result<Self, String> {
        intersect(self, other, 0)
    }
}

fn intersect(a: &T, b: &T, depth: usize) -> Result<T, String> {
    if depth >= 64 {
        return Err("intersection depth exceeds 64".into());
    }
    let nullable = (a.nullable || a.shape == S::Null) && (b.nullable || b.shape == S::Null);
    let bottom = || T {
        shape: if nullable { S::Null } else { S::Never },
        domain: None,
        nullable,
    };
    if matches!(a.shape, S::Null | S::Never) || matches!(b.shape, S::Null | S::Never) {
        return Ok(bottom());
    }
    let domain = match (&a.domain, &b.domain) {
        (Some(a), Some(b)) if a != b => {
            return Err("intersection contains distinct value domains".into())
        }
        (Some(d), _) | (_, Some(d)) => Some(d.clone()),
        _ => None,
    };
    let recur = |a: &T, b: &T| intersect(a, b, depth + 1);
    let shape = match (&a.shape, &b.shape) {
        (S::Union { variants }, _) | (_, S::Union { variants }) => {
            let other = if matches!(a.shape, S::Union { .. }) {
                b
            } else {
                a
            };
            let mut result = bottom();
            for variant in variants {
                result = T::join(result, recur(variant, other)?);
            }
            result.nullable = nullable;
            if domain.is_some() && !matches!(result.shape, S::Never | S::Null) {
                result.domain = domain;
            }
            return Ok(result);
        }
        (S::Scalar { field_type: a }, S::Scalar { field_type: b }) if a == b => S::Scalar {
            field_type: a.clone(),
        },
        (
            S::Scalar {
                field_type: FieldType::Integer,
            },
            S::Scalar {
                field_type: FieldType::Number,
            },
        )
        | (
            S::Scalar {
                field_type: FieldType::Number,
            },
            S::Scalar {
                field_type: FieldType::Integer,
            },
        ) => S::Scalar {
            field_type: FieldType::Integer,
        },
        // These semantic scalars retain their representation when constrained by
        // Python's string carrier. No new domain is inferred from that carrier.
        (
            S::Scalar {
                field_type: FieldType::String,
            },
            S::Scalar { field_type: value },
        )
        | (
            S::Scalar { field_type: value },
            S::Scalar {
                field_type: FieldType::String,
            },
        ) if matches!(
            value,
            FieldType::Uuid | FieldType::DigitId | FieldType::Select
        ) =>
        {
            S::Scalar {
                field_type: value.clone(),
            }
        }
        (S::Scalar { field_type: a }, S::Scalar { field_type: b })
            if matches!(a, FieldType::Uuid | FieldType::DigitId | FieldType::Select)
                && matches!(b, FieldType::Uuid | FieldType::DigitId | FieldType::Select) =>
        {
            return Err("intersection contains distinct semantic string carriers".into());
        }
        (S::Temporal { kind: a, wire: aw }, S::Temporal { kind: b, wire: bw }) if a == b => {
            if aw.is_some() && bw.is_some() && aw != bw {
                return Err("intersection contains distinct temporal wire encodings".into());
            }
            S::Temporal {
                kind: *a,
                wire: aw.or(*bw),
            }
        }
        (S::Array { element: a }, S::Array { element: b }) => S::Array {
            element: Box::new(recur(a, b)?),
        },
        (
            S::Record { fields: af } | S::ObservedRecord { fields: af, .. },
            S::Record { fields: bf } | S::ObservedRecord { fields: bf, .. },
        ) => {
            let optional = |shape: &S, name: &str| {
                matches!(shape,
                S::ObservedRecord { optional_fields, .. } if optional_fields.contains(name))
            };
            // Both materialized schemas are closed. A field absent from either
            // schema must be absent in the intersection; a required such field
            // makes the non-null intersection empty.
            if af
                .keys()
                .any(|name| !bf.contains_key(name) && !optional(&a.shape, name))
                || bf
                    .keys()
                    .any(|name| !af.contains_key(name) && !optional(&b.shape, name))
            {
                return Ok(bottom());
            }
            let mut fields = std::collections::BTreeMap::new();
            let mut optional_fields = std::collections::BTreeSet::new();
            for (name, av) in af {
                let Some(bv) = bf.get(name) else { continue };
                let value = recur(av, bv)?;
                let is_optional = optional(&a.shape, name) && optional(&b.shape, name);
                if value.shape == S::Never && !value.nullable && !is_optional {
                    return Ok(bottom());
                }
                if is_optional {
                    optional_fields.insert(name.clone());
                }
                fields.insert(name.clone(), value);
            }
            T::record(fields, optional_fields).shape
        }
        // Opaque/specialized carriers can overlap structured values. A shape
        // mismatch alone is not evidence of disjointness for these contracts.
        (a, b)
            if [a, b].iter().any(|shape| {
                matches!(
                    shape,
                    S::Scalar {
                        field_type: FieldType::Json
                            | FieldType::Blob
                            | FieldType::Money
                            | FieldType::EntityRef { .. }
                            | FieldType::MultiSelect
                            | FieldType::Date
                            | FieldType::Array
                    }
                )
            }) =>
        {
            return Err(
                "intersection of specialized carriers requires a representable common contract"
                    .into(),
            );
        }
        _ => return Ok(bottom()),
    };
    Ok(T {
        shape,
        domain,
        nullable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{value_contract::DomainRef, ValueDomainKey};

    fn named() -> T {
        T {
            domain: Some(DomainRef {
                entry_id: "matrix".into(),
                catalog_hash: "pin".into(),
                value_ref: ValueDomainKey::new("id").unwrap(),
            }),
            ..T::scalar(FieldType::String)
        }
    }
    fn equivalent(a: &T, b: &T) -> bool {
        if a.nullable != b.nullable || a.domain != b.domain {
            return false;
        }
        match (&a.shape, &b.shape) {
            (S::Union { variants: a }, S::Union { variants: b }) => {
                a.len() == b.len() && a.iter().all(|a| b.iter().any(|b| equivalent(a, b)))
            }
            _ => a == b,
        }
    }
    #[test]
    fn intersection_finite_scalar_algebra() {
        let text = T::scalar(FieldType::String);
        let number = T::scalar(FieldType::Integer);
        let boolean = T::scalar(FieldType::Boolean);
        let mut types = vec![
            text.clone(),
            number.clone(),
            T::scalar(FieldType::Number),
            boolean.clone(),
            named(),
            T::join(text.clone(), number.clone()),
            T::join(number, boolean),
            T {
                shape: S::Never,
                domain: None,
                nullable: false,
            },
        ];
        types.extend(types.clone().into_iter().map(|mut t| {
            t.nullable = true;
            t
        }));
        // Canonical Null, rather than a nullable Never spelling.
        types.retain(|t| !(t.shape == S::Never && t.nullable));
        types.push(T {
            shape: S::Null,
            domain: None,
            nullable: true,
        });
        for a in &types {
            assert!(
                equivalent(&a.intersect_constraints(a).unwrap(), a),
                "idempotence: {a:?}"
            );
            for b in &types {
                let ab = a.intersect_constraints(b).unwrap();
                assert!(
                    equivalent(&ab, &b.intersect_constraints(a).unwrap()),
                    "commutativity: {a:?}, {b:?}"
                );
                for c in &types {
                    let left = ab.intersect_constraints(c).unwrap();
                    let right = a
                        .intersect_constraints(&b.intersect_constraints(c).unwrap())
                        .unwrap();
                    assert!(
                        equivalent(&left, &right),
                        "associativity: {a:?}, {b:?}, {c:?}: {left:?} != {right:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn intersection_rejects_nested_domain_and_wire_conflicts() {
        let a = named();
        let mut b = a.clone();
        b.domain.as_mut().unwrap().catalog_hash = "different-pin".into();
        let wrap = |value| T {
            shape: S::Array {
                element: Box::new(value),
            },
            domain: None,
            nullable: false,
        };
        for (a, b) in [(a.clone(), b.clone()), (wrap(a), wrap(b))] {
            assert!(a
                .intersect_constraints(&b)
                .unwrap_err()
                .contains("distinct value domains"));
            assert!(b.intersect_constraints(&a).is_err());
        }
        let temporal = |wire| T {
            shape: S::Temporal {
                kind: crate::temporal_value::TemporalKind::Datetime,
                wire: Some(wire),
            },
            domain: None,
            nullable: false,
        };
        let a = temporal(crate::TemporalWireFormat::UnixMs);
        let b = temporal(crate::TemporalWireFormat::Rfc3339);
        assert!(a.intersect_constraints(&b).is_err());
        assert!(b.intersect_constraints(&a).is_err());
    }

    #[test]
    fn intersection_preserves_required_presence_and_recursive_domains() {
        let optional = T::record(
            std::collections::BTreeMap::from([("id".into(), named())]),
            std::collections::BTreeSet::from(["id".into()]),
        );
        let required = T::record(
            std::collections::BTreeMap::from([("id".into(), T::scalar(FieldType::String))]),
            Default::default(),
        );
        let expected = T::record(
            std::collections::BTreeMap::from([("id".into(), named())]),
            Default::default(),
        );
        assert_eq!(optional.intersect_constraints(&required).unwrap(), expected);
        assert_eq!(required.intersect_constraints(&optional).unwrap(), expected);
    }
}

#[cfg(test)]
mod denotation_tests {
    use super::*;
    #[test]
    fn intersection_matches_independent_value_sets() {
        use crate::Value;
        // Bit positions denote the literal witnesses below. Expectations are
        // authored independently of the intersection or refinement routines.
        let values = [
            Value::Bool(true),
            Value::Integer(7),
            Value::Float(1.5),
            Value::String("x".into()),
            Value::Null,
        ];
        let mut cases = vec![
            (T::scalar(FieldType::Boolean), 0b00001),
            (T::scalar(FieldType::Integer), 0b00010),
            (T::scalar(FieldType::Number), 0b00110),
            (T::scalar(FieldType::String), 0b01000),
            (
                T::join(T::scalar(FieldType::String), T::scalar(FieldType::Integer)),
                0b01010,
            ),
        ];
        cases.extend(cases.clone().into_iter().map(|(mut t, mask)| {
            t.nullable = true;
            (t, mask | 0b10000)
        }));
        for (a, am) in &cases {
            for (b, bm) in &cases {
                let result = a.intersect_constraints(b).unwrap();
                for (index, value) in values.iter().enumerate() {
                    let accepts = result
                        .validate(value, &crate::CGS::default(), "", "intersection")
                        .is_ok();
                    assert_eq!(
                        accepts,
                        am & bm & (1 << index) != 0,
                        "{a:?} & {b:?} at {value:?}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod record_denotation_tests {
    use super::*;
    #[test]
    fn intersection_matches_closed_record_presence_sets() {
        let text = T::scalar(FieldType::String);
        let int = T::scalar(FieldType::Integer);
        let record = |fields: Vec<(&str, T)>, optional: Vec<&str>| {
            T::record(
                fields
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
                optional.into_iter().map(str::to_string).collect(),
            )
        };
        let cases = [
            (record(vec![], vec![]), 0b0001),
            (record(vec![("id", text.clone())], vec![]), 0b0010),
            (record(vec![("id", text.clone())], vec!["id"]), 0b0011),
            (record(vec![("id", int.clone())], vec![]), 0b0100),
            (
                record(vec![("id", text), ("extra", int)], vec!["extra"]),
                0b1010,
            ),
        ];
        let object = |fields: Vec<(&str, crate::Value)>| {
            crate::Value::Object(
                fields
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            )
        };
        let values = [
            object(vec![]),
            object(vec![("id", "x".into())]),
            object(vec![("id", 7_i64.into())]),
            object(vec![("id", "x".into()), ("extra", 1_i64.into())]),
        ];
        for (a, am) in &cases {
            for (b, bm) in &cases {
                let result = a.intersect_constraints(b).unwrap();
                assert_eq!(result, b.intersect_constraints(a).unwrap());
                for (i, value) in values.iter().enumerate() {
                    assert_eq!(
                        result
                            .validate(value, &crate::CGS::default(), "", "record")
                            .is_ok(),
                        am & bm & (1 << i) != 0,
                        "{a:?} & {b:?} at {value:?}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod unsupported_tests {
    use super::*;
    #[test]
    fn overlapping_specialized_carriers_are_not_proven_empty() {
        let array = T {
            shape: S::Array {
                element: Box::new(T::scalar(FieldType::String)),
            },
            domain: None,
            nullable: false,
        };
        for field in [
            FieldType::Json,
            FieldType::Blob,
            FieldType::Money,
            FieldType::MultiSelect,
        ] {
            let value = T::scalar(field);
            assert!(value.intersect_constraints(&array).is_err());
            assert!(array.intersect_constraints(&value).is_err());
        }
    }
}
