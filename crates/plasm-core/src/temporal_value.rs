//! Lossless temporal values at the Python boundary; API encodings remain contracts.
use crate::Value;
use crate::{
    value_contract::{ValueContract, ValueShape},
    TemporalWireFormat as Wire,
};
use chrono::{Datelike, Timelike};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalKind {
    Date,
    Datetime,
    Time,
    Timedelta,
    Timezone,
}

impl TemporalKind {
    pub fn python_name(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Datetime => "datetime",
            Self::Time => "time",
            Self::Timedelta => "timedelta",
            Self::Timezone => "timezone",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "date" => Self::Date,
            "datetime" => Self::Datetime,
            "time" => Self::Time,
            "timedelta" => Self::Timedelta,
            "timezone" => Self::Timezone,
            _ => return None,
        })
    }
    pub fn contract(self) -> ValueContract {
        ValueContract {
            shape: ValueShape::Temporal {
                kind: self,
                wire: None,
            },
            domain: None,
            nullable: false,
        }
    }
}

pub fn tagged(kind: TemporalKind, components: Value) -> Value {
    Value::Object(indexmap::IndexMap::from([
        (
            "__plasm_temporal".into(),
            Value::String(kind.python_name().into()),
        ),
        ("components".into(), components),
    ]))
}

/// A tag is interpreted only under a temporal contract, never as arbitrary JSON authority.
pub fn components(value: &Value, kind: TemporalKind, wire: Option<Wire>) -> Result<Value, String> {
    if value.get("__plasm_temporal").is_some() {
        if value.get("__plasm_temporal") != Some(&Value::String(kind.python_name().into()))
            || value.as_object().is_none_or(|o| o.len() != 2)
        {
            return Err("temporal kind mismatch".into());
        }
        let c = value
            .get("components")
            .ok_or("missing temporal components")?;
        validate_components(c, kind)?;
        if let Some(wire) = wire {
            // A typed tag does not bypass the catalog's timezone/precision contract.
            // encode validates components without a wire before checking representability.
            encode(value, wire)?;
        }
        return Ok(c.clone());
    }
    let parsed = match kind {
        TemporalKind::Date => {
            let date = chrono::NaiveDate::parse_from_str(
                value.as_str().ok_or("expected date string")?,
                "%Y-%m-%d",
            )
            .map_err(|e| e.to_string())?;
            Value::Object(indexmap::IndexMap::from([
                ("year".into(), Value::Integer(date.year().into())),
                ("month".into(), Value::Integer(date.month().into())),
                ("day".into(), Value::Integer(date.day().into())),
            ]))
        }
        TemporalKind::Datetime if wire == Some(Wire::Iso8601NaiveDatetime) => {
            let dt = chrono::NaiveDateTime::parse_from_str(
                value.as_str().ok_or("expected naive ISO datetime string")?,
                "%Y-%m-%dT%H:%M:%S%.f",
            )
            .map_err(|e| e.to_string())?;
            if dt.nanosecond() % 1000 != 0 {
                return Err("datetime precision exceeds Python microseconds".into());
            }
            Value::Object(indexmap::IndexMap::from([
                ("year".into(), Value::Integer(dt.year().into())),
                ("month".into(), Value::Integer(dt.month().into())),
                ("day".into(), Value::Integer(dt.day().into())),
                ("hour".into(), Value::Integer(dt.hour().into())),
                ("minute".into(), Value::Integer(dt.minute().into())),
                ("second".into(), Value::Integer(dt.second().into())),
                (
                    "microsecond".into(),
                    Value::Integer((dt.nanosecond() / 1000).into()),
                ),
                ("offset_seconds".into(), Value::Null),
                ("timezone_name".into(), Value::Null),
            ]))
        }
        TemporalKind::Datetime => {
            let dt = match wire {
                Some(Wire::UnixSec) => chrono::DateTime::from_timestamp(
                    value.as_integer().ok_or("expected Unix seconds integer")?,
                    0,
                )
                .map(|d| d.fixed_offset()),
                Some(Wire::UnixMs) => chrono::DateTime::from_timestamp_millis(
                    value
                        .as_integer()
                        .ok_or("expected Unix milliseconds integer")?,
                )
                .map(|d| d.fixed_offset()),
                _ => Some(
                    chrono::DateTime::parse_from_rfc3339(
                        value
                            .as_str()
                            .ok_or("datetime requires a declared encoding")?,
                    )
                    .map_err(|e| e.to_string())?,
                ),
            }
            .ok_or("datetime outside supported range")?;
            if dt.nanosecond() % 1000 != 0 {
                return Err("datetime precision exceeds Python microseconds".into());
            }
            Value::Object(indexmap::IndexMap::from([
                ("year".into(), Value::Integer((dt.year()).into())),
                ("month".into(), Value::Integer((dt.month()).into())),
                ("day".into(), Value::Integer((dt.day()).into())),
                ("hour".into(), Value::Integer((dt.hour()).into())),
                ("minute".into(), Value::Integer((dt.minute()).into())),
                ("second".into(), Value::Integer((dt.second()).into())),
                (
                    "microsecond".into(),
                    Value::Integer((dt.nanosecond() / 1000).into()),
                ),
                (
                    "offset_seconds".into(),
                    Value::Integer((dt.offset().local_minus_utc()).into()),
                ),
                ("timezone_name".into(), Value::Null),
            ]))
        }
        _ => return Err("expected typed temporal value".into()),
    };
    validate_components(&parsed, kind)?;
    Ok(parsed)
}

