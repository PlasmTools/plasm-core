//! Fowler money: exact decimal amount plus optional currency.
//!
//! Wire encoding stays a scalar (decimal string, JSON number, or integer minor units).
//! Currency is CGS-side — fixed on a `values:` row or attached from a sibling field.

use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

use crate::value::Value;

/// `value_format` payload for money (see [`crate::ValueWireFormat::Money`]).
///
/// Scale exists only on [`Self::MinorUnits`]. Catalog YAML `{ money, scale }` is
/// decoded at the [`crate::ValueWireFormat`] boundary; this type cannot represent
/// `scale` on `decimal_string` / `json_number`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MoneyWireFormat {
    DecimalString,
    JsonNumber,
    MinorUnits { scale: u8 },
}

impl MoneyWireFormat {
    #[must_use]
    pub fn decimal_string() -> Self {
        Self::DecimalString
    }

    #[must_use]
    pub fn json_number() -> Self {
        Self::JsonNumber
    }

    pub fn minor_units(scale: u8) -> Result<Self, MoneyError> {
        if scale > 28 {
            return Err(MoneyError::ScaleTooLarge);
        }
        Ok(Self::MinorUnits { scale })
    }

    /// Lift catalog YAML/JSON `{ money, scale }` into a valid format.
    pub(crate) fn from_catalog_parts(
        encoding: &str,
        scale: Option<u8>,
    ) -> Result<Self, MoneyError> {
        match encoding {
            "decimal_string" => {
                if scale.is_some() {
                    Err(MoneyError::ScaleOnlyForMinorUnits)
                } else {
                    Ok(Self::DecimalString)
                }
            }
            "json_number" => {
                if scale.is_some() {
                    Err(MoneyError::ScaleOnlyForMinorUnits)
                } else {
                    Ok(Self::JsonNumber)
                }
            }
            "minor_units" => match scale {
                Some(s) => Self::minor_units(s),
                None => Err(MoneyError::ScaleRequired),
            },
            other => Err(MoneyError::UnknownEncoding {
                tag: other.to_string(),
            }),
        }
    }

    pub(crate) fn encode_amount(self, amount: Decimal) -> Result<serde_json::Value, MoneyError> {
        match self {
            Self::DecimalString => Ok(serde_json::Value::String(amount.to_string())),
            Self::JsonNumber => decimal_to_json_number(amount),
            Self::MinorUnits { scale } => {
                let factor = ten_pow_scale(u32::from(scale))?;
                let units = (amount * factor).round_dp(0);
                let i = decimal_to_i64(units)?;
                Ok(serde_json::Value::Number(i.into()))
            }
        }
    }
}

/// Serde shape `{ "encoding": "…", "scale": N }` for tagged money / compiled decoders.
#[derive(Serialize, Deserialize)]
struct MoneyWireFormatSerde {
    encoding: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scale: Option<u8>,
}

impl Serialize for MoneyWireFormat {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = match *self {
            Self::DecimalString => MoneyWireFormatSerde {
                encoding: "decimal_string".into(),
                scale: None,
            },
            Self::JsonNumber => MoneyWireFormatSerde {
                encoding: "json_number".into(),
                scale: None,
            },
            Self::MinorUnits { scale } => MoneyWireFormatSerde {
                encoding: "minor_units".into(),
                scale: Some(scale),
            },
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MoneyWireFormat {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = MoneyWireFormatSerde::deserialize(deserializer)?;
        Self::from_catalog_parts(&wire.encoding, wire.scale).map_err(serde::de::Error::custom)
    }
}

/// Decode-time coerce spec for one money field (amount format + optional sibling currency).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoneyDecodeSpec {
    format: MoneyWireFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_currency: Option<CurrencyCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    currency_field: Option<String>,
}

impl MoneyDecodeSpec {
    #[must_use]
    pub fn new(
        format: MoneyWireFormat,
        default_currency: Option<String>,
        currency_field: Option<String>,
    ) -> Self {
        Self {
            format,
            default_currency: CurrencyCode::parse_opt(default_currency),
            currency_field: currency_field.filter(|s| !s.is_empty()),
        }
    }

