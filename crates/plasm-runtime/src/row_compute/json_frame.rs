//! JSON object rows ↔ Polars DataFrame. Nested objects stay JSON strings; dotted paths
//! are extra columns used only for FieldPath access.

use indexmap::IndexMap;
use plasm_core::money::MoneyValue;
use plasm_core::{json_value_to_plasm_value, Value};
use polars::prelude::*;
use rust_decimal::Decimal;
use std::str::FromStr;

pub(super) const IDX_COL: &str = "__plasm_idx";
pub(super) const MONEY_AMOUNT: &str = "__amount";
pub(super) const MONEY_CCY: &str = "__ccy";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ColKind {
    Bool,
    Int,
    Float,
    Str,
    Temporal,
    Money,
    Json,
}

#[derive(Debug, Clone)]
pub(super) struct FrameState {
    pub df: DataFrame,
    pub visible: Vec<String>,
    pub kinds: IndexMap<String, ColKind>,
    /// Output names of money `sum` aggregates pending reconstruct + currency check.
    pub money_sum_names: Vec<String>,
}

pub(super) fn ingest_json_rows(rows: &[serde_json::Value]) -> PolarsResult<FrameState> {
    let mut visible = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        if let serde_json::Value::Object(map) = row {
            for k in map.keys() {
                if seen.insert(k.clone()) {
                    visible.push(k.clone());
                }
            }
        }
    }
    let mut kinds: IndexMap<String, ColKind> = IndexMap::new();
    let mut columns: IndexMap<String, Vec<AnyValue<'static>>> = IndexMap::new();
    for key in &visible {
        columns.insert(key.clone(), Vec::with_capacity(rows.len()));
    }
    let mut extra: IndexMap<String, Vec<AnyValue<'static>>> = IndexMap::new();

    for (i, row) in rows.iter().enumerate() {
        let obj = match row {
            serde_json::Value::Object(m) => m,
            other => {
                let mut m = serde_json::Map::new();
                m.insert("value".into(), other.clone());
                for key in &visible {
                    let cell = json_to_any(m.get(key).unwrap_or(&serde_json::Value::Null));
                    columns.get_mut(key).unwrap().push(cell);
                }
                flatten_into("", other, &mut extra, i, rows.len());
                continue;
            }
        };
        for key in &visible {
            let v = obj.get(key).unwrap_or(&serde_json::Value::Null);
            let cell = json_to_any(v);
            let kind = kind_of(&cell);
            kinds
                .entry(key.clone())
                .and_modify(|k| *k = promote(*k, kind))
                .or_insert(kind);
            columns.get_mut(key).unwrap().push(cell);
            flatten_into(key, v, &mut extra, i, rows.len());
        }
    }

    let mut series: Vec<Column> = Vec::new();
    let idx: Vec<u32> = (0..rows.len() as u32).collect();
    series.push(Column::new(PlSmallStr::from_static(IDX_COL), idx));
    for (key, vals) in columns {
        series.push(series_from_any(key.as_str(), vals, kinds.get(&key).copied())?.into());
    }
    for (key, vals) in extra {
        if visible.iter().any(|v| v == &key) {
            continue;
        }
        let kind = vals.iter().find_map(|v| {
            let k = kind_of(v);
            if k == ColKind::Json && matches!(v, AnyValue::Null) {
                None
            } else {
                Some(k)
            }
        });
        kinds
            .entry(key.clone())
            .or_insert(kind.unwrap_or(ColKind::Json));
        series.push(series_from_any(key.as_str(), vals, kinds.get(&key).copied())?.into());
    }
    Ok(FrameState {
        df: DataFrame::new(series)?,
        visible,
        kinds,
        money_sum_names: Vec::new(),
    })
}

fn flatten_into(
    prefix: &str,
    v: &serde_json::Value,
    extra: &mut IndexMap<String, Vec<AnyValue<'static>>>,
    row_i: usize,
    n: usize,
) {
    let serde_json::Value::Object(map) = v else {
        return;
    };
    for (k, child) in map {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        let slot = extra
            .entry(path.clone())
            .or_insert_with(|| vec![AnyValue::Null; n]);
        if slot.len() < n {
            slot.resize(n, AnyValue::Null);
        }
        slot[row_i] = json_to_any(child);
        flatten_into(&path, child, extra, row_i, n);
    }
}

