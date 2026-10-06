//! Arithmetic reduction capabilities resolved from every declared union variant.
use crate::{
    value_contract::{ValueContract, ValueShape},
    AggregateFunction, FieldType, Value,
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ArithmeticContractError {
    #[error("arithmetic union has incompatible dimensions")]
    IncompatibleUnionDimensions,
    #[error("arithmetic requires a numeric or money contract")]
    NonArithmeticDomain,
    #[error("arithmetic domain has no numeric inhabitants")]
    EmptyArithmeticDomain,
    #[error("operation is not an arithmetic reduction")]
    NotArithmeticReduction,
    #[error("value does not inhabit the declared arithmetic domain")]
    InvalidValueDomain,
    #[error("arithmetic operand shape is unsupported")]
    UnsupportedOperandShape,
    #[error("arithmetic operator is unsupported for the operand domains")]
    UnsupportedOperandDomains,
    #[error("arithmetic union has no variants")]
    EmptyUnion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithmeticDomain {
    Integer,
    Number,
    Money,
}

pub trait Arithmetic {
    fn binary_result(
        &self,
        op: crate::ArithOp,
        right: &ValueContract,
    ) -> Result<ValueContract, ArithmeticContractError>;
    fn arithmetic_domain(&self) -> Result<ArithmeticDomain, ArithmeticContractError>;
}
impl Arithmetic for ValueContract {
    fn binary_result(
        &self,
        op: crate::ArithOp,
        right: &ValueContract,
    ) -> Result<ValueContract, ArithmeticContractError> {
        binary_result(op, self, right)
    }
    fn arithmetic_domain(&self) -> Result<ArithmeticDomain, ArithmeticContractError> {
        fn resolve(c: &ValueContract) -> Result<Option<ArithmeticDomain>, ArithmeticContractError> {
            use ArithmeticDomain::*;
            Ok(match &c.shape {
                ValueShape::Null | ValueShape::Never => None,
                ValueShape::Scalar {
                    field_type: FieldType::Integer,
                } => Some(Integer),
                ValueShape::Scalar {
                    field_type: FieldType::Number,
                } => Some(Number),
                ValueShape::Scalar {
                    field_type: FieldType::Money,
                } => Some(Money),
                ValueShape::Union { variants } => {
                    let mut result = None;
                    for c in variants {
                        if let Some(next) = resolve(c)? {
                            result = Some(match (result, next) {
                                (None, v) => v,
                                (Some(a), b) if a == b => a,
                                (Some(Integer | Number), Integer | Number) => Number,
                                _ => {
                                    return Err(
                                        ArithmeticContractError::IncompatibleUnionDimensions,
                                    )
                                }
                            });
                        }
                    }
                    result
                }
                _ => return Err(ArithmeticContractError::NonArithmeticDomain),
            })
        }
        resolve(self)?.ok_or(ArithmeticContractError::EmptyArithmeticDomain)
    }
}
impl ArithmeticDomain {
    pub fn reduction_result(
        self,
        function: AggregateFunction,
    ) -> Result<ValueContract, ArithmeticContractError> {
        use AggregateFunction::*;
        if !matches!(function, Sum | Avg) {
            return Err(ArithmeticContractError::NotArithmeticReduction);
        }
        let mut result = ValueContract::scalar(match (self, function) {
            (Self::Money, _) => FieldType::Money,
            (Self::Integer, Sum) => FieldType::Integer,
            _ => FieldType::Number,
        });
        result.nullable = function == Avg;
        Ok(result)
    }
    pub fn validate(self, value: &Value) -> Result<(), ArithmeticContractError> {
        let valid = match self {
            Self::Integer => matches!(value, Value::Integer(_) | Value::Unsigned(_)),
            Self::Number => value.is_number(),
            Self::Money => matches!(value, Value::Money(_)),
        };
        if valid && value.as_number().is_none_or(f64::is_finite) {
            Ok(())
        } else {
            Err(ArithmeticContractError::InvalidValueDomain)
        }
    }
    pub fn zero(self) -> Value {
        match self {
            Self::Money => Value::Money(crate::MoneyValue::new(rust_decimal::Decimal::ZERO, None)),
            Self::Integer | Self::Number => Value::Integer(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reduction_capability_distributes_over_union_and_removes_source_domains() {
        let types = [
            FieldType::Integer,
            FieldType::Number,
            FieldType::Money,
            FieldType::String,
            FieldType::Boolean,
        ];
        for (i, a) in types.iter().enumerate() {
            for (j, b) in types.iter().enumerate() {
                for (a, b) in [(a, b), (b, a)] {
                    let union = ValueContract::join(
                        ValueContract::scalar(a.clone()),
                        ValueContract::scalar(b.clone()),
                    );
                    let result = union.arithmetic_domain();
                    assert_eq!(result.is_ok(), (i < 2 && j < 2) || (i == 2 && j == 2));
                    if let Ok(domain) = result {
                        for op in [AggregateFunction::Sum, AggregateFunction::Avg] {
                            let result = ValueContract::aggregate(op, Some(&union)).unwrap();
                            assert_eq!(result, domain.reduction_result(op).unwrap());
                            assert!(result.domain.is_none());
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn nullable_numeric_union_and_bad_runtime_inhabitants() {
        let mut a = ValueContract::scalar(FieldType::Integer);
        a.nullable = true;
        let b = ValueContract::scalar(FieldType::Number);
        assert_eq!(
            ValueContract::join(a, b).arithmetic_domain().unwrap(),
            ArithmeticDomain::Number
        );
        assert!(ArithmeticDomain::Integer
            .validate(&Value::Float(1.0))
            .is_err());
        assert!(ArithmeticDomain::Number
            .validate(&Value::Float(f64::INFINITY))
            .is_err());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperandDomain {
    Integer,
    Number,
    Money,
    String,
    Temporal(crate::temporal_value::TemporalKind),
}
fn operand_domain(c: &ValueContract) -> Result<OperandDomain, ArithmeticContractError> {
    Ok(match &c.shape {
        ValueShape::Scalar { field_type } => match field_type {
            FieldType::Integer => OperandDomain::Integer,
            FieldType::Number => OperandDomain::Number,
            FieldType::Money => OperandDomain::Money,
            FieldType::String | FieldType::Uuid | FieldType::DigitId | FieldType::Select => {
                OperandDomain::String
            }
            _ => return Err(ArithmeticContractError::UnsupportedOperandShape),
        },
        ValueShape::Temporal { kind, .. } => OperandDomain::Temporal(*kind),
        _ => return Err(ArithmeticContractError::UnsupportedOperandShape),
    })
}
fn binary_result(
    op: crate::ArithOp,
    left: &ValueContract,
    right: &ValueContract,
) -> Result<ValueContract, ArithmeticContractError> {
    use crate::ArithOp;
    if let ValueShape::Union { variants } = &left.shape {
        let results = variants
            .iter()
            .map(|v| binary_result(op, v, right))
            .collect::<Result<Vec<_>, _>>()?;
        let mut result = results
            .into_iter()
            .reduce(ValueContract::join)
            .ok_or(ArithmeticContractError::EmptyUnion)?;
        result.nullable |= left.nullable;
        return Ok(result);
    }
    if let ValueShape::Union { variants } = &right.shape {
        let results = variants
            .iter()
            .map(|v| binary_result(op, left, v))
            .collect::<Result<Vec<_>, _>>()?;
        let mut result = results
            .into_iter()
            .reduce(ValueContract::join)
            .ok_or(ArithmeticContractError::EmptyUnion)?;
        result.nullable |= right.nullable;
        return Ok(result);
    }
    let (l, r) = (operand_domain(left)?, operand_domain(right)?);
    let numeric = |k| matches!(k, OperandDomain::Integer | OperandDomain::Number);
    let kind = match op {
        ArithOp::Add if l == OperandDomain::String && r == OperandDomain::String => {
            FieldType::String
        }
        ArithOp::Sub
            if matches!(
                l,
                OperandDomain::Temporal(crate::temporal_value::TemporalKind::Datetime)
            ) && l == r =>
        {
            FieldType::Integer
        }
        ArithOp::Add | ArithOp::Sub if l == OperandDomain::Money && r == OperandDomain::Money => {
            FieldType::Money
        }
        ArithOp::Mul
            if (l == OperandDomain::Money && numeric(r))
                || (numeric(l) && r == OperandDomain::Money) =>
        {
            FieldType::Money
        }
        ArithOp::Div if l == OperandDomain::Money && numeric(r) => FieldType::Money,
        _ if numeric(l) && numeric(r) => {
            if op == ArithOp::Div || l == OperandDomain::Number || r == OperandDomain::Number {
                FieldType::Number
            } else {
                FieldType::Integer
            }
        }
        _ => return Err(ArithmeticContractError::UnsupportedOperandDomains),
    };
    let mut result = ValueContract::scalar(kind);
    result.nullable = left.nullable || right.nullable;
    Ok(result)
}
