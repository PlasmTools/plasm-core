//! Pure exact-money bindings shared by Python inference and the Monty host.
use plasm_core::Value;

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

pub(crate) fn decode(value: Value) -> Result<plasm_core::MoneyValue, String> {
    Ok(match value {
        Value::Money(money) => money,
        Value::Object(fields) => {
            let amount = fields
                .get("__plasm_money")
                .and_then(Value::as_str)
                .ok_or("Python Money requires an exact __plasm_money decimal string")?
                .parse()
                .map_err(|e| format!("invalid Python money amount: {e}"))?;
            let currency = match fields.get("currency") {
                None | Some(Value::Null) => None,
                Some(Value::String(value)) => Some(value.clone()),
                _ => return Err("invalid Python money currency".into()),
            };
            let mut money = plasm_core::MoneyValue::new(amount, currency);
            if let Some(format) = fields.get("format").filter(|v| !v.is_null()) {
                use plasm_core::money::MoneyWireFormat;
                let encoding = format
                    .get("encoding")
                    .and_then(Value::as_str)
                    .ok_or("money format requires encoding")?;
                let scale = format.get("scale").filter(|v| !v.is_null());
                let format = match (encoding, scale) {
                    ("decimal_string", None) => MoneyWireFormat::DecimalString,
                    ("json_number", None) => MoneyWireFormat::JsonNumber,
                    ("minor_units", Some(scale)) => MoneyWireFormat::minor_units(
                        u8::try_from(scale.as_unsigned().ok_or("money scale requires integer")?)
                            .map_err(|_| "money scale is out of range")?,
                    )
                    .map_err(|e| e.to_string())?,
                    _ => return Err("invalid money encoding or scale".into()),
                };
                money = money.with_format(format);
            }
            money
        }
        _ => return Err("Python Money requires a money record".into()),
    })
}

pub(crate) fn evaluate(name: &str, left: Value, right: Value) -> Result<Value, String> {
    let left = decode(left)?;
    match name {
        "money_compare" => {
            let right = match right {
                Value::Money(_) | Value::Object(_) => Value::Money(decode(right)?),
                Value::Integer(_) | Value::Unsigned(_) | Value::String(_) => right,
                _ => {
                    return Err(
                        "money comparison requires money, an integer or exact decimal string"
                            .into(),
                    )
                }
            };
            let order = plasm_core::money::try_cmp_values(&Value::Money(left), &right)
                .map_err(|e| e.to_string())?
                .ok_or("invalid exact money comparison operand")?;
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
        ),
        "money_mul" | "money_div" => {
            let factor = match right {
                Value::Integer(n) => n.to_string(),
                Value::Unsigned(n) => n.to_string(),
                Value::String(s) => s,
                _ => return Err("money factor requires an integer or exact decimal string".into()),
            };
            plasm_core::money::scale_exact(&left, &factor, name == "money_div").map(Value::Money)
        }
        _ => Err("unknown exact-money function".into()),
    }
}