fn integer(c: &Value, name: &str, min: i64, max: i64) -> Result<i64, String> {
    c.get(name)
        .and_then(Value::as_integer)
        .filter(|n| (min..=max).contains(n))
        .ok_or_else(|| format!("invalid temporal {name}"))
}

pub fn validate_components(c: &Value, kind: TemporalKind) -> Result<(), String> {
    use TemporalKind::*;
    if !c.is_object() {
        return Err("expected temporal components".into());
    }
    let allowed: &[&str] = match kind {
        Date => &["year", "month", "day"],
        Datetime => &[
            "year",
            "month",
            "day",
            "hour",
            "minute",
            "second",
            "microsecond",
            "offset_seconds",
            "timezone_name",
        ],
        Time => &[
            "hour",
            "minute",
            "second",
            "microsecond",
            "offset_seconds",
            "timezone_name",
            "fold",
        ],
        Timedelta => &["days", "seconds", "microseconds"],
        Timezone => &["offset_seconds", "name"],
    };
    if c.as_object()
        .is_some_and(|fields| fields.keys().any(|name| !allowed.contains(&name.as_str())))
    {
        return Err("unsupported temporal component".into());
    }
    if matches!(kind, Date | Datetime) {
        chrono::NaiveDate::from_ymd_opt(
            integer(c, "year", 1, 9999)? as i32,
            integer(c, "month", 1, 12)? as u32,
            integer(c, "day", 1, 31)? as u32,
        )
        .ok_or("invalid calendar date")?;
    }
    if matches!(kind, Datetime | Time) {
        integer(c, "hour", 0, 23)?;
        integer(c, "minute", 0, 59)?;
        integer(c, "second", 0, 59)?;
        integer(c, "microsecond", 0, 999_999)?;
        if c.get("offset_seconds").is_some_and(|v| !v.is_null()) {
            integer(c, "offset_seconds", -86399, 86399)?;
        }
        if c.get("timezone_name")
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return Err("invalid timezone name".into());
        }
        if c.get("offset_seconds").is_none_or(Value::is_null)
            && c.get("timezone_name").is_some_and(|v| !v.is_null())
        {
            return Err("naive datetime cannot carry a timezone name".into());
        }
    }
    if kind == Time {
        integer(c, "fold", 0, 1)?;
    }
    if kind == Timedelta {
        integer(c, "days", -999_999_999, 999_999_999)?;
        integer(c, "seconds", 0, 86399)?;
        integer(c, "microseconds", 0, 999_999)?;
    }
    if kind == Timezone {
        integer(c, "offset_seconds", -86399, 86399)?;
        if c.get("name")
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return Err("invalid timezone name".into());
        }
    }
    Ok(())
}

