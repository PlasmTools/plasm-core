//! `server/discover` result (SEP-2575).

use rust_mcp_sdk::schema::{InitializeResult, ServerCapabilities};
use serde::Serialize;
use serde_json::{json, Value};

use super::meta::SUPPORTED_STATELESS_VERSIONS;

#[derive(Serialize)]
pub(crate) struct DiscoverResult {
    #[serde(rename = "supportedVersions")]
    supported_versions: Vec<&'static str>,
    capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    server_info: rust_mcp_sdk::schema::Implementation,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
}

pub(crate) fn build_discover_result(init: &InitializeResult) -> Value {
    let result = DiscoverResult {
        supported_versions: SUPPORTED_STATELESS_VERSIONS.to_vec(),
        capabilities: init.capabilities.clone(),
        server_info: init.server_info.clone(),
        instructions: init.instructions.clone(),
    };
    serde_json::to_value(result).unwrap_or_else(|_| json!({}))
}
