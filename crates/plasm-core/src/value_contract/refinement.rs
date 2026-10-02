//! Intersection with inferred evidence preserves the original value's metadata.
use super::{ValueContract as T, ValueShape as S};
impl T {
    pub fn refined_by(&self, evidence: &Self) -> Result<Self, String> {
        meet(self, evidence, 0)
    }
    /// Check representation after a branch selection. Domains remain guaranteed
    /// by the original validated value; a refinement cannot manufacture a domain.
    pub fn validate_refinement_value(&self, value: &crate::Value) -> Result<(), String> {
        fn erase(t: &mut T, depth: usize) -> Result<(), String> {
            if depth >= 64 {
                return Err("refinement depth exceeds 64".into());
            }
            t.domain = None;
            match &mut t.shape {
                S::Array { element } | S::Set { element } => erase(element, depth + 1)?,
                S::MappingRecord { record } => erase(record, depth + 1)?,
                S::Union { variants } => {
                    for value in variants {
                        erase(value, depth + 1)?;
                    }
                }
                S::Record { fields } | S::ObservedRecord { fields, .. } => {
                    for value in fields.values_mut() {
                        erase(value, depth + 1)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        let mut storage = self.clone();
        erase(&mut storage, 0)?;
        storage.validate_at(
            value,
            &crate::CGS::default(),
            "",
            "branch refinement",
            0,
            &|_| None,
            super::ValidationBoundary::Observation,
        )
    }
}
fn bottom() -> T {
    T {
        shape: S::Never,
        domain: None,
        nullable: false,
    }
}
fn meet(original: &T, evidence: &T, depth: usize) -> Result<T, String> {
    if depth >= 64 {
        return Err("refinement depth exceeds 64".into());
    }
    let nullable = (original.nullable || original.shape == S::Null)
        && (evidence.nullable || evidence.shape == S::Null);
    let mut result = match (&original.shape, &evidence.shape) {
        (S::Union { variants }, _) => {
            let mut result = bottom();
            for variant in variants {
                result = T::join(result, meet(variant, evidence, depth + 1)?);
            }
            result
        }
        (_, S::Union { variants }) => {
            let mut result = bottom();
            for variant in variants {
                result = T::join(result, meet(original, variant, depth + 1)?);
            }
            result
        }
        (S::Scalar { field_type: a }, S::Scalar { field_type: b }) if a == b => original.clone(),
        (S::Temporal { kind: a, .. }, S::Temporal { kind: b, .. }) if a == b => original.clone(),
        (S::Set { element: a }, S::Set { element: b }) => T {
            shape: S::Set {
                element: Box::new(meet(a, b, depth + 1)?),
            },
            ..original.clone()
        },
        (S::MappingRecord { record: a }, S::MappingRecord { record: b }) => T {
            shape: S::MappingRecord {
                record: Box::new(meet(a, b, depth + 1)?),
            },
            ..original.clone()
        },
        (S::Array { element: a }, S::Array { element: b }) => T {
            shape: S::Array {
                element: Box::new(meet(a, b, depth + 1)?),
            },
            ..original.clone()
        },
        (
            S::Record { fields: a } | S::ObservedRecord { fields: a, .. },
            S::Record { fields: b } | S::ObservedRecord { fields: b, .. },
        ) => {
            if b.keys().any(|k| !a.contains_key(k)) {
                let mut result = bottom();
                if nullable {
                    result.shape = S::Null;
                    result.nullable = true;
                }
                return Ok(result);
            }
            let mut result = original.clone();
            let (S::Record { fields } | S::ObservedRecord { fields, .. }) = &mut result.shape
            else {
                unreachable!()
            };
            for (name, evidence) in b {
                fields.insert(name.clone(), meet(&a[name], evidence, depth + 1)?);
            }
            result
        }
        _ => bottom(),
    };
    result.nullable = nullable;
    if result.shape == S::Never && nullable {
        result.shape = S::Null;
    }
    if original.domain.is_some() {
        result.domain = original.domain.clone();
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{value_contract::DomainRef, FieldType, Value, ValueDomainKey};
    #[test]
    fn refinement_intersects_unions_without_granting_domains() {
        let text = T::scalar(FieldType::String);
        let number = T::scalar(FieldType::Integer);
        let mut original = T::join(text.clone(), number.clone());
        original.nullable = true;
        let mut forged = text.clone();
        forged.domain = Some(DomainRef {
            entry_id: "other".into(),
            catalog_hash: "forged".into(),
            value_ref: ValueDomainKey::new("secret").unwrap(),
        });
        assert_eq!(original.refined_by(&forged).unwrap(), text);
        assert!(text.validate_refinement_value(&Value::Integer(1)).is_err());
        assert!(text.validate_refinement_value(&Value::Null).is_err());
        assert_eq!(number.refined_by(&text).unwrap().shape, S::Never);
    }
    #[test]
    fn refinement_retains_temporal_wire_representation() {
        let original = T {
            shape: S::Temporal {
                kind: crate::temporal_value::TemporalKind::Datetime,
                wire: Some(crate::TemporalWireFormat::UnixMs),
            },
            domain: None,
            nullable: true,
        };
        let refined = original
            .refined_by(&crate::temporal_value::TemporalKind::Datetime.contract())
            .unwrap();
        assert_eq!(refined.shape, original.shape);
        assert!(!refined.nullable);
    }
    #[test]
    fn observed_refinement_checks_known_fields_without_closing_the_graph_row() {
        use std::collections::{BTreeMap, BTreeSet};
        let contract = T::record(
            BTreeMap::from([
                ("score".into(), T::scalar(FieldType::Integer)),
                ("optional".into(), T::scalar(FieldType::String)),
            ]),
            BTreeSet::from(["optional".into()]),
        );
        let row = |score| {
            Value::Object(indexmap::IndexMap::from([
                ("score".into(), score),
                ("hydrated_relation".into(), Value::Array(vec![])),
            ]))
        };
        let valid = row(Value::Integer(3));
        contract.validate_refinement_value(&valid).unwrap();
        assert!(contract
            .validate(&valid, &crate::CGS::default(), "", "output")
            .is_err());
        assert!(contract
            .validate_refinement_value(&row(Value::Null))
            .is_err());
        assert!(contract
            .validate_refinement_value(&Value::Object(Default::default()))
            .is_err());
        let nested = T {
            shape: S::Array {
                element: Box::new(contract),
            },
            domain: None,
            nullable: false,
        };
        nested
            .validate_refinement_value(&Value::Array(vec![valid]))
            .unwrap();
        let closed = T::record(
            BTreeMap::from([("score".into(), T::scalar(FieldType::Integer))]),
            BTreeSet::new(),
        );
        assert!(closed
            .validate_refinement_value(&row(Value::Integer(3)))
            .is_err());
    }
}
