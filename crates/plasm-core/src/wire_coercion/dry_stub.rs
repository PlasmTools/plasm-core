//! DryStub mode: invent catalog-typed placeholder cells (RA-8 DryStub).

use super::try_plasm_value_to_json;
use crate::{
    ArrayItemsSchema, EntityDef, FieldType, NamedValueSchema, Value, ValueWireFormat, CGS,
};
use indexmap::IndexMap;

/// DryStub mode: invent a catalog-typed placeholder cell (never blanket strings for int/bool/…).
pub fn dry_stub_value_for_named_value(nv: &NamedValueSchema, i: usize) -> Value {
    dry_stub_value_for_field_type(
        &nv.field_type,
        nv.value_format,
        nv.array_items.as_ref(),
        nv.allowed_values.as_deref(),
        nv.currency.as_deref(),
        i,
    )
}

/// DryStub JSON cell for agent dry-plan row materialization.
pub fn dry_stub_json_for_named_value(nv: &NamedValueSchema, i: usize) -> serde_json::Value {
    // DryStub invents only JSON-encodable placeholders (int/bool/string/array/object).
    try_plasm_value_to_json(&dry_stub_value_for_named_value(nv, i))
        .unwrap_or_else(|e| panic!("RA-8 DryStub cell must be JSON-encodable: {e}"))
}

/// Build typed dry-plan stub rows for an entity (RA-8 DryStub).
pub fn dry_stub_entity_row_json(
    cgs: &CGS,
    ent: &EntityDef,
    count: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = Vec::with_capacity(count);
    for i in 0..count {
        let mut obj = serde_json::Map::new();
        for (field_name, field) in &ent.fields {
            let nv = field.named_value(cgs).map_err(|e| e.to_string())?;
            obj.insert(
                field_name.as_str().to_string(),
                dry_stub_json_for_named_value(nv, i),
            );
        }
        let id_name = ent.id_field.as_str();
        if let Some(id_field) = ent.fields.get(id_name) {
            let nv = id_field.named_value(cgs).map_err(|e| e.to_string())?;
            obj.insert(id_name.to_string(), dry_stub_json_for_named_value(nv, i));
        } else {
            obj.insert(
                id_name.to_string(),
                serde_json::Value::String(format!("dry-{i}")),
            );
        }
        rows.push(serde_json::Value::Object(obj));
    }
    Ok(rows)
}

fn dry_stub_value_for_field_type(
    ft: &FieldType,
    value_format: Option<ValueWireFormat>,
    array_items: Option<&ArrayItemsSchema>,
    allowed: Option<&[String]>,
    currency: Option<&str>,
    i: usize,
) -> Value {
    match ft {
        FieldType::Integer => Value::Integer(i as i64),
        FieldType::Number => Value::Float(i as f64),
        FieldType::Boolean => Value::Bool(i.is_multiple_of(2)),
        FieldType::String | FieldType::Uuid | FieldType::Blob | FieldType::EntityRef { .. } => {
            Value::String(format!("dry-{i}"))
        }
        FieldType::Select => {
            if let Some(toks) = allowed.filter(|t| !t.is_empty()) {
                Value::String(toks[i % toks.len()].clone())
            } else {
                Value::String(format!("dry-{i}"))
            }
        }
        FieldType::MultiSelect => {
            if let Some(toks) = allowed.filter(|t| !t.is_empty()) {
                Value::Array(vec![Value::String(toks[i % toks.len()].clone())])
            } else {
                Value::Array(vec![Value::String(format!("dry-{i}"))])
            }
        }
        FieldType::Date => {
            let day = (i % 28) + 1;
            let raw = Value::String(format!("2020-01-{day:02}T00:00:00Z"));
            match value_format {
                Some(ValueWireFormat::Temporal(fmt)) => {
                    crate::temporal::normalize_temporal_value(raw.clone(), fmt).unwrap_or(raw)
                }
                _ => raw,
            }
        }
        FieldType::Money => {
            let amount = rust_decimal::Decimal::from(i as i64);
            let m = crate::money::MoneyValue::new(amount, currency.map(str::to_string));
            let fmt = match value_format {
                Some(ValueWireFormat::Money(f)) => f,
                _ => crate::money::MoneyWireFormat::DecimalString,
            };
            Value::Money(m.with_format(fmt))
        }
        FieldType::Array => {
            let elem = match array_items {
                Some(items) => dry_stub_value_for_field_type(
                    &items.field_type,
                    items.value_format,
                    None,
                    items.allowed_values.as_deref(),
                    None,
                    i,
                ),
                None => Value::String(format!("dry-{i}")),
            };
            Value::Array(vec![elem])
        }
        FieldType::Json => Value::Object(IndexMap::new()),
    }
}