    #[must_use]
    pub fn format(&self) -> MoneyWireFormat {
        self.format
    }

    #[must_use]
    pub fn default_currency(&self) -> Option<&str> {
        self.default_currency.as_ref().map(CurrencyCode::as_str)
    }

    #[must_use]
    pub fn currency_field(&self) -> Option<&str> {
        self.currency_field.as_deref()
    }
}

/// ISO-ish unit label. Absence on [`MoneyValue`] means the unit is unknown (Fowler).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
struct CurrencyCode(String);

impl CurrencyCode {
    fn parse(s: &str) -> Option<Self> {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(Self(t.to_string()))
        }
    }

    fn parse_opt(s: Option<String>) -> Option<Self> {
        s.and_then(|s| Self::parse(&s))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for CurrencyCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).ok_or_else(|| serde::de::Error::custom("currency code must be non-empty"))
    }
}

fn deserialize_opt_currency<'de, D>(deserializer: D) -> Result<Option<CurrencyCode>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = Option::<String>::deserialize(deserializer)?;
    Ok(s.and_then(|s| CurrencyCode::parse(&s)))
}

/// Runtime money value. Serde uses a tagged object so untagged [`Value`] does not collide with [`Value::Object`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoneyValue {
    #[serde(rename = "__plasm_money", with = "rust_decimal::serde::str")]
    amount: Decimal,
    #[serde(
        default,
        deserialize_with = "deserialize_opt_currency",
        skip_serializing_if = "Option::is_none"
    )]
    currency: Option<CurrencyCode>,
    /// Wire encoding stamped at coerce time so HTTP emit does not need the CGS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    format: Option<MoneyWireFormat>,
}

impl PartialEq for MoneyValue {
    fn eq(&self, other: &Self) -> bool {
        self.amount == other.amount
            && currencies_eq_opt(
                self.currency.as_ref().map(CurrencyCode::as_str),
                other.currency.as_ref().map(CurrencyCode::as_str),
            )
    }
}

impl MoneyValue {
    #[must_use]
    pub fn new(amount: Decimal, currency: Option<String>) -> Self {
        Self {
            amount: normalize_decimal(amount),
            currency: CurrencyCode::parse_opt(currency),
            format: None,
        }
    }

    #[must_use]
    pub fn amount(&self) -> Decimal {
        self.amount
    }

    #[must_use]
    pub fn currency(&self) -> Option<&str> {
        self.currency.as_ref().map(CurrencyCode::as_str)
    }

    #[must_use]
    pub fn with_format(mut self, format: MoneyWireFormat) -> Self {
        self.format = Some(format);
        self
    }

    pub(crate) fn attach_currency_if_absent(&mut self, currency: Option<&str>) {
        if self.currency.is_some() {
            return;
        }
        if let Some(c) = currency.and_then(CurrencyCode::parse) {
            self.currency = Some(c);
        }
    }

    /// HTTP/JSON scalar using the stamped format. Missing format is an error (no decimal-string default).
    pub fn encode_stored(&self) -> Result<serde_json::Value, MoneyError> {
        let format = self.format.ok_or(MoneyError::UnstampedFormat)?;
        format.encode_amount(self.amount)
    }

    /// Form / multipart / key-slot text for the same scalar [`Self::encode_stored`] would emit.
    pub fn to_wire_text(&self) -> Result<String, MoneyError> {
        match self.encode_stored()? {
            serde_json::Value::String(s) => Ok(s),
            serde_json::Value::Number(n) => Ok(n.to_string()),
            other => Err(MoneyError::UnexpectedWireScalar {
                got: other.to_string(),
            }),
        }
    }

    #[must_use]
    pub fn display(&self) -> String {
        match self.currency() {
            Some(c) => format!("{} {c}", self.amount),
            None => self.amount.to_string(),
        }
    }
}

