//! Outer expression transfer algebra. Selection preserves domains; transformation
//! produces a storage type and never invents a proof of catalog constraints.
use super::{ValueContract, ValueShape};
use crate::ArithOp;
#[cfg(test)]
use crate::FieldType;

impl ValueContract {
    /// A disjoint choice of complete contracts, including nested domain pins.
    pub fn join(left: Self, right: Self) -> Self {
        // An empty materialized collection contributes no element values. It
        // is a subtype of any unpinned collection of the same kind, so a
        // return path such as `return []` need not create a second output ABI.
        fn empty_collection(value: &ValueContract) -> bool {
            if value.nullable || value.domain.is_some() {
                return false;
            }
            match &value.shape {
                ValueShape::Array { element } | ValueShape::Set { element } => {
                    element.shape == ValueShape::Never
                        && !element.nullable
                        && element.domain.is_none()
                }
                ValueShape::Dictionary { key, value } => {
                    key.shape == ValueShape::Never
                        && value.shape == ValueShape::Never
                        && !key.nullable
                        && !value.nullable
                        && key.domain.is_none()
                        && value.domain.is_none()
                }
                _ => false,
            }
        }
        let same_collection = matches!(
            (&left.shape, &right.shape),
            (ValueShape::Array { .. }, ValueShape::Array { .. })
                | (ValueShape::Set { .. }, ValueShape::Set { .. })
                | (ValueShape::Dictionary { .. }, ValueShape::Dictionary { .. })
        );
        if same_collection && left.domain.is_none() && right.domain.is_none() {
            if empty_collection(&left) && !right.nullable {
                return right;
            }
            if empty_collection(&right) && !left.nullable {
                return left;
            }
        }
        fn append(mut value: ValueContract, out: &mut Vec<ValueContract>, nullable: &mut bool) {
            *nullable |= value.nullable || value.shape == ValueShape::Null;
            value.nullable = false;
            match value.shape {
                ValueShape::Null | ValueShape::Never => {}
                ValueShape::Union { variants } if value.domain.is_none() => {
                    for variant in variants {
                        append(variant, out, nullable);
                    }
                }
                _ => {
                    if !out.contains(&value) {
                        out.push(value);
                    }
                }
            }
        }
        let mut variants = Vec::new();
        let mut nullable = false;
        append(left, &mut variants, &mut nullable);
        append(right, &mut variants, &mut nullable);
        let mut result = if variants.len() == 1 {
            variants.remove(0)
        } else {
            Self {
                shape: if variants.is_empty() {
                    if nullable {
                        ValueShape::Null
                    } else {
                        ValueShape::Never
                    }
                } else {
                    ValueShape::Union { variants }
                },
                domain: None,
                nullable: false,
            }
        };
        result.nullable = nullable;
        result
    }

    /// Universal distribution: every possible operand pair must admit the operator.
    pub fn arithmetic(
        op: ArithOp,
        left: &Self,
        right: &Self,
    ) -> Result<Self, crate::value_arithmetic::ArithmeticContractError> {
        crate::value_arithmetic::Arithmetic::binary_result(left, op, right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_collections_are_neutral_for_matching_materialized_returns() {
        let empty = ValueContract {
            shape: ValueShape::Array {
                element: Box::new(ValueContract {
                    shape: ValueShape::Never,
                    domain: None,
                    nullable: false,
                }),
            },
            domain: None,
            nullable: false,
        };
        let strings = ValueContract {
            shape: ValueShape::Array {
                element: Box::new(ValueContract::scalar(FieldType::String)),
            },
            domain: None,
            nullable: false,
        };
        assert_eq!(ValueContract::join(empty.clone(), strings.clone()), strings);
        assert_eq!(ValueContract::join(strings.clone(), empty), strings);
    }

    #[test]
    fn operator_product_has_explicit_positive_and_negative_laws() {
        use FieldType::*;
        let types = [Integer, Number, String, Boolean, Money, Json, Array, Date];
        for (i, left) in types.iter().enumerate() {
            for (j, right) in types.iter().enumerate() {
                for (k, op) in [ArithOp::Add, ArithOp::Sub, ArithOp::Mul, ArithOp::Div]
                    .into_iter()
                    .enumerate()
                {
                    let expected = match (i, j, k) {
                        (0, 0, 0..=2) => Some(Integer),
                        (0..=1, 0..=1, _) => Some(Number),
                        (2, 2, 0) => Some(String),
                        (4, 4, 0..=1) | (4, 0..=1, 2..=3) | (0..=1, 4, 2) => Some(Money),
                        (7, 7, 1) => Some(Integer),
                        _ => None,
                    };
                    for nullable in [false, true] {
                        let mut l = ValueContract::scalar(left.clone());
                        l.nullable = nullable;
                        let actual = ValueContract::arithmetic(
                            op,
                            &l,
                            &ValueContract::scalar(right.clone()),
                        );
                        match &expected {
                            Some(kind) => {
                                let actual = actual.unwrap();
                                assert_eq!(
                                    actual.shape,
                                    ValueShape::Scalar {
                                        field_type: kind.clone()
                                    }
                                );
                                assert_eq!(actual.nullable, nullable);
                                assert!(actual.domain.is_none());
                            }
                            None => assert!(actual.is_err(), "{left:?} {op:?} {right:?}"),
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn choice_preserves_domains_and_arithmetic_distributes_without_inheriting_constraints() {
        let domain = |key: &str| {
            let mut t = ValueContract::scalar(FieldType::Integer);
            t.domain = Some(super::super::DomainRef {
                entry_id: "fixture".into(),
                catalog_hash: "pin".into(),
                value_ref: crate::ValueDomainKey::new(key).unwrap(),
            });
            t
        };
        let (a, b) = (domain("a"), domain("b"));
        let union = ValueContract::join(a.clone(), b.clone());
        assert_eq!(
            union.shape,
            ValueShape::Union {
                variants: vec![a.clone(), b]
            }
        );
        assert_eq!(ValueContract::join(union.clone(), a), union);
        let number = ValueContract::scalar(FieldType::Number);
        let result = ValueContract::arithmetic(ArithOp::Mul, &union, &number).unwrap();
        assert_eq!(result, number);
        let invalid = ValueContract::join(union, ValueContract::scalar(FieldType::String));
        assert!(ValueContract::arithmetic(ArithOp::Mul, &invalid, &number).is_err());
    }
}