fn json_to_any(v: &serde_json::Value) -> AnyValue<'static> {
    match json_value_to_plasm_value(v) {
        Value::Null => AnyValue::Null,
        Value::Bool(b) => AnyValue::Boolean(b),
        Value::Integer(i) => AnyValue::Int64(i),
        Value::Float(f) => AnyValue::Float64(f),
        Value::String(s) | Value::PhraseIdent(s) => AnyValue::StringOwned(s.into()),
        Value::Money(m) => money_any(&m),
        Value::Array(_) | Value::Object(_) | Value::UnionCtor { .. } | Value::PlasmInputRef(_) => {
            AnyValue::StringOwned(v.to_string().into())
        }
    }
}

fn money_any(m: &MoneyValue) -> AnyValue<'static> {
    let amount = m.amount().to_string();
    let ccy = m.currency().unwrap_or("").to_string();
    AnyValue::StructOwned(Box::new((
        vec![
            AnyValue::StringOwned(amount.into()),
            AnyValue::StringOwned(ccy.into()),
        ],
        vec![
            Field::new(PlSmallStr::from_static(MONEY_AMOUNT), DataType::String),
            Field::new(PlSmallStr::from_static(MONEY_CCY), DataType::String),
        ],
    )))
}

fn kind_of(v: &AnyValue<'_>) -> ColKind {
    match v {
        AnyValue::Null => ColKind::Json,
        AnyValue::Boolean(_) => ColKind::Bool,
        AnyValue::Int64(_) | AnyValue::Int32(_) | AnyValue::UInt32(_) | AnyValue::UInt64(_) => {
            ColKind::Int
        }
        AnyValue::Float64(_) | AnyValue::Float32(_) => ColKind::Float,
        AnyValue::StructOwned(_) | AnyValue::Struct(_, _, _) => ColKind::Money,
        AnyValue::String(_) | AnyValue::StringOwned(_) => ColKind::Str,
        _ => ColKind::Json,
    }
}

fn promote(a: ColKind, b: ColKind) -> ColKind {
    if a == b {
        return a;
    }
    if a == ColKind::Json {
        return b;
    }
    if b == ColKind::Json {
        return a;
    }
    match (a, b) {
        (ColKind::Int, ColKind::Float) | (ColKind::Float, ColKind::Int) => ColKind::Float,
        (ColKind::Money, _) | (_, ColKind::Money) => ColKind::Money,
        _ => ColKind::Str,
    }
}

fn series_from_any(
    name: &str,
    vals: Vec<AnyValue<'static>>,
    kind: Option<ColKind>,
) -> PolarsResult<Series> {
    let name = PlSmallStr::from_str(name);
    match kind.unwrap_or(ColKind::Json) {
        ColKind::Bool => {
            let data: Vec<Option<bool>> = vals
                .into_iter()
                .map(|v| match v {
                    AnyValue::Boolean(b) => Some(b),
                    AnyValue::Null => None,
                    _ => None,
                })
                .collect();
            Ok(Series::new(name, data))
        }
        ColKind::Int => {
            let data: Vec<Option<i64>> = vals
                .into_iter()
                .map(|v| match v {
                    AnyValue::Int64(i) => Some(i),
                    AnyValue::Int32(i) => Some(i as i64),
                    AnyValue::UInt32(i) => Some(i as i64),
                    AnyValue::Null => None,
                    _ => None,
                })
                .collect();
            Ok(Series::new(name, data))
        }
        ColKind::Float => {
            let data: Vec<Option<f64>> = vals
                .into_iter()
                .map(|v| match v {
                    AnyValue::Float64(f) => Some(f),
                    AnyValue::Int64(i) => Some(i as f64),
                    AnyValue::Null => None,
                    _ => None,
                })
                .collect();
            Ok(Series::new(name, data))
        }
        ColKind::Str | ColKind::Json | ColKind::Temporal => {
            let data: Vec<Option<String>> = vals
                .into_iter()
                .map(|v| match v {
                    AnyValue::Null => None,
                    AnyValue::StringOwned(s) => Some(s.as_str().to_string()),
                    AnyValue::String(s) => Some(s.to_string()),
                    AnyValue::Boolean(b) => Some(b.to_string()),
                    AnyValue::Int64(i) => Some(i.to_string()),
                    AnyValue::Float64(f) => Some(f.to_string()),
                    other => Some(other.to_string()),
                })
                .collect();
            Ok(Series::new(name, data))
        }
        ColKind::Money => Series::from_any_values_and_dtype(
            name,
            &vals,
            &DataType::Struct(vec![
                Field::new(PlSmallStr::from_static(MONEY_AMOUNT), DataType::String),
                Field::new(PlSmallStr::from_static(MONEY_CCY), DataType::String),
            ]),
            true,
        ),
    }
}

