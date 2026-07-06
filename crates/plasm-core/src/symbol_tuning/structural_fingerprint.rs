//! Structural allocation fingerprints for value domains and inline capability slots.

use crate::schema::{ArrayItemsSchema, StringSemantics};
use crate::FieldType;

pub(crate) fn structural_value_domain_allocation_fp(
    catalog_entry_id: &str,
    field_type: &FieldType,
    string_semantics: Option<StringSemantics>,
    array_items: Option<&ArrayItemsSchema>,
    allowed_values: Option<&Vec<String>>,
) -> String {
    let type_fp = structural_field_type_fp(catalog_entry_id, field_type);
    let sem = string_semantics
        .map(|s| serde_json::to_string(&s).unwrap_or_else(|_| "\"?\"".to_string()))
        .unwrap_or_else(|| "null".to_string());
    let array = array_items
        .map(|items| structural_array_items_fp(catalog_entry_id, items))
        .unwrap_or_else(|| "null".to_string());
    let allowed = allowed_values
        .map(|values| {
            let mut values = values.clone();
            values.sort();
            serde_json::to_string(&values).unwrap_or_else(|_| "[]".to_string())
        })
        .unwrap_or_else(|| "null".to_string());
    format!("vc|{type_fp}|sem:{sem}|items:{array}|allowed:{allowed}")
}

pub(crate) fn structural_field_type_fp(catalog_entry_id: &str, field_type: &FieldType) -> String {
    match field_type {
        FieldType::EntityRef { target } => {
            format!("entity_ref|{}|{}", catalog_entry_id, target.as_str())
        }
        _ => serde_json::to_string(field_type).unwrap_or_else(|_| "\"?\"".to_string()),
    }
}

pub(crate) fn structural_array_items_fp(catalog_entry_id: &str, items: &ArrayItemsSchema) -> String {
    let field_type = structural_field_type_fp(catalog_entry_id, &items.field_type);
    let value_format =
        serde_json::to_string(&items.value_format).unwrap_or_else(|_| "null".to_string());
    let allowed = items
        .allowed_values
        .as_ref()
        .map(|values| {
            let mut values = values.clone();
            values.sort();
            serde_json::to_string(&values).unwrap_or_else(|_| "[]".to_string())
        })
        .unwrap_or_else(|| "null".to_string());
    format!("{field_type}|format:{value_format}|allowed:{allowed}")
}
