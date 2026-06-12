//! CGS-informed column metadata for Run Explorer UI (`_meta.plasm.steps[].column_schema`).

use plasm_core::{Cardinality, EntityDef, FieldType, RelationSchema, StringSemantics, CGS};
use plasm_runtime::{CachedEntity, ExecutionResult};
use serde_json::{json, Value};

use crate::output::union_entity_table_columns;

#[derive(Debug, Clone)]
pub(crate) struct RunStepColumnSchema {
    pub entity_type: String,
    pub entry_id: String,
    pub columns: Vec<Value>,
}

pub(crate) fn build_run_step_column_schema(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
    entry_id: Option<&str>,
    entity_type_hint: Option<&str>,
) -> Option<RunStepColumnSchema> {
    let first = result.entities.first()?;
    let entity_type = entity_type_hint
        .map(str::to_string)
        .unwrap_or_else(|| first.reference.entity_type.to_string());
    let cgs = cgs?;
    let ent = cgs.get_entity(entity_type.as_str())?;
    let column_names = union_entity_table_columns(result, Some(cgs));
    if column_names.is_empty() {
        return None;
    }
    let mut columns = Vec::with_capacity(column_names.len());
    for name in column_names {
        if let Some(col) = column_meta_for_name(ent, cgs, &name, first) {
            columns.push(col);
        }
    }
    Some(RunStepColumnSchema {
        entity_type,
        entry_id: entry_id.unwrap_or("default").to_string(),
        columns,
    })
}

fn column_meta_for_name(
    ent: &EntityDef,
    cgs: &CGS,
    name: &str,
    sample: &CachedEntity,
) -> Option<Value> {
    if let Some(base) = name.strip_suffix("_ref").filter(|b| !b.is_empty()) {
        if ent.fields.contains_key(base) {
            return Some(json!({
                "name": name,
                "kind": "field",
                "wire_type": "string",
                "blob_part": "ref",
            }));
        }
    }
    if let Some(base) = name.strip_suffix("_mime").filter(|b| !b.is_empty()) {
        if ent.fields.contains_key(base) {
            return Some(json!({
                "name": name,
                "kind": "field",
                "wire_type": "string",
                "blob_part": "mime",
            }));
        }
    }
    if let Some(fs) = ent.fields.get(name) {
        let nv = cgs.named_value_for_slot(fs).ok()?;
        return Some(json!({
            "name": name,
            "kind": "field",
            "wire_type": field_wire_type(&nv.field_type, nv.string_semantics),
        }));
    }
    if let Some(rel) = ent.relations.get(name) {
        if !sample.relations.contains_key(name) {
            return None;
        }
        return Some(relation_column_meta(name, rel));
    }
    None
}

fn relation_column_meta(name: &str, rel: &RelationSchema) -> Value {
    let cardinality = match rel.cardinality {
        Cardinality::One => "one",
        Cardinality::Many => "many",
    };
    json!({
        "name": name,
        "kind": "relation",
        "wire_type": "entity_ref",
        "ref_entity": rel.target_resource.to_string(),
        "cardinality": cardinality,
    })
}

fn field_wire_type(ft: &FieldType, semantics: Option<StringSemantics>) -> &'static str {
    if matches!(ft, FieldType::String) {
        if semantics == Some(StringSemantics::Markdown) {
            return "markdown";
        }
        if semantics == Some(StringSemantics::Html) {
            return "html";
        }
        if semantics == Some(StringSemantics::Document) {
            return "document";
        }
    }
    field_type_wire_label(ft)
}

fn field_type_wire_label(ft: &FieldType) -> &'static str {
    match ft {
        FieldType::Boolean => "boolean",
        FieldType::Number => "number",
        FieldType::Integer => "integer",
        FieldType::Uuid => "uuid",
        FieldType::Blob => "blob",
        FieldType::String => "string",
        FieldType::Select => "select",
        FieldType::MultiSelect => "multi_select",
        FieldType::Date => "date",
        FieldType::Array => "array",
        FieldType::Json => "json",
        FieldType::EntityRef { .. } => "entity_ref",
    }
}

pub(crate) fn column_schema_json(schema: &RunStepColumnSchema) -> Value {
    json!({
        "entity_type": schema.entity_type,
        "entry_id": schema.entry_id,
        "columns": schema.columns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_wire_type_markdown_from_semantics() {
        assert_eq!(
            field_wire_type(&FieldType::String, Some(StringSemantics::Markdown)),
            "markdown"
        );
        assert_eq!(
            field_wire_type(&FieldType::String, Some(StringSemantics::Short)),
            "string"
        );
    }

    #[test]
    fn column_schema_json_shape() {
        let schema = RunStepColumnSchema {
            entity_type: "Pokemon".into(),
            entry_id: "pokeapi".into(),
            columns: vec![json!({
                "name": "moves",
                "kind": "relation",
                "wire_type": "entity_ref",
                "ref_entity": "Move",
                "cardinality": "many",
            })],
        };
        let v = column_schema_json(&schema);
        assert_eq!(v["entity_type"], "Pokemon");
        assert_eq!(v["columns"][0]["name"], "moves");
    }
}