pub(super) fn collect_json(state: &FrameState) -> PolarsResult<Vec<serde_json::Value>> {
    let df = &state.df;
    let n = df.height();
    let mut out = Vec::with_capacity(n);
    for row_idx in 0..n {
        let mut map = serde_json::Map::new();
        for key in &state.visible {
            if key == IDX_COL {
                continue;
            }
            let Some(s) = df.column(key).ok() else {
                continue;
            };
            map.insert(
                key.clone(),
                any_to_json(s.get(row_idx)?, state.kinds.get(key).copied()),
            );
        }
        out.push(serde_json::Value::Object(map));
    }
    Ok(out)
}

fn any_to_json(v: AnyValue<'_>, kind: Option<ColKind>) -> serde_json::Value {
    if kind == Some(ColKind::Money) {
        if let Some(m) = money_from_any(&v) {
            return money_tagged_json(&m);
        }
    }
    match v {
        AnyValue::Null => serde_json::Value::Null,
        AnyValue::Boolean(b) => serde_json::Value::Bool(b),
        AnyValue::Int64(i) => serde_json::json!(i),
        AnyValue::Int32(i) => serde_json::json!(i),
        AnyValue::UInt32(i) => serde_json::json!(i),
        AnyValue::UInt64(i) => serde_json::json!(i),
        AnyValue::Float64(f) => serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        AnyValue::Float32(f) => serde_json::Number::from_f64(f as f64)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        AnyValue::String(s) => parse_json_or_string(s),
        AnyValue::StringOwned(s) => parse_json_or_string(s.as_str()),
        AnyValue::StructOwned(boxed) => {
            if let Some(m) = money_from_struct(&boxed.0, &boxed.1) {
                return money_tagged_json(&m);
            }
            serde_json::Value::Null
        }
        other => serde_json::Value::String(other.to_string()),
    }
}

fn parse_json_or_string(s: &str) -> serde_json::Value {
    let t = s.trim();
    if (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']')) {
        if let Ok(v) = serde_json::from_str(s) {
            return v;
        }
    }
    serde_json::Value::String(s.to_string())
}

fn money_tagged_json(m: &MoneyValue) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "__plasm_money".into(),
        serde_json::Value::String(m.amount().to_string()),
    );
    if let Some(c) = m.currency() {
        map.insert("currency".into(), serde_json::Value::String(c.to_string()));
    }
    serde_json::Value::Object(map)
}

fn money_from_any(v: &AnyValue<'_>) -> Option<MoneyValue> {
    match v {
        AnyValue::StructOwned(boxed) => money_from_struct(&boxed.0, &boxed.1),
        _ => None,
    }
}

fn money_from_struct(vals: &[AnyValue<'_>], fields: &[Field]) -> Option<MoneyValue> {
    let mut amount = None;
    let mut ccy = None;
    for (field, val) in fields.iter().zip(vals.iter()) {
        match field.name().as_str() {
            MONEY_AMOUNT => {
                amount = match val {
                    AnyValue::String(s) => Decimal::from_str(s).ok(),
                    AnyValue::StringOwned(s) => Decimal::from_str(s.as_str()).ok(),
                    _ => None,
                }
            }
            MONEY_CCY => {
                ccy = match val {
                    AnyValue::String(s) if !s.is_empty() => Some(s.to_string()),
                    AnyValue::StringOwned(s) if !s.is_empty() => Some(s.as_str().to_string()),
                    _ => None,
                }
            }
            _ => {}
        }
    }
    Some(MoneyValue::new(amount?, ccy))
}

pub(super) fn col_expr(path: &plasm_core::FieldPath) -> Expr {
    col(path.dotted())
}