/// Parse a program/wire token into [`Value::Money`].
pub fn normalize(
    val: Value,
    format: MoneyWireFormat,
    default_currency: Option<&str>,
) -> Result<Value, MoneyError> {
    if val.is_domain_example_placeholder() {
        return Ok(val);
    }
    match val {
        Value::Money(m) => {
            let mut m = m;
            m.attach_currency_if_absent(default_currency);
            Ok(Value::Money(m.with_format(format)))
        }
        Value::PlasmInputRef(_) => Ok(val),
        other => {
            let (amount, from_obj_ccy) = parse_amount_and_optional_currency(&other, format)?;
            let currency = from_obj_ccy.or_else(|| default_currency.map(str::to_string));
            Ok(Value::Money(
                MoneyValue::new(amount, currency).with_format(format),
            ))
        }
    }
}

/// Both currencies present and unequal (compare is illegal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossCurrencyError {
    left: String,
    right: String,
}

impl CrossCurrencyError {
    #[must_use]
    pub fn left(&self) -> &str {
        &self.left
    }

    #[must_use]
    pub fn right(&self) -> &str {
        &self.right
    }
}

/// Failures while parsing, stamping, or encoding money.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MoneyError {
    #[error("unknown money encoding `{tag}`")]
    UnknownEncoding { tag: String },
    #[error("money minor_units scale must be ≤ 28")]
    ScaleTooLarge,
    #[error("money value_format minor_units requires `scale`")]
    ScaleRequired,
    #[error("money value_format `scale` is only valid with money: minor_units")]
    ScaleOnlyForMinorUnits,
    #[error("money value missing stamped wire format; cannot encode")]
    UnstampedFormat,
    #[error("empty money amount")]
    EmptyAmount,
    #[error("invalid minor-units money `{token}`")]
    InvalidMinorUnits { token: String },
    #[error("invalid decimal money `{token}`")]
    InvalidDecimal { token: String },
    #[error("money amount must be finite")]
    NonFiniteAmount,
    #[error("minor-units money must be a whole number")]
    MinorUnitsNotWhole,
    #[error("cannot represent float as money")]
    FloatUnrepresentable,
    #[error("money object requires `amount`")]
    ObjectMissingAmount,
    #[error("cannot coerce {type_name} to money")]
    CannotCoerce { type_name: &'static str },
    #[error("money amount cannot be {type_name}")]
    AmountWrongType { type_name: &'static str },
    #[error("money minor_units 10^{scale} overflow")]
    ScaleOverflow { scale: u32 },
    #[error("money minor units overflow `{amount}`")]
    MinorUnitsOverflow { amount: String },
    #[error("money wire scalar must be a JSON string or number, got {got}")]
    UnexpectedWireScalar { got: String },
    #[error("currency_field `{field}` must be a string, got {got}")]
    CurrencyFieldNotString { field: String, got: String },
}

impl From<MoneyError> for String {
    fn from(e: MoneyError) -> Self {
        e.to_string()
    }
}

/// Error only when both currencies are present and differ.
pub(crate) fn currency_conflict(
    left: Option<&str>,
    right: Option<&str>,
) -> Result<(), CrossCurrencyError> {
    match (left, right) {
        (Some(a), Some(b)) if !currency_eq(a, b) => Err(CrossCurrencyError {
            left: a.to_string(),
            right: b.to_string(),
        }),
        _ => Ok(()),
    }
}

/// Compare two money values. Error only when both currencies are present and differ.
pub(crate) fn try_cmp(
    left: &MoneyValue,
    right: &MoneyValue,
) -> Result<Ordering, CrossCurrencyError> {
    currency_conflict(left.currency(), right.currency())?;
    Ok(left.amount().cmp(&right.amount()))
}

/// Ordered compare when at least one side is already [`Value::Money`].
///
/// The non-money side is parsed as a **major-unit** decimal (program literals), never as
/// `minor_units`. `Ok(None)` means this is not a money compare — callers use `==` / numeric.
pub fn try_cmp_values(left: &Value, right: &Value) -> Result<Option<Ordering>, CrossCurrencyError> {
    match (left, right) {
        (Value::Money(a), Value::Money(b)) => try_cmp(a, b).map(Some),
        (Value::Money(a), other) => match parse_predicate_literal(other) {
            Some(b) => try_cmp(a, &b).map(Some),
            None => Ok(None),
        },
        (other, Value::Money(b)) => match parse_predicate_literal(other) {
            Some(a) => try_cmp(&a, b).map(Some),
            None => Ok(None),
        },
        _ => Ok(None),
    }
}

/// Equality for predicates: money compare only when a side is [`Value::Money`], else `==`.
pub fn values_eq(left: &Value, right: &Value) -> Result<bool, CrossCurrencyError> {
    match try_cmp_values(left, right)? {
        Some(ord) => Ok(ord.is_eq()),
        None => Ok(left == right),
    }
}

/// Ordered compare for predicates: money when a side is [`Value::Money`], else numeric.
pub fn values_ord(left: &Value, right: &Value) -> Result<Option<Ordering>, CrossCurrencyError> {
    match try_cmp_values(left, right)? {
        Some(ord) => Ok(Some(ord)),
        None => Ok(match (left.as_number(), right.as_number()) {
            (Some(a), Some(b)) => a.partial_cmp(&b),
            _ => None,
        }),
    }
}

/// Coerce money fields on a decoded entity row, then attach sibling currency when still absent.
pub fn coerce_decoded_fields(
    fields: &mut indexmap::IndexMap<String, Value>,
    specs: impl IntoIterator<Item = (String, MoneyDecodeSpec)>,
) -> Result<(), MoneyError> {
    for (field, spec) in specs {
        let Some(raw) = fields.get(&field).cloned() else {
            continue;
        };
        if matches!(raw, Value::Null) {
            continue;
        }
        let mut coerced = normalize(raw, spec.format(), spec.default_currency())?;
        if let Value::Money(ref mut m) = coerced {
            if let Some(cf) = spec.currency_field() {
                match fields.get(cf) {
                    None | Some(Value::Null) => {}
                    Some(sibling) => {
                        let Some(s) = sibling.as_str() else {
                            return Err(MoneyError::CurrencyFieldNotString {
                                field: cf.to_string(),
                                got: sibling.type_name().to_string(),
                            });
                        };
                        m.attach_currency_if_absent(Some(s));
                    }
                }
            }
        }
        fields.insert(field, coerced);
    }
    Ok(())
}

/// Lift JSON so money coerce sees lexical decimal digits, not `f64`.
pub fn json_amount_to_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else {
                Value::String(n.to_string())
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        other => crate::json_value_to_plasm_value(other),
    }
}

