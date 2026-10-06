//! Temporal transport codecs and host clock configuration.
//! Python datetime owns authored temporal computation. No relative phrases,
//! natural-language parser, magnitude-based Unix inference or syntax rewrites.
use crate::{TemporalWireFormat, Value};
use chrono::{DateTime, NaiveDateTime, Utc};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TemporalPatternError {
    #[error("datetime format must be a nonempty valid strftime layout")]
    InvalidPattern,
    #[error(transparent)]
    Value(#[from] crate::temporal_value::TemporalValueError),
    #[error("temporal wire representation is invalid")]
    InvalidWireRepresentation,
    #[error("datetime format cannot represent this temporal value")]
    Unrepresentable,
    #[error("datetime formatting failed")]
    Parse(#[source] std::sync::Arc<chrono::ParseError>),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TemporalNormalizationError {
    #[error(transparent)]
    Value(#[from] crate::temporal_value::TemporalValueError),
    #[error("Python date/datetime values are required for temporal computation")]
    PythonTemporalValueRequired {
        #[source]
        source: crate::temporal_value::TemporalValueError,
    },
    #[error("expected an encoded temporal scalar")]
    ExpectedEncodedScalar,
}

#[derive(Debug, Error)]
pub enum TemporalNowError {
    #[error("temporal clock setting must be an ISO datetime; naive values mean UTC")]
    InvalidClockFormat(#[source] chrono::ParseError),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TemporalWireFormatError {
    #[error("unknown temporal wire format '{name}'")]
    UnknownName { name: String },
}

/// Harness clock configuration. A naive ISO datetime explicitly means UTC here,
/// not in authored Python values or API argument coercion.
pub fn parse_temporal_now_env(raw: &str) -> Result<DateTime<Utc>, TemporalNowError> {
    let raw = raw.trim();
    if let Ok(value) = DateTime::parse_from_rfc3339(raw) {
        return Ok(value.with_timezone(&Utc));
    }
    let mut last_parse_error = None;
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        match NaiveDateTime::parse_from_str(raw, format) {
            Ok(value) => return Ok(value.and_utc()),
            Err(error) => last_parse_error = Some(error),
        }
    }
    Err(TemporalNowError::InvalidClockFormat(
        last_parse_error.expect("at least one temporal format is attempted"),
    ))
}

pub fn temporal_reference_now() -> Result<DateTime<Utc>, TemporalNowError> {
    match std::env::var("PLASM_TEMPORAL_NOW") {
        Ok(raw) if !raw.trim().is_empty() => parse_temporal_now_env(&raw),
        _ => Ok(Utc::now()),
    }
}

/// Validate an explicit wire scalar or encode a typed Python temporal value.
pub fn normalize_temporal_value(
    value: Value,
    wire: TemporalWireFormat,
) -> Result<Value, TemporalNormalizationError> {
    use crate::temporal_value::{components, encode, tagged, TemporalKind};
    let json = value;
    let result = if json.get("__plasm_temporal").is_some() {
        encode(&json, wire)?
    } else {
        let kind = if wire == TemporalWireFormat::Iso8601Date {
            TemporalKind::Date
        } else {
            TemporalKind::Datetime
        };
        // An explicit ISO instant has its own encoding; integer units come only
        // from the declared destination profile, never from magnitude.
        let source_wire = if json.is_string()
            && kind == TemporalKind::Datetime
            && wire != TemporalWireFormat::Iso8601NaiveDatetime
        {
            TemporalWireFormat::Rfc3339
        } else {
            wire
        };
        let c = components(&json, kind, Some(source_wire))
            .map_err(|source| TemporalNormalizationError::PythonTemporalValueRequired { source })?;
        encode(&tagged(kind, c), wire)?
    };
    Ok(result)
}

pub fn temporal_encoded_as_wire_string(
    encoded: &Value,
) -> Result<String, TemporalNormalizationError> {
    match encoded {
        Value::String(value) => Ok(value.clone()),
        Value::Integer(value) => Ok(value.to_string()),
        _ => Err(TemporalNormalizationError::ExpectedEncodedScalar),
    }
}

pub fn wire_temporal_value(
    value: Value,
    wire: TemporalWireFormat,
) -> Result<Value, TemporalNormalizationError> {
    let encoded = normalize_temporal_value(value, wire)?;
    Ok(Value::String(temporal_encoded_as_wire_string(&encoded)?))
}

/// Validated temporal layout used only at a transport boundary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TemporalPattern(String);

impl TryFrom<String> for TemporalPattern {
    type Error = TemporalPatternError;
    fn try_from(pattern: String) -> Result<Self, Self::Error> {
        if pattern.is_empty()
            || chrono::format::StrftimeItems::new(&pattern)
                .any(|item| matches!(item, chrono::format::Item::Error))
        {
            return Err(TemporalPatternError::InvalidPattern);
        }
        Ok(Self(pattern))
    }
}

impl From<TemporalPattern> for String {
    fn from(pattern: TemporalPattern) -> Self {
        pattern.0
    }
}

impl TemporalPattern {
    /// Format a declared temporal encoding. Calendar values remain calendar values;
    /// instant encodings use UTC. No clock lookup or encoding inference occurs here.
    pub fn encode(
        &self,
        value: Value,
        wire: TemporalWireFormat,
    ) -> Result<Value, TemporalPatternError> {
        use crate::temporal_value::{components, encode, tagged, TemporalKind};
        if value.is_null() {
            return Ok(Value::Null);
        }
        let kind = if wire == TemporalWireFormat::Iso8601Date {
            TemporalKind::Date
        } else {
            TemporalKind::Datetime
        };
        let typed = tagged(kind, components(&value, kind, Some(wire))?);
        let render = |formatted: &dyn std::fmt::Display| {
            use std::fmt::Write;
            let mut text = String::new();
            write!(&mut text, "{formatted}").map_err(|_| TemporalPatternError::Unrepresentable)?;
            Ok(Value::String(text))
        };
        match wire {
            TemporalWireFormat::Iso8601NaiveDatetime => {
                let raw = encode(&typed, wire)?;
                let dt = NaiveDateTime::parse_from_str(
                    raw.as_str()
                        .ok_or(TemporalPatternError::InvalidWireRepresentation)?,
                    "%Y-%m-%dT%H:%M:%S%.f",
                )
                .map_err(|e| TemporalPatternError::Parse(std::sync::Arc::new(e)))?;
                render(&dt.format(&self.0))
            }
            TemporalWireFormat::Iso8601Date => {
                let raw = encode(&typed, wire)?;
                let date = chrono::NaiveDate::parse_from_str(
                    raw.as_str()
                        .ok_or(TemporalPatternError::InvalidWireRepresentation)?,
                    "%Y-%m-%d",
                )
                .map_err(|e| TemporalPatternError::Parse(std::sync::Arc::new(e)))?;
                render(&date.format(&self.0))
            }
            TemporalWireFormat::Rfc3339
            | TemporalWireFormat::UnixMs
            | TemporalWireFormat::UnixSec => {
                let raw = encode(&typed, TemporalWireFormat::Rfc3339)?;
                let instant = DateTime::parse_from_rfc3339(
                    raw.as_str()
                        .ok_or(TemporalPatternError::InvalidWireRepresentation)?,
                )
                .map_err(|e| TemporalPatternError::Parse(std::sync::Arc::new(e)))?;
                render(&instant.with_timezone(&Utc).format(&self.0))
            }
        }
    }
}

/// Parse a wire-format name (`unix_ms`, `rfc3339`, …) for view templates and filters.
pub fn temporal_wire_format_from_name(
    name: &str,
) -> Result<TemporalWireFormat, TemporalWireFormatError> {
    match name.trim().to_ascii_lowercase().as_str() {
        "rfc3339" => Ok(TemporalWireFormat::Rfc3339),
        "unix_ms" => Ok(TemporalWireFormat::UnixMs),
        "unix_sec" => Ok(TemporalWireFormat::UnixSec),
        "iso8601_naive_datetime" => Ok(TemporalWireFormat::Iso8601NaiveDatetime),
        "iso8601_date" => Ok(TemporalWireFormat::Iso8601Date),
        other => Err(TemporalWireFormatError::UnknownName {
            name: other.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retired_temporal_spellings_are_not_a_second_language() {
        for text in [
            "now",
            "today",
            "yesterday",
            "7d ago",
            "next week",
            "now-1h",
            "now() - 7d",
            "1720000000123",
        ] {
            for wire in [
                TemporalWireFormat::Rfc3339,
                TemporalWireFormat::UnixMs,
                TemporalWireFormat::UnixSec,
                TemporalWireFormat::Iso8601Date,
            ] {
                assert!(
                    normalize_temporal_value(Value::String(text.into()), wire).is_err(),
                    "{text}"
                );
                assert!(
                    wire_temporal_value(Value::String(text.into()), wire).is_err(),
                    "{text}"
                );
            }
        }
    }
    #[test]
    fn transport_units_are_declared_not_inferred() {
        assert_eq!(
            normalize_temporal_value(Value::Integer(-1), TemporalWireFormat::UnixMs).unwrap(),
            Value::Integer(-1)
        );
        assert_eq!(
            normalize_temporal_value(Value::Integer(-1), TemporalWireFormat::UnixSec).unwrap(),
            Value::Integer(-1)
        );
        assert!(
            normalize_temporal_value(Value::Integer(1720000000), TemporalWireFormat::Rfc3339)
                .is_err()
        );
        assert_eq!(
            normalize_temporal_value(
                Value::String("2024-01-01T00:00:00Z".into()),
                TemporalWireFormat::UnixMs
            )
            .unwrap(),
            Value::Integer(1704067200000)
        );
    }
    #[test]
    fn harness_clock_is_explicit_absolute_configuration() {
        let expected = parse_temporal_now_env("2024-01-01T00:00:00.123456Z").unwrap();
        assert_eq!(
            parse_temporal_now_env("2024-01-01 00:00:00.123456").unwrap(),
            expected
        );
        let error = parse_temporal_now_env("today").unwrap_err();
        assert!(matches!(error, TemporalNowError::InvalidClockFormat(_)));
        assert!(std::error::Error::source(&error).is_some());
        assert!(matches!(
            parse_temporal_now_env("1720000000123"),
            Err(TemporalNowError::InvalidClockFormat(_))
        ));
    }

    #[test]
    fn temporal_wire_format_parser_returns_a_semantic_error() {
        assert_eq!(
            temporal_wire_format_from_name("unix_ms").unwrap(),
            TemporalWireFormat::UnixMs
        );
        assert_eq!(
            temporal_wire_format_from_name("  MADE_UP  "),
            Err(TemporalWireFormatError::UnknownName {
                name: "made_up".to_owned()
            })
        );
    }
}
