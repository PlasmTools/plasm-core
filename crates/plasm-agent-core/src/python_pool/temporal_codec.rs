//! Direct native temporal components / Monty codec; no serialization intermediate.
use monty_types::*;
use plasm_core::{
    temporal_value::{self, TemporalKind as K},
    Value,
};

pub(super) fn to_monty(
    c: &Value,
    kind: K,
) -> Result<MontyObject, super::PythonValueConversionError> {
    temporal_value::validate_components(c, kind)?;
    let integer = |name: &'static str| {
        c.get(name)
            .and_then(Value::as_integer)
            .ok_or(super::PythonValueConversionError::MissingTemporalComponent { name })
    };
    let text = |name: &str| c.get(name).and_then(Value::as_str).map(str::to_owned);
    Ok(match kind {
        K::Date => MontyObject::date(MontyDate {
            year: integer("year")? as i32,
            month: integer("month")? as u8,
            day: integer("day")? as u8,
        }),
        K::Datetime => MontyObject::datetime(MontyDateTime {
            year: integer("year")? as i32,
            month: integer("month")? as u8,
            day: integer("day")? as u8,
            hour: integer("hour")? as u8,
            minute: integer("minute")? as u8,
            second: integer("second")? as u8,
            microsecond: integer("microsecond")? as u32,
            offset_seconds: c
                .get("offset_seconds")
                .and_then(Value::as_integer)
                .map(|v| v as i32),
            timezone_name: text("timezone_name"),
        }),
        K::Time => MontyObject::time(MontyTime {
            hour: integer("hour")? as u8,
            minute: integer("minute")? as u8,
            second: integer("second")? as u8,
            microsecond: integer("microsecond")? as u32,
            offset_seconds: c
                .get("offset_seconds")
                .and_then(Value::as_integer)
                .map(|v| v as i32),
            timezone_name: text("timezone_name"),
            fold: integer("fold")? as u8,
        }),
        K::Timedelta => MontyObject::timedelta(MontyTimeDelta {
            days: integer("days")? as i32,
            seconds: integer("seconds")? as i32,
            microseconds: integer("microseconds")? as i32,
        }),
        K::Timezone => MontyObject::timezone(MontyTimeZone {
            offset_seconds: integer("offset_seconds")? as i32,
            name: text("name"),
        }),
    })
}
pub(super) fn from_monty(
    value: ObjectRef<'_>,
) -> Result<Option<Value>, super::PythonValueConversionError> {
    use monty_types::unstable::{node, MontyNode};
    let (kind, components) = match node(value) {
        MontyNode::Date(v) => (
            K::Date,
            Value::Object(indexmap::IndexMap::from([
                ("year".into(), Value::Integer(v.year.into())),
                ("month".into(), Value::Integer(v.month.into())),
                ("day".into(), Value::Integer(v.day.into())),
            ])),
        ),
        MontyNode::DateTime(v) => (
            K::Datetime,
            Value::Object(indexmap::IndexMap::from([
                ("year".into(), Value::Integer(v.year.into())),
                ("month".into(), Value::Integer(v.month.into())),
                ("day".into(), Value::Integer(v.day.into())),
                ("hour".into(), Value::Integer(v.hour.into())),
                ("minute".into(), Value::Integer(v.minute.into())),
                ("second".into(), Value::Integer(v.second.into())),
                ("microsecond".into(), Value::Integer(v.microsecond.into())),
                (
                    "offset_seconds".into(),
                    v.offset_seconds
                        .map(|v| Value::Integer(v.into()))
                        .unwrap_or(Value::Null),
                ),
                (
                    "timezone_name".into(),
                    v.timezone_name
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
            ])),
        ),
        MontyNode::Time(v) => (
            K::Time,
            Value::Object(indexmap::IndexMap::from([
                ("hour".into(), Value::Integer(v.hour.into())),
                ("minute".into(), Value::Integer(v.minute.into())),
                ("second".into(), Value::Integer(v.second.into())),
                ("microsecond".into(), Value::Integer(v.microsecond.into())),
                (
                    "offset_seconds".into(),
                    v.offset_seconds
                        .map(|v| Value::Integer(v.into()))
                        .unwrap_or(Value::Null),
                ),
                (
                    "timezone_name".into(),
                    v.timezone_name
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                ("fold".into(), Value::Integer(v.fold.into())),
            ])),
        ),
        MontyNode::TimeDelta(v) => (
            K::Timedelta,
            Value::Object(indexmap::IndexMap::from([
                ("days".into(), Value::Integer(v.days.into())),
                ("seconds".into(), Value::Integer(v.seconds.into())),
                ("microseconds".into(), Value::Integer(v.microseconds.into())),
            ])),
        ),
        MontyNode::TimeZone(v) => (
            K::Timezone,
            Value::Object(indexmap::IndexMap::from([
                (
                    "offset_seconds".into(),
                    Value::Integer(v.offset_seconds.into()),
                ),
                (
                    "name".into(),
                    v.name.clone().map(Value::String).unwrap_or(Value::Null),
                ),
            ])),
        ),
        _ => return Ok(None),
    };
    temporal_value::validate_components(&components, kind)?;
    Ok(Some(temporal_value::tagged(kind, components)))
}