/// Lift a tagged JSON object (`__plasm_money`) into [`Value::Money`].
pub(crate) fn try_from_json_object(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<MoneyValue> {
    let amount_v = obj.get("__plasm_money")?;
    let amount = match amount_v {
        serde_json::Value::String(s) => Decimal::from_str(s).ok()?,
        serde_json::Value::Number(n) => Decimal::from_str(&n.to_string()).ok()?,
        _ => return None,
    };
    let currency = obj
        .get("currency")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let format = obj
        .get("format")
        .and_then(|v| serde_json::from_value::<MoneyWireFormat>(v.clone()).ok());
    let mut m = MoneyValue::new(amount, currency);
    if let Some(fmt) = format {
        m = m.with_format(fmt);
    }
    Some(m)
}

fn parse_predicate_literal(v: &Value) -> Option<MoneyValue> {
    parse_amount_and_optional_currency(v, MoneyWireFormat::decimal_string())
        .ok()
        .map(|(amount, currency)| MoneyValue::new(amount, currency))
}

fn parse_amount_and_optional_currency(
    val: &Value,
    format: MoneyWireFormat,
) -> Result<(Decimal, Option<String>), MoneyError> {
    match val {
        Value::String(s) | Value::PhraseIdent(s) => Ok((parse_decimal_token(s, format)?, None)),
        Value::Integer(i) => Ok((integer_to_amount(*i, format)?, None)),
        Value::Float(f) => Ok((float_to_amount(*f, format)?, None)),
        Value::Object(map) => {
            let amount_v = map
                .get("amount")
                .or_else(|| map.get("__plasm_money"))
                .ok_or(MoneyError::ObjectMissingAmount)?;
            let amount = parse_amount_leaf(amount_v, format)?;
            let currency = map
                .get("currency")
                .and_then(Value::as_str)
                .map(str::to_string);
            Ok((amount, currency))
        }
        other => Err(MoneyError::CannotCoerce {
            type_name: other.type_name(),
        }),
    }
}

fn parse_amount_leaf(val: &Value, format: MoneyWireFormat) -> Result<Decimal, MoneyError> {
    match val {
        Value::String(s) | Value::PhraseIdent(s) => parse_decimal_token(s, format),
        Value::Integer(i) => integer_to_amount(*i, format),
        Value::Float(f) => float_to_amount(*f, format),
        Value::Money(m) => Ok(m.amount()),
        other => Err(MoneyError::AmountWrongType {
            type_name: other.type_name(),
        }),
    }
}

fn parse_decimal_token(s: &str, format: MoneyWireFormat) -> Result<Decimal, MoneyError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(MoneyError::EmptyAmount);
    }
    match format {
        MoneyWireFormat::MinorUnits { .. } => {
            let i = t
                .parse::<i64>()
                .map_err(|_| MoneyError::InvalidMinorUnits {
                    token: t.to_string(),
                })?;
            integer_to_amount(i, format)
        }
        MoneyWireFormat::DecimalString | MoneyWireFormat::JsonNumber => Decimal::from_str(t)
            .map(normalize_decimal)
            .map_err(|_| MoneyError::InvalidDecimal {
                token: t.to_string(),
            }),
    }
}