pub fn encode(value: &Value, wire: Wire) -> Result<Value, String> {
    let kind = value
        .get("__plasm_temporal")
        .and_then(Value::as_str)
        .and_then(TemporalKind::parse)
        .ok_or("expected temporal value")?;
    let c = components(value, kind, None)?;
    let date = || {
        chrono::NaiveDate::from_ymd_opt(
            c.get("year")
                .unwrap_or(&Value::Null)
                .as_integer()
                .unwrap_or_default() as i32,
            c.get("month")
                .unwrap_or(&Value::Null)
                .as_unsigned()
                .unwrap_or_default() as u32,
            c.get("day")
                .unwrap_or(&Value::Null)
                .as_unsigned()
                .unwrap_or_default() as u32,
        )
        .ok_or("expected calendar date".to_string())
    };
    if wire == Wire::Iso8601Date {
        if kind != TemporalKind::Date {
            return Err("date input requires date, not datetime".into());
        }
        return Ok(Value::String(date()?.format("%Y-%m-%d").to_string()));
    }
    if kind != TemporalKind::Datetime {
        return Err("instant input requires datetime".into());
    }
    let naive = date()?
        .and_hms_micro_opt(
            c.get("hour")
                .unwrap_or(&Value::Null)
                .as_unsigned()
                .unwrap_or_default() as u32,
            c.get("minute")
                .unwrap_or(&Value::Null)
                .as_unsigned()
                .unwrap_or_default() as u32,
            c.get("second")
                .unwrap_or(&Value::Null)
                .as_unsigned()
                .unwrap_or_default() as u32,
            c.get("microsecond")
                .unwrap_or(&Value::Null)
                .as_unsigned()
                .unwrap_or_default() as u32,
        )
        .ok_or("invalid datetime")?;
    if wire == Wire::Iso8601NaiveDatetime {
        if c.get("offset_seconds")
            .is_some_and(|offset| !offset.is_null())
        {
            return Err("naive datetime transport cannot discard a timezone; use an explicitly naive datetime".into());
        }
        return Ok(Value::String(
            naive.format("%Y-%m-%dT%H:%M:%S%.f").to_string(),
        ));
    }
    let offset = chrono::FixedOffset::east_opt(
        c.get("offset_seconds")
            .unwrap_or(&Value::Null)
            .as_integer()
            .ok_or("instant input requires an aware datetime")? as i32,
    )
    .ok_or("invalid offset")?;
    let dt = naive
        .and_local_timezone(offset)
        .single()
        .ok_or("invalid datetime")?;
    Ok(match wire {
        Wire::Rfc3339 if offset.local_minus_utc() % 60 == 0 => Value::String(dt.to_rfc3339()),
        Wire::Rfc3339 => {
            return Err(
                "RFC3339 cannot encode offset seconds; convert with astimezone(timezone.utc)"
                    .into(),
            )
        }
        Wire::UnixSec if dt.timestamp_subsec_micros() == 0 => Value::Integer(dt.timestamp()),
        Wire::UnixMs if dt.timestamp_subsec_micros() % 1000 == 0 => {
            Value::Integer(dt.timestamp_millis())
        }
        _ => return Err("temporal wire encoding would lose precision".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_value as json;
    #[test]
    fn naive_datetime_wire_round_trips_without_inventing_or_discarding_timezone() {
        let wire = Wire::Iso8601NaiveDatetime;
        for text in ["2023-01-02T03:04:05", "2023-01-02T03:04:05.123456"] {
            let input = Value::String(text.into());
            let c = components(&input, TemporalKind::Datetime, Some(wire)).unwrap();
            assert!(c.get("offset_seconds").unwrap().is_null());
            let typed = tagged(TemporalKind::Datetime, c);
            assert_eq!(encode(&typed, wire).unwrap(), input);
            assert!(encode(&typed, Wire::Rfc3339).is_err());
        }
        for invalid in [
            "2023-01-02T03:04:05Z",
            "2023-01-02T03:04:05+01:00",
            "2023-02-30T03:04:05",
            "2023-01-02T03:04:05.123456789",
        ] {
            assert!(
                components(
                    &Value::String(invalid.into()),
                    TemporalKind::Datetime,
                    Some(wire)
                )
                .is_err(),
                "{invalid}"
            );
        }
        let aware = components(
            &Value::String("2023-01-02T03:04:05Z".into()),
            TemporalKind::Datetime,
            Some(Wire::Rfc3339),
        )
        .unwrap();
        let aware = tagged(TemporalKind::Datetime, aware);
        assert!(encode(&aware, wire).is_err());
        assert!(components(&aware, TemporalKind::Datetime, Some(wire)).is_err());
    }

    #[test]
    fn rfc3339_rejects_unrepresentable_offset_seconds() {
        let value = tagged(
            TemporalKind::Datetime,
            json!({"year":2024,"month":1,"day":1,"hour":0,"minute":0,"second":0,"microsecond":0,"offset_seconds":30}),
        );
        assert!(encode(&value, Wire::Rfc3339)
            .unwrap_err()
            .contains("offset seconds"));
        assert_eq!(encode(&value, Wire::UnixSec).unwrap(), json!(1704067170));
        let mut c = components(&value, TemporalKind::Datetime, None).unwrap();
        c.as_object_mut().unwrap().insert("fold".into(), json!(1));
        assert!(validate_components(&c, TemporalKind::Datetime).is_err());
    }

    #[test]
    fn temporal_wire_units_and_precision_are_explicit() {
        for (wire, raw, expected) in [
            (Wire::UnixSec, -1, -1),
            (Wire::UnixMs, -1, -1),
            (Wire::UnixMs, 1720000000123_i64, 1720000000123_i64),
        ] {
            let c = components(&json!(raw), TemporalKind::Datetime, Some(wire)).unwrap();
            let value = tagged(TemporalKind::Datetime, c);
            assert_eq!(encode(&value, wire).unwrap(), json!(expected));
        }
        let c = components(
            &json!("2024-02-29T12:34:56.123456+05:30"),
            TemporalKind::Datetime,
            Some(Wire::Rfc3339),
        )
        .unwrap();
        let value = tagged(TemporalKind::Datetime, c);
        assert_eq!(
            encode(&value, Wire::Rfc3339).unwrap(),
            json!("2024-02-29T12:34:56.123456+05:30")
        );
        assert!(encode(&value, Wire::UnixMs).is_err());
        assert!(encode(&value, Wire::UnixSec).is_err());
        assert!(encode(&value, Wire::Iso8601Date).is_err());
    }
    #[test]
    fn date_and_instant_are_distinct_and_naive_is_not_utc() {
        let value = tagged(
            TemporalKind::Date,
            components(
                &json!("2024-02-29"),
                TemporalKind::Date,
                Some(Wire::Iso8601Date),
            )
            .unwrap(),
        );
        assert_eq!(
            encode(&value, Wire::Iso8601Date).unwrap(),
            json!("2024-02-29")
        );
        assert!(encode(&value, Wire::Rfc3339).is_err());
        let naive = tagged(
            TemporalKind::Datetime,
            json!({"year":2024,"month":1,"day":1,"hour":0,"minute":0,"second":0,"microsecond":0,"offset_seconds":null,"timezone_name":null}),
        );
        assert!(components(&naive, TemporalKind::Datetime, None).is_ok());
        assert!(encode(&naive, Wire::Rfc3339).is_err());
    }
    #[test]
    fn invalid_temporal_values_fail_closed() {
        for text in [
            "2024-02-30T00:00:00Z",
            "2024-01-01T00:00:00.1234567Z",
            "2024-01-01T00:00:60Z",
            "2024-01-01T00:00:00",
        ] {
            assert!(
                components(&json!(text), TemporalKind::Datetime, Some(Wire::Rfc3339)).is_err(),
                "{text}"
            );
        }
        let wrong = tagged(TemporalKind::Date, json!({"year":2024,"month":1,"day":1}));
        assert!(components(&wrong, TemporalKind::Datetime, None).is_err());
    }
}
