//! Execution of the closed outer row-expression algebra, not a Python interpreter.
//! Branches are lazy; values retain their native Plasm representation.
use crate::Value;
use crate::{ArithOp, FieldPath, PlanPredicateOp, WithExpr, WithLiteral};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::cmp::Ordering;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WithEvaluationError<E: std::error::Error + 'static> {
    #[error(transparent)]
    Field(E),
    #[error(transparent)]
    Arithmetic(#[from] ArithmeticError),
    #[error("invalid number literal")]
    InvalidNumberLiteral,
    #[error("length requires a string, array or record")]
    InvalidLengthOperand,
    #[error(transparent)]
    Comparison(#[from] ComparisonError),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ArithmeticError {
    #[error("arithmetic result is not finite")]
    NonFiniteResult,
    #[error("numeric scalar exceeds decimal range")]
    DecimalOutOfRange,
    #[error("expected numeric scalar")]
    ExpectedNumericScalar,
    #[error("money arithmetic requires matching currencies")]
    MoneyCurrencyMismatch,
    #[error("unsupported money arithmetic dimensions")]
    UnsupportedMoneyDimensions,
    #[error("money arithmetic overflow or division by zero")]
    MoneyOverflow,
    #[error("temporal arithmetic operand is invalid")]
    InvalidTemporalOperand,
    #[error("unsupported string arithmetic")]
    UnsupportedStringArithmetic,
    #[error("integer arithmetic overflow")]
    IntegerOverflow,
    #[error("integer arithmetic result is outside its storage range")]
    IntegerOutOfRange,
    #[error("expected numeric operand")]
    ExpectedNumericOperand,
    #[error("division by zero")]
    DivisionByZero,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ComparisonError {
    #[error("comparison requires finite numbers")]
    NonFiniteNumber,
    #[error("comparison operands are incomparable")]
    IncomparableOperands,
    #[error("unsupported conditional comparison")]
    UnsupportedComparison,
    #[error("money comparison has incompatible currencies `{left}` and `{right}`")]
    MoneyCurrencyMismatch { left: String, right: String },
    #[error("contains requires two strings")]
    ContainsRequiresStrings,
    #[error("string membership requires a string operand")]
    StringMembershipRequiresString,
    #[error("membership requires a string or array")]
    MembershipRequiresCollection,
}

pub fn evaluate_with<E: std::error::Error + 'static>(
    expr: &WithExpr,
    now: DateTime<Utc>,
    field: &mut impl FnMut(&FieldPath) -> Result<Value, E>,
) -> Result<Value, WithEvaluationError<E>> {
    Ok(match expr {
        WithExpr::Field(path) => field(path).map_err(WithEvaluationError::Field)?,
        WithExpr::Now => Value::String(now.to_rfc3339()),
        WithExpr::Literal(literal) => match literal {
            WithLiteral::Null => Value::Null,
            WithLiteral::Bool(v) => Value::from(*v),
            WithLiteral::Integer(v) => Value::from(*v),
            WithLiteral::Number(v) => finite(
                v.parse()
                    .map_err(|_| WithEvaluationError::InvalidNumberLiteral)?,
            )?,
            WithLiteral::String(v) => Value::from(v.clone()),
        },
        WithExpr::Len { field: path } => match field(path).map_err(WithEvaluationError::Field)? {
            Value::Null => Value::Null,
            Value::String(v) => Value::from(v.chars().count()),
            Value::Array(v) => Value::from(v.len()),
            Value::Object(v) => Value::from(v.len()),
            _ => return Err(WithEvaluationError::InvalidLengthOperand),
        },
        WithExpr::Arith { op, lhs, rhs } => arithmetic(
            *op,
            evaluate_with(lhs, now, field)?,
            evaluate_with(rhs, now, field)?,
        )?,
        WithExpr::When {
            lhs,
            op,
            rhs,
            then,
            else_,
        } => {
            let l = evaluate_with(lhs, now, field)?;
            let r = evaluate_with(rhs, now, field)?;
            let condition = compare(*op, &l, &r)?;
            evaluate_with(if condition { then } else { else_ }, now, field)?
        }
    })
}

fn finite(value: f64) -> Result<Value, ArithmeticError> {
    if value.is_finite() {
        Ok(Value::Float(value))
    } else {
        Err(ArithmeticError::NonFiniteResult)
    }
}

fn decimal(value: &Value) -> Result<Decimal, ArithmeticError> {
    match value {
        Value::Integer(v) => Ok(Decimal::from(*v)),
        Value::Unsigned(v) => Ok(Decimal::from(*v)),
        Value::Float(v) => Decimal::from_f64_retain(*v).ok_or(ArithmeticError::DecimalOutOfRange),
        _ => Err(ArithmeticError::ExpectedNumericScalar),
    }
}

pub fn arithmetic(op: ArithOp, l: Value, r: Value) -> Result<Value, ArithmeticError> {
    use ArithOp::*;
    if matches!(l, Value::Null) || matches!(r, Value::Null) {
        return Ok(Value::Null);
    }
    use crate::Value::Money;
    if matches!(l, Money(_)) || matches!(r, Money(_)) {
        let (left, right, currency) = match (&l, &r, op) {
            (Money(a), Money(b), Add | Sub) => {
                if a.currency().map(str::to_ascii_uppercase)
                    != b.currency().map(str::to_ascii_uppercase)
                {
                    return Err(ArithmeticError::MoneyCurrencyMismatch);
                }
                (a.amount(), b.amount(), a.currency())
            }
            (Money(a), _, Mul | Div) => (a.amount(), decimal(&r)?, a.currency()),
            (_, Money(b), Mul) => (decimal(&l)?, b.amount(), b.currency()),
            _ => return Err(ArithmeticError::UnsupportedMoneyDimensions),
        };
        let amount = match op {
            Add => left.checked_add(right),
            Sub => left.checked_sub(right),
            Mul => left.checked_mul(right),
            Div => left.checked_div(right),
        }
        .ok_or(ArithmeticError::MoneyOverflow)?;
        return Ok(Value::Money(crate::MoneyValue::new(
            amount.normalize(),
            currency.map(str::to_owned),
        )));
    }
    if let (Some(l), Some(r)) = (l.as_str(), r.as_str()) {
        return match op {
            Add => Ok(Value::String(format!("{l}{r}"))),
            // Temporal operands have already been admitted by their contracts.
            Sub => {
                let parse = |s: &str| {
                    DateTime::parse_from_rfc3339(s)
                        .map_err(|_| ArithmeticError::InvalidTemporalOperand)
                };
                Ok(Value::Integer((parse(l)? - parse(r)?).num_days()))
            }
            _ => Err(ArithmeticError::UnsupportedStringArithmetic),
        };
    }
    if op != Div
        && matches!(l, Value::Integer(_) | Value::Unsigned(_))
        && matches!(r, Value::Integer(_) | Value::Unsigned(_))
    {
        let unsigned = matches!((&l, &r), (Value::Unsigned(_), Value::Unsigned(_)));
        let integer = |v: &Value| match v {
            Value::Integer(v) => Ok(i128::from(*v)),
            Value::Unsigned(v) => Ok(i128::from(*v)),
            _ => Err(ArithmeticError::ExpectedNumericOperand),
        };
        let (l, r) = (integer(&l)?, integer(&r)?);
        let result = match op {
            Add => l.checked_add(r),
            Sub => l.checked_sub(r),
            Mul => l.checked_mul(r),
            Div => unreachable!(),
        }
        .ok_or(ArithmeticError::IntegerOverflow)?;
        if unsigned {
            return u64::try_from(result)
                .map(Value::Unsigned)
                .map_err(|_| ArithmeticError::IntegerOutOfRange);
        }
        return if let Ok(n) = i64::try_from(result) {
            Ok(Value::Integer(n))
        } else {
            Err(ArithmeticError::IntegerOutOfRange)
        };
    }
    let (l, r) = (
        l.as_number()
            .ok_or(ArithmeticError::ExpectedNumericOperand)?,
        r.as_number()
            .ok_or(ArithmeticError::ExpectedNumericOperand)?,
    );
    if op == Div && r == 0.0 {
        return Err(ArithmeticError::DivisionByZero);
    }
    finite(match op {
        Add => l + r,
        Sub => l - r,
        Mul => l * r,
        Div => l / r,
    })
}

pub fn compare(op: PlanPredicateOp, l: &Value, r: &Value) -> Result<bool, ComparisonError> {
    use PlanPredicateOp::*;
    // Row comparisons retain three-valued null semantics: unknown selects else.
    if matches!(l, Value::Null) || matches!(r, Value::Null) {
        return Ok(false);
    }
    let order = ordering(l, r)?;
    Ok(match op {
        Eq => order == Some(Ordering::Equal) || l == r,
        Ne => !(order == Some(Ordering::Equal) || l == r),
        Lt | Lte | Gt | Gte => {
            let order = order.ok_or(ComparisonError::IncomparableOperands)?;
            match op {
                Lt => order.is_lt(),
                Lte => order.is_le(),
                Gt => order.is_gt(),
                _ => order.is_ge(),
            }
        }
        _ => return Err(ComparisonError::UnsupportedComparison),
    })
}

pub fn ordering(l: &Value, r: &Value) -> Result<Option<Ordering>, ComparisonError> {
    if [l, r]
        .iter()
        .any(|v| matches!(v, Value::Float(n) if !n.is_finite()))
    {
        return Err(ComparisonError::NonFiniteNumber);
    }
    Ok(if l.is_number() && r.is_number() {
        Some(number_order(l, r))
    } else if let (Some(l), Some(r)) = (l.as_str(), r.as_str()) {
        Some(l.cmp(r))
    } else if let (Some(l), Some(r)) = (l.as_bool(), r.as_bool()) {
        Some(l.cmp(&r))
    } else {
        if matches!(l, crate::Value::Money(_)) || matches!(r, crate::Value::Money(_)) {
            crate::money::values_ord(l, r).map_err(|error| {
                ComparisonError::MoneyCurrencyMismatch {
                    left: error.left().to_owned(),
                    right: error.right().to_owned(),
                }
            })?
        } else {
            None
        }
    })
}

fn number_order(l: &Value, r: &Value) -> Ordering {
    let integer = |v: &Value| match v {
        Value::Integer(v) => Some(i128::from(*v)),
        Value::Unsigned(v) => Some(i128::from(*v)),
        _ => None,
    };
    fn int_float(i: i128, f: f64) -> Ordering {
        if f >= i128::MAX as f64 {
            return Ordering::Less;
        }
        if f <= i128::MIN as f64 {
            return Ordering::Greater;
        }
        i.cmp(&(f as i128))
            .then_with(|| 0.0f64.partial_cmp(&f.fract()).unwrap())
    }
    match (integer(l), integer(r)) {
        (Some(l), Some(r)) => l.cmp(&r),
        (Some(l), None) => int_float(l, r.as_number().unwrap()),
        (None, Some(r)) => int_float(r, l.as_number().unwrap()).reverse(),
        (None, None) => l
            .as_number()
            .unwrap()
            .partial_cmp(&r.as_number().unwrap())
            .unwrap(),
    }
}

pub fn predicate(op: PlanPredicateOp, l: &Value, r: &Value) -> Result<bool, ComparisonError> {
    use PlanPredicateOp::*;
    match op {
        Exists => Ok(!matches!(l, Value::Null)),
        Contains => l
            .as_str()
            .zip(r.as_str())
            .map(|(l, r)| l.contains(r))
            .ok_or(ComparisonError::ContainsRequiresStrings),
        In | NotIn => {
            if let Some(haystack) = r.as_str() {
                let needle = l
                    .as_str()
                    .ok_or(ComparisonError::StringMembershipRequiresString)?;
                let found = haystack.contains(needle);
                return Ok(if op == In { found } else { !found });
            }
            let mut found = false;
            for item in r
                .as_array()
                .ok_or(ComparisonError::MembershipRequiresCollection)?
            {
                found |= compare(Eq, l, item)?;
            }
            Ok(if op == In { found } else { !found })
        }
        _ => compare(op, l, r),
    }
}