fn integer_to_amount(i: i64, format: MoneyWireFormat) -> Result<Decimal, MoneyError> {
    match format {
        MoneyWireFormat::MinorUnits { scale } => {
            let factor = ten_pow_scale(u32::from(scale))?;
            Ok(normalize_decimal(Decimal::from(i) / factor))
        }
        MoneyWireFormat::DecimalString | MoneyWireFormat::JsonNumber => Ok(Decimal::from(i)),
    }
}

fn float_to_amount(f: f64, format: MoneyWireFormat) -> Result<Decimal, MoneyError> {
    if !f.is_finite() {
        return Err(MoneyError::NonFiniteAmount);
    }
    match format {
        MoneyWireFormat::MinorUnits { .. } => {
            if f.fract() != 0.0 {
                return Err(MoneyError::MinorUnitsNotWhole);
            }
            integer_to_amount(f as i64, format)
        }
        MoneyWireFormat::DecimalString | MoneyWireFormat::JsonNumber => Decimal::from_f64_retain(f)
            .map(normalize_decimal)
            .ok_or(MoneyError::FloatUnrepresentable),
    }
}

fn ten_pow_scale(scale: u32) -> Result<Decimal, MoneyError> {
    let mut v = Decimal::ONE;
    let ten = Decimal::TEN;
    for _ in 0..scale {
        v = v
            .checked_mul(ten)
            .ok_or(MoneyError::ScaleOverflow { scale })?;
    }
    Ok(v)
}

fn decimal_to_json_number(d: Decimal) -> Result<serde_json::Value, MoneyError> {
    let s = d.normalize().to_string();
    match serde_json::Number::from_str(&s) {
        Ok(n) => Ok(serde_json::Value::Number(n)),
        Err(_) => Ok(serde_json::Value::String(s)),
    }
}

