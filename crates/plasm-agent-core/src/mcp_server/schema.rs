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

pub(crate) fn args_value(params: &CallToolRequestParams) -> serde_json::Value {
    serde_json::Value::Object(params.arguments.clone().unwrap_or_default())
}
