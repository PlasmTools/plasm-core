//! Pure exact-money bindings shared by Python inference and the Monty host.
use plasm_core::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum PythonMoneyError {
    #[error("Python Money requires a money record")]
    ExpectedRecord,
    #[error("Python Money requires an exact decimal amount")]
    MissingAmount,
    #[error("Python Money amount is not a valid decimal")]
    InvalidAmount,
    #[error("Python Money currency must be a string or null")]
    InvalidCurrency,
    #[error("money format requires an encoding")]
    MissingEncoding,
    #[error("money scale requires an unsigned integer")]
    InvalidScale,
    #[error("money scale is outside the supported range")]
    ScaleOutOfRange,
    #[error("Python Money encoding and scale are inconsistent")]
    InvalidFormat,
    #[error(transparent)]
    MoneyFormat(#[from] plasm_core::MoneyError),
    #[error("money comparison requires money, an integer, or an exact decimal string")]
    InvalidComparisonOperand,
    #[error("exact money comparison operand is not comparable")]
    IncomparableValues,
    #[error("money currencies `{left}` and `{right}` do not match")]
    CurrencyMismatch { left: String, right: String },
    #[error(transparent)]
    Arithmetic(#[from] plasm_core::value_expression::ArithmeticError),
    #[error("money factor must be an integer or exact decimal string")]
    InvalidFactor,
    #[error("unknown exact-money function")]
    UnknownFunction,
}

pub(crate) const STUBS: &str = r#"from typing import NewType, TypeAlias, TypeVar, overload
PlasmMoneyItem: TypeAlias = None | bool | int | float | str | list[PlasmMoneyItem] | dict[str, PlasmMoneyItem]
PlasmMoney = NewType('PlasmMoney', dict[str, PlasmMoneyItem])
PlasmMoneyT = TypeVar('PlasmMoneyT', bound=PlasmMoney)
@overload
def money_add(left: PlasmMoneyT, right: PlasmMoneyT) -> PlasmMoneyT: ...
@overload
def money_add(left: dict[str, PlasmMoneyItem], right: dict[str, PlasmMoneyItem]) -> PlasmMoney: ...
@overload
def money_sub(left: PlasmMoneyT, right: PlasmMoneyT) -> PlasmMoneyT: ...
@overload
def money_sub(left: dict[str, PlasmMoneyItem], right: dict[str, PlasmMoneyItem]) -> PlasmMoney: ...
@overload
def money_mul(value: PlasmMoneyT, factor: int | str) -> PlasmMoneyT: ...
@overload
def money_mul(value: dict[str, PlasmMoneyItem], factor: int | str) -> PlasmMoney: ...
def money_compare(left: dict[str, PlasmMoneyItem], right: dict[str, PlasmMoneyItem] | int | str) -> int: ...
@overload
def money_div(value: PlasmMoneyT, factor: int | str) -> PlasmMoneyT: ...
@overload
def money_div(value: dict[str, PlasmMoneyItem], factor: int | str) -> PlasmMoney: ...
"#;
pub(crate) const FUNCTIONS: [&str; 5] = [
    "money_add",
    "money_sub",
    "money_mul",
    "money_div",
    "money_compare",
];

pub(crate) fn decode(value: Value) -> Result<plasm_core::MoneyValue, PythonMoneyError> {
    Ok(match value {
        Value::Money(money) => money,
        Value::Object(fields) => {
            let amount = fields
                .get("__plasm_money")
                .and_then(Value::as_str)
                .ok_or(PythonMoneyError::MissingAmount)?
                .parse()
                .map_err(|_| PythonMoneyError::InvalidAmount)?;
            let currency = match fields.get("currency") {
                None | Some(Value::Null) => None,
                Some(Value::String(value)) => Some(value.clone()),
                _ => return Err(PythonMoneyError::InvalidCurrency),
            };
            let mut money = plasm_core::MoneyValue::new(amount, currency);
            if let Some(format) = fields.get("format").filter(|v| !v.is_null()) {
                use plasm_core::money::MoneyWireFormat;
                let encoding = format
                    .get("encoding")
                    .and_then(Value::as_str)
                    .ok_or(PythonMoneyError::MissingEncoding)?;
                let scale = format.get("scale").filter(|v| !v.is_null());
                let format = match (encoding, scale) {
                    ("decimal_string", None) => MoneyWireFormat::DecimalString,
                    ("json_number", None) => MoneyWireFormat::JsonNumber,
                    ("minor_units", Some(scale)) => MoneyWireFormat::minor_units(
                        u8::try_from(scale.as_unsigned().ok_or(PythonMoneyError::InvalidScale)?)
                            .map_err(|_| PythonMoneyError::ScaleOutOfRange)?,
                    )
                    .map_err(PythonMoneyError::MoneyFormat)?,
                    _ => return Err(PythonMoneyError::InvalidFormat),
                };
                money = money.with_format(format);
            }
            money
        }
        _ => return Err(PythonMoneyError::ExpectedRecord),
    })
}

pub(crate) fn evaluate(name: &str, left: Value, right: Value) -> Result<Value, PythonMoneyError> {
    let left = decode(left)?;
    match name {
        "money_compare" => {
            let right = match right {
                Value::Money(_) | Value::Object(_) => Value::Money(decode(right)?),
                Value::Integer(_) | Value::Unsigned(_) | Value::String(_) => right,
                _ => return Err(PythonMoneyError::InvalidComparisonOperand),
            };
            let order = plasm_core::money::try_cmp_values(&Value::Money(left), &right)
                .map_err(|error| PythonMoneyError::CurrencyMismatch {
                    left: error.left().to_owned(),
                    right: error.right().to_owned(),
                })?
                .ok_or(PythonMoneyError::IncomparableValues)?;
            Ok(Value::Integer(match order {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }))
        }
        "money_add" | "money_sub" => plasm_core::value_expression::arithmetic(
            if name == "money_add" {
                plasm_core::ArithOp::Add
            } else {
                plasm_core::ArithOp::Sub
            },
            Value::Money(left),
            Value::Money(decode(right)?),
        )
        // Monty's host callback accepts its terminal exception diagnostic as text.
        // Keep the shared kernel typed up to this rendering boundary.
        .map_err(PythonMoneyError::Arithmetic),
        "money_mul" | "money_div" => {
            let factor = match right {
                Value::Integer(n) => n.to_string(),
                Value::Unsigned(n) => n.to_string(),
                Value::String(s) => s,
                _ => return Err(PythonMoneyError::InvalidFactor),
            };
            plasm_core::money::scale_exact(&left, &factor, name == "money_div")
                .map(Value::Money)
                .map_err(PythonMoneyError::MoneyFormat)
        }
        _ => Err(PythonMoneyError::UnknownFunction),
    }
}