fn decimal_to_i64(d: Decimal) -> Result<i64, MoneyError> {
    d.to_string()
        .parse::<i64>()
        .map_err(|_| MoneyError::MinorUnitsOverflow {
            amount: d.to_string(),
        })
}

fn normalize_decimal(mut d: Decimal) -> Decimal {
    d.normalize_assign();
    d
}

fn currencies_eq_opt(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => currency_eq(x, y),
        (None, None) => true,
        _ => false,
    }
}

fn currency_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

impl fmt::Display for CrossCurrencyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Cannot compare money in '{}' with money in '{}'",
            self.left, self.right
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_string_round_trip() {
        let v = normalize(
            Value::String("5234.50".into()),
            MoneyWireFormat::decimal_string(),
            Some("USD"),
        )
        .unwrap();
        let Value::Money(m) = v else {
            panic!("expected money")
        };
        assert_eq!(m.display(), "5234.5 USD");
        let wire = m.encode_stored().unwrap();
        assert_eq!(wire, serde_json::Value::String("5234.5".into()));
    }

    #[test]
    fn json_number_and_float_input() {
        let v = normalize(
            Value::Float(12.5),
            MoneyWireFormat::json_number(),
            Some("USD"),
        )
        .unwrap();
        let Value::Money(m) = &v else {
            panic!("expected money")
        };
        let wire = m.encode_stored().unwrap();
        assert!(wire.is_number());
    }

    #[test]
    fn minor_units_scale_2() {
        let fmt = MoneyWireFormat::minor_units(2).unwrap();
        let v = normalize(Value::Integer(1050), fmt, Some("USD")).unwrap();
        let Value::Money(m) = v else {
            panic!("expected money")
        };
        assert_eq!(m.amount().to_string(), "10.5");
        let wire = m.encode_stored().unwrap();
        assert_eq!(wire, serde_json::json!(1050));
    }

    #[test]
    fn compare_same_and_missing_currency() {
        let a = MoneyValue::new(Decimal::from_str("1.5").unwrap(), Some("USD".into()));
        let b = MoneyValue::new(Decimal::from_str("2").unwrap(), None);
        assert_eq!(try_cmp(&a, &b).unwrap(), Ordering::Less);
        let c = MoneyValue::new(Decimal::from_str("1.5").unwrap(), Some("EUR".into()));
        let err = try_cmp(&a, &c).unwrap_err();
        assert_eq!(err.left(), "USD");
        assert_eq!(err.right(), "EUR");
    }

    #[test]
    fn object_amount_currency_input() {
        let mut map = indexmap::IndexMap::new();
        map.insert("amount".into(), Value::String("10".into()));
        map.insert("currency".into(), Value::String("USD".into()));
        let v = normalize(Value::Object(map), MoneyWireFormat::decimal_string(), None).unwrap();
        let Value::Money(m) = v else {
            panic!("expected money")
        };
        assert_eq!(m.currency(), Some("USD"));
    }

    #[test]
    fn minor_units_requires_scale() {
        let err = serde_json::from_value::<MoneyWireFormat>(serde_json::json!({
            "encoding": "minor_units"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("scale"));
    }

    #[test]
    fn scale_rejected_on_decimal_string_wire() {
        let err = serde_json::from_value::<MoneyWireFormat>(serde_json::json!({
            "encoding": "decimal_string",
            "scale": 2
        }))
        .unwrap_err();
        assert!(err.to_string().contains("minor_units"));
    }

    #[test]
    fn minor_units_rejects_scale_above_28() {
        assert!(matches!(
            MoneyWireFormat::minor_units(29),
            Err(MoneyError::ScaleTooLarge)
        ));
    }

    #[test]
    fn sibling_currency_attaches_when_amount_has_none() {
        let mut fields = indexmap::IndexMap::new();
        fields.insert("price".into(), Value::String("10.5".into()));
        fields.insert("quote_currency".into(), Value::String("USD".into()));
        coerce_decoded_fields(
            &mut fields,
            [(
                "price".into(),
                MoneyDecodeSpec::new(
                    MoneyWireFormat::decimal_string(),
                    None,
                    Some("quote_currency".into()),
                ),
            )],
        )
        .unwrap();
        let Value::Money(m) = fields.get("price").unwrap() else {
            panic!("expected money");
        };
        assert_eq!(m.currency(), Some("USD"));
    }

    #[test]
    fn json_number_nineteen_ninety_nine_is_exact_decimal() {
        let raw: serde_json::Value = serde_json::from_str("19.99").unwrap();
        let v = normalize(
            json_amount_to_value(&raw),
            MoneyWireFormat::json_number(),
            Some("USD"),
        )
        .unwrap();
        let Value::Money(m) = v else {
            panic!("expected money")
        };
        assert_eq!(m.amount().to_string(), "19.99");
    }

    #[test]
    fn minor_units_scale_18_does_not_panic() {
        let fmt = MoneyWireFormat::minor_units(18).unwrap();
        let v = normalize(Value::Integer(1), fmt, Some("ETH")).unwrap();
        let Value::Money(m) = v else {
            panic!("expected money")
        };
        assert_eq!(m.amount().to_string(), "0.000000000000000001");
        let wire = m.encode_stored().unwrap();
        assert_eq!(wire, serde_json::json!(1));
    }

    #[test]
    fn encode_stored_requires_stamped_format() {
        let m = MoneyValue::new(Decimal::from_str("1.5").unwrap(), Some("USD".into()));
        assert!(matches!(
            m.encode_stored(),
            Err(MoneyError::UnstampedFormat)
        ));
        assert!(m
            .encode_stored()
            .unwrap_err()
            .to_string()
            .contains("stamped wire format"));
    }

    #[test]
    fn string_equality_is_not_money_compare() {
        let a = Value::String("1.50".into());
        let b = Value::String("1.5".into());
        assert!(!values_eq(&a, &b).unwrap());
        assert!(
            values_ord(&Value::String("100".into()), &Value::String("20".into()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn neq_does_not_succeed_on_cross_currency() {
        let a = Value::Money(MoneyValue::new(
            Decimal::from_str("1").unwrap(),
            Some("USD".into()),
        ));
        let b = Value::Money(MoneyValue::new(
            Decimal::from_str("1").unwrap(),
            Some("EUR".into()),
        ));
        assert!(values_eq(&a, &b).is_err());
        assert!(values_ord(&a, &b).is_err());
    }

    #[test]
    fn sibling_currency_rejects_non_string() {
        let mut fields = indexmap::IndexMap::new();
        fields.insert("price".into(), Value::String("10.5".into()));
        fields.insert("quote_currency".into(), Value::Integer(1));
        let err = coerce_decoded_fields(
            &mut fields,
            [(
                "price".into(),
                MoneyDecodeSpec::new(
                    MoneyWireFormat::decimal_string(),
                    None,
                    Some("quote_currency".into()),
                ),
            )],
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("quote_currency") && msg.contains("string"));
    }

    #[test]
    fn currency_eq_is_case_insensitive() {
        let a = MoneyValue::new(Decimal::from_str("1").unwrap(), Some("usd".into()));
        let b = MoneyValue::new(Decimal::from_str("1").unwrap(), Some("USD".into()));
        assert_eq!(a, b);
    }

    #[test]
    fn wire_format_serde_omits_scale_for_decimal_string() {
        let j = serde_json::to_value(MoneyWireFormat::decimal_string()).unwrap();
        assert_eq!(j, serde_json::json!({ "encoding": "decimal_string" }));
        let back: MoneyWireFormat = serde_json::from_value(j).unwrap();
        assert_eq!(back, MoneyWireFormat::DecimalString);
    }

    #[test]
    fn unknown_catalog_encoding_is_error() {
        let err = MoneyWireFormat::from_catalog_parts("float", None).unwrap_err();
        assert!(matches!(err, MoneyError::UnknownEncoding { tag } if tag == "float"));
    }
}
