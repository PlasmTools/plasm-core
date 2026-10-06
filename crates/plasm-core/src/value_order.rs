//! Declared ordering semantics, shared by admission and row execution.
use crate::{
    temporal_value::TemporalKind,
    value_contract::{ValueContract, ValueShape},
    FieldType, Value,
};
use std::cmp::Ordering;

/// Resolve a semantic operation without consulting observed row values.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrderingError {
    #[error("ordering is not defined for this declared domain")]
    UnsupportedDomain,
    #[error("union variants do not share an ordering domain")]
    IncompatibleUnion,
    #[error("null has no scalar ordering")]
    NullOperand,
    #[error("cannot order naive and aware temporal values")]
    IncompatibleAwareness,
    #[error("value does not inhabit declared ordering domain")]
    InvalidRepresentation,
    #[error("invalid temporal ordered value")]
    InvalidTemporalValue(#[source] TemporalValueError),
    #[error(transparent)]
    Expression(#[from] crate::value_expression::ComparisonError),
    #[error("invalid monetary comparison")]
    InvalidMoney(#[source] crate::money::CrossCurrencyError),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemporalValueError {
    #[error("value does not inhabit temporal union")]
    UnionUnmatched,
    #[error("ambiguous temporal union representation")]
    AmbiguousUnion,
    #[error("temporal contract missing")]
    MissingContract,
    #[error("missing temporal component `{0}`")]
    MissingComponent(&'static str),
    #[error("invalid temporal component `{0}`")]
    InvalidComponent(&'static str),
    #[error("invalid temporal calendar date")]
    InvalidCalendarDate,
    #[error("timezone offset is absent")]
    MissingTimezoneOffset,
    #[error("expected {0}")]
    Expected(&'static str),
    #[error("temporal kind mismatch")]
    KindMismatch,
    #[error(transparent)]
    InvalidRepresentation(#[from] crate::temporal_value::TemporalValueError),
}

pub trait Orderable {
    fn ordering(&self) -> Result<ValueOrdering<'_>, OrderingError>;
}

#[derive(Clone, Copy)]
pub struct ValueOrdering<'a> {
    contract: &'a ValueContract,
    family: Family,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Number,
    Text,
    Bool,
    Money,
    Temporal(TemporalKind),
    Empty,
}

fn family(contract: &ValueContract) -> Result<Family, OrderingError> {
    Ok(match &contract.shape {
        ValueShape::Null | ValueShape::Never => Family::Empty,
        ValueShape::Temporal {
            kind: TemporalKind::Timezone,
            ..
        } => return Err(OrderingError::UnsupportedDomain),
        ValueShape::Temporal { kind, .. } => Family::Temporal(*kind),
        ValueShape::Scalar { field_type } => match field_type {
            FieldType::Integer | FieldType::Number => Family::Number,
            FieldType::String | FieldType::Uuid | FieldType::DigitId | FieldType::Select => {
                Family::Text
            }
            FieldType::Boolean => Family::Bool,
            FieldType::Money => Family::Money,
            _ => return Err(OrderingError::UnsupportedDomain),
        },
        ValueShape::Union { variants } => {
            let mut result = Family::Empty;
            for variant in variants {
                let next = family(variant)?;
                if next == Family::Empty {
                    continue;
                }
                if result != Family::Empty && result != next {
                    return Err(OrderingError::IncompatibleUnion);
                }
                result = next;
            }
            result
        }
        _ => return Err(OrderingError::UnsupportedDomain),
    })
}

impl Orderable for ValueContract {
    fn ordering(&self) -> Result<ValueOrdering<'_>, OrderingError> {
        Ok(ValueOrdering {
            contract: self,
            family: family(self)?,
        })
    }
}

impl ValueOrdering<'_> {
    /// Validate even singleton inputs; no comparison is needed to expose bad values.
    pub fn validate(&self, value: &Value) -> Result<(), OrderingError> {
        if value.is_null() {
            return Ok(());
        }
        self.compare(value, value).map(|_| ())
    }

    /// Predicate literals may use the money kernel's exact major-unit notation.
    /// This does not admit those literals as stored inhabitants of a money field.
    pub fn compare_literal(
        &self,
        value: &Value,
        literal: &Value,
    ) -> Result<Ordering, OrderingError> {
        if self.family == Family::Money {
            self.validate(value)?;
            return crate::money::values_ord(value, literal)
                .map_err(OrderingError::InvalidMoney)?
                .ok_or(OrderingError::InvalidRepresentation);
        }
        self.compare(value, literal)
    }

    /// Equality does not impose orderability between naive and aware values.
    pub fn equal_literal(&self, value: &Value, literal: &Value) -> Result<bool, OrderingError> {
        if let Family::Temporal(kind) = self.family {
            let left = temporal_key(self.contract, value, kind)
                .map_err(OrderingError::InvalidTemporalValue)?;
            let right = temporal_key(self.contract, literal, kind)
                .map_err(OrderingError::InvalidTemporalValue)?;
            return Ok(left == right);
        }
        if self.family == Family::Money {
            self.validate(value)?;
            return crate::money::values_eq(value, literal).map_err(OrderingError::InvalidMoney);
        }
        self.compare_literal(value, literal)
            .map(|order| order.is_eq())
    }

    /// Null placement belongs to the consuming operator, not scalar ordering.
    pub fn compare(&self, left: &Value, right: &Value) -> Result<Ordering, OrderingError> {
        if left.is_null() || right.is_null() {
            return Err(OrderingError::NullOperand);
        }
        match self.family {
            Family::Temporal(kind) => {
                let a = temporal_key(self.contract, left, kind)
                    .map_err(OrderingError::InvalidTemporalValue)?;
                let b = temporal_key(self.contract, right, kind)
                    .map_err(OrderingError::InvalidTemporalValue)?;
                if a.0 != b.0 {
                    return Err(OrderingError::IncompatibleAwareness);
                }
                Ok(a.1.cmp(&b.1))
            }
            domain => {
                let valid = |v: &Value| match domain {
                    Family::Number => v.is_number(),
                    Family::Text => v.as_str().is_some(),
                    Family::Bool => matches!(v, Value::Bool(_)),
                    Family::Money => matches!(v, Value::Money(_)),
                    _ => false,
                };
                if !valid(left) || !valid(right) {
                    return Err(OrderingError::InvalidRepresentation);
                }
                crate::value_expression::ordering(left, right)?
                    .ok_or(OrderingError::InvalidRepresentation)
            }
        }
    }
}

pub(crate) fn temporal_key(
    contract: &ValueContract,
    value: &Value,
    kind: TemporalKind,
) -> Result<(bool, i128), TemporalValueError> {
    if let ValueShape::Union { variants } = &contract.shape {
        let mut keys = variants
            .iter()
            .filter_map(|v| temporal_key(v, value, kind).ok());
        let first = keys.next().ok_or(TemporalValueError::UnionUnmatched)?;
        if keys.any(|key| key != first) {
            return Err(TemporalValueError::AmbiguousUnion);
        }
        return Ok(first);
    }
    let ValueShape::Temporal { wire, .. } = contract.shape else {
        return Err(TemporalValueError::MissingContract);
    };
    let c = crate::temporal_value::components(value, kind, wire)
        .map_err(TemporalValueError::InvalidRepresentation)?;
    if kind == TemporalKind::Timezone {
        return c
            .get("offset_seconds")
            .and_then(Value::as_integer)
            .map(|offset| (true, i128::from(offset)))
            .ok_or(TemporalValueError::MissingTimezoneOffset);
    }
    let n = |name: &str| {
        c.get(name)
            .and_then(Value::as_integer)
            .ok_or(TemporalValueError::MissingComponent(match name {
                "year" => "year",
                "month" => "month",
                "day" => "day",
                "hour" => "hour",
                "minute" => "minute",
                "second" => "second",
                "microsecond" => "microsecond",
                "days" => "days",
                "seconds" => "seconds",
                "microseconds" => "microseconds",
                _ => "unknown",
            }))
    };
    let offset = c.get("offset_seconds").and_then(Value::as_integer);
    let day = match kind {
        TemporalKind::Date | TemporalKind::Datetime => {
            let date = chrono::NaiveDate::from_ymd_opt(
                n("year")? as i32,
                n("month")? as u32,
                n("day")? as u32,
            )
            .ok_or(TemporalValueError::InvalidCalendarDate)?;
            i128::from(chrono::Datelike::num_days_from_ce(&date))
        }
        TemporalKind::Timedelta => i128::from(n("days")?),
        _ => 0,
    };
    let micros = match kind {
        TemporalKind::Date => 0,
        TemporalKind::Timedelta => {
            i128::from(n("seconds")?) * 1_000_000 + i128::from(n("microseconds")?)
        }
        _ => {
            (i128::from(n("hour")?) * 3600
                + i128::from(n("minute")?) * 60
                + i128::from(n("second")?)
                - i128::from(offset.unwrap_or(0)))
                * 1_000_000
                + i128::from(n("microsecond")?)
        }
    };
    Ok((offset.is_some(), day * 86_400_000_000 + micros))
}

#[cfg(test)]
mod tests {
    #[test]
    fn monetary_ordering_retains_currency_conflict_source() {
        let error = super::OrderingError::InvalidMoney(
            crate::money::currency_conflict(Some("USD"), Some("EUR")).unwrap_err(),
        );
        let source = std::error::Error::source(&error)
            .unwrap()
            .downcast_ref::<crate::money::CrossCurrencyError>()
            .unwrap();
        assert_eq!(source.left(), "USD");
        assert_eq!(source.right(), "EUR");
    }
    use super::*;
    use crate::TemporalWireFormat;
    #[test]
    fn temporal_order_is_not_wire_lexical_order() {
        let mut contract = TemporalKind::Datetime.contract();
        contract.shape = ValueShape::Temporal {
            kind: TemporalKind::Datetime,
            wire: Some(TemporalWireFormat::Rfc3339),
        };
        let order = contract.ordering().unwrap();
        let a = Value::String("2024-01-01T01:00:00+02:00".into());
        let b = Value::String("2024-01-01T00:00:00Z".into());
        assert_eq!(order.compare(&a, &b).unwrap(), Ordering::Less);
        let same = Value::String("2023-12-31T23:00:00Z".into());
        assert_eq!(order.compare(&a, &same).unwrap(), Ordering::Equal);
        assert!(order.validate(&Value::String("invalid".into())).is_err());
        assert!(matches!(
            order.validate(&Value::String("invalid".into())),
            Err(OrderingError::InvalidTemporalValue(_))
        ));
    }
    #[test]
    fn numeric_order_laws_cover_signed_unsigned_and_float_boundaries() {
        let contract = ValueContract::scalar(FieldType::Number);
        let order = contract.ordering().unwrap();
        let values = [
            Value::Integer(i64::MIN),
            Value::Integer(-1),
            Value::Integer(0),
            Value::Float(0.5),
            Value::Integer(9_007_199_254_740_993),
            Value::Unsigned(u64::MAX),
        ];
        for (i, a) in values.iter().enumerate() {
            for (j, b) in values.iter().enumerate() {
                assert_eq!(order.compare(a, b).unwrap(), i.cmp(&j));
                assert_eq!(
                    order.compare(a, b).unwrap(),
                    order.compare(b, a).unwrap().reverse()
                );
            }
        }
        assert!(order.validate(&Value::Float(f64::NAN)).is_err());
    }
    #[test]
    fn unsupported_domains_reject_without_rows() {
        assert!(ValueContract::scalar(FieldType::Json).ordering().is_err());
        assert!(TemporalKind::Timezone.contract().ordering().is_err());
        let union = ValueContract::join(
            ValueContract::scalar(FieldType::String),
            ValueContract::scalar(FieldType::Integer),
        );
        assert!(union.ordering().is_err());
    }
}

#[cfg(test)]
mod properties {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn temporal_offsets_preserve_instant_order(seconds in 0i64..2_000_000_000, delta in -100000i64..100000, offset in -1439i32..1439) {
            let contract = TemporalKind::Datetime.contract();
            let order = contract.ordering().unwrap();
            let zone=chrono::FixedOffset::east_opt(offset * 60).unwrap();
            let a=chrono::DateTime::from_timestamp(seconds,0).unwrap();
            let b=chrono::DateTime::from_timestamp(seconds+delta,0).unwrap();
            let left=Value::String(a.with_timezone(&zone).to_rfc3339());
            let same=Value::String(a.to_rfc3339());
            let right=Value::String(b.to_rfc3339());
            prop_assert_eq!(order.compare(&left,&same).unwrap(),Ordering::Equal);
            prop_assert_eq!(order.compare(&left,&right).unwrap(),0i64.cmp(&delta));
        }
        #[test]
        fn integer_comparison_is_transitive(a in any::<i64>(), b in any::<i64>(), c in any::<i64>()) {
            let contract=ValueContract::scalar(FieldType::Integer);
            let order=contract.ordering().unwrap();
            let (a,b,c)=(Value::Integer(a),Value::Integer(b),Value::Integer(c));
            if order.compare(&a,&b).unwrap().is_le() && order.compare(&b,&c).unwrap().is_le() {
                prop_assert!(order.compare(&a,&c).unwrap().is_le());
            }
        }
    }
}
