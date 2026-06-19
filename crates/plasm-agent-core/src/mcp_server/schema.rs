//! JSON Schema fragments for MCP tool parameter definitions.

use rust_mcp_sdk::schema::CallToolRequestParams;

pub(crate) fn json_schema_string_type(
    description: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), serde_json::json!("string"));
    m.insert(
        "description".into(),
        serde_json::Value::String(description.to_string()),
    );
    m
}

pub(crate) fn json_schema_non_empty_string_type(
    description: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut m = json_schema_string_type(description);
    m.insert("minLength".into(), serde_json::json!(1));
    m
}

pub(crate) fn json_schema_non_empty_object_array(
    description: &str,
    required_fields: Vec<&str>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut item_props = serde_json::Map::new();
    item_props.insert(
        "api".into(),
        serde_json::Value::Object(json_schema_string_type(
            "**`api`**: integration id from the discovery table (or any id you already know). **`entry_id`** is an alternate JSON key for the same value",
        )),
    );
    item_props.insert(
        "entity".into(),
        serde_json::Value::Object(json_schema_string_type(
            "**`entity`**: resource type name from the discovery table (or a name you already know)",
        )),
    );
    let mut item_obj = serde_json::Map::new();
    item_obj.insert("type".into(), serde_json::json!("object"));
    item_obj.insert("properties".into(), serde_json::Value::Object(item_props));
    item_obj.insert(
        "required".into(),
        serde_json::Value::Array(
            required_fields
                .into_iter()
                .map(|f| serde_json::Value::String(f.to_string()))
                .collect(),
        ),
    );
    let mut m = serde_json::Map::new();
    m.insert("type".into(), serde_json::json!("array"));
    m.insert("items".into(), serde_json::Value::Object(item_obj));
    m.insert("minItems".into(), serde_json::json!(1));
    m.insert(
        "description".into(),
        serde_json::Value::String(description.to_string()),
    );
    m
}

pub(crate) fn args_value(params: &CallToolRequestParams) -> serde_json::Value {
    serde_json::Value::Object(params.arguments.clone().unwrap_or_default())
}
