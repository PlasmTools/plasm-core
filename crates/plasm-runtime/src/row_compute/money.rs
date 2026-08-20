//! Money aggregate lowering and reconstruction.

use super::json_frame::{col_expr, ColKind, FrameState, MONEY_AMOUNT, MONEY_CCY};
use plasm_core::FieldPath;
use polars::prelude::*;
use rust_decimal::Decimal;

pub(super) fn push_money_sum(
    agg_exprs: &mut Vec<Expr>,
    visible: &mut Vec<String>,
    state: &mut FrameState,
    name: &str,
    field: &FieldPath,
) {
    let amount = col_expr(field)
        .struct_()
        .field_by_name(MONEY_AMOUNT)
        .cast(DataType::Decimal(Some(38), Some(8)));
    let ccy = col_expr(field).struct_().field_by_name(MONEY_CCY);
    agg_exprs.push(ccy.clone().n_unique().alias(format!("__ccy_n_{name}")));
    agg_exprs.push(ccy.first().alias(format!("__ccy_{name}")));
    agg_exprs.push(amount.sum().alias(name));
    visible.push(name.to_string());
    state.kinds.insert(name.to_string(), ColKind::Money);
    state.money_sum_names.push(name.to_string());
}

pub(super) fn finalize_money_sums(state: &mut FrameState) -> PolarsResult<()> {
    let names = std::mem::take(&mut state.money_sum_names);
    for name in names {
        let n_col = format!("__ccy_n_{name}");
        let c_col = format!("__ccy_{name}");
        let n_unique = state.df.column(&n_col)?;
        let ccys = state.df.column(&c_col)?;
        for i in 0..state.df.height() {
            let n = match n_unique.get(i)? {
                AnyValue::UInt32(n) => n as u64,
                AnyValue::UInt64(n) => n,
                AnyValue::Int64(n) if n >= 0 => n as u64,
                AnyValue::Int32(n) if n >= 0 => n as u64,
                AnyValue::Null => 0,
                other => {
                    return Err(PolarsError::ComputeError(
                        format!("unexpected currency-count dtype {other:?}").into(),
                    ))
                }
            };
            if n > 1 {
                let left = match ccys.get(i)? {
                    AnyValue::String(s) => s.to_string(),
                    AnyValue::StringOwned(s) => s.as_str().to_string(),
                    _ => "left".into(),
                };
                return Err(PolarsError::ComputeError(
                    format!("cannot compare money in {left} to money in another currency").into(),
                ));
            }
        }

        let amounts = state.df.column(&name)?;
        let mut encoded: Vec<Option<String>> = Vec::with_capacity(state.df.height());
        for i in 0..state.df.height() {
            let amount = any_amount_string(amounts.get(i)?)?;
            let ccy = match ccys.get(i)? {
                AnyValue::String(s) => s.to_string(),
                AnyValue::StringOwned(s) => s.as_str().to_string(),
                AnyValue::Null => String::new(),
                other => other.to_string(),
            };
            let mut map = serde_json::Map::new();
            map.insert("__plasm_money".into(), serde_json::Value::String(amount));
            if !ccy.is_empty() {
                map.insert("currency".into(), serde_json::Value::String(ccy));
            }
            encoded.push(Some(serde_json::Value::Object(map).to_string()));
        }
        let series = Series::new(PlSmallStr::from_str(&name), encoded);
        state.df.with_column(series)?;
        let _ = state.df.drop_in_place(&n_col);
        let _ = state.df.drop_in_place(&c_col);
        state.kinds.insert(name, ColKind::Json);
    }
    Ok(())
}

fn any_amount_string(v: AnyValue<'_>) -> PolarsResult<String> {
    Ok(match v {
        AnyValue::Decimal(unscaled, scale) => format_decimal_i128(unscaled, scale),
        AnyValue::Float64(f) => trim_float(f),
        AnyValue::Float32(f) => trim_float(f as f64),
        AnyValue::Int64(i) => i.to_string(),
        AnyValue::Int32(i) => i.to_string(),
        AnyValue::String(s) => s.to_string(),
        AnyValue::StringOwned(s) => s.as_str().to_string(),
        AnyValue::Null => "0".into(),
        other => {
            return Err(PolarsError::ComputeError(
                format!("cannot encode money amount from {other:?}").into(),
            ))
        }
    })
}

fn format_decimal_i128(unscaled: i128, scale: usize) -> String {
    Decimal::from_i128_with_scale(unscaled, scale as u32)
        .normalize()
        .to_string()
}

fn trim_float(f: f64) -> String {
    let d = Decimal::from_f64_retain(f)
        .unwrap_or(Decimal::ZERO)
        .normalize();
    d.to_string()
}
