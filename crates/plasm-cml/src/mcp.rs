//! Declarative MCP calls. Endpoint configuration and credentials are host-owned.
use crate::{eval_cml, CmlEnv, CmlError, CmlExpr};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTemplate {
    pub server: String,
    pub tool: String,
    pub arguments: CmlExpr,
    pub response: McpResponse,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagination: Option<crate::PaginationConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<McpCollectionContract>,
}

/// Provider evidence required to certify an enumerated collection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpCollectionContract {
    pub total_pointer: String,
    pub complete_pointer: String,
    pub complete_value: serde_json::Value,
    pub max_items: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{compile_operation, parse_capability_template, CompiledOperation};
    use serde_json::json;

    fn mapping() -> serde_json::Value {
        json!({"transport":"mcp", "server":"fixture", "tool":"list_items",
            "arguments":{"type":"var","name":"input"},
            "response":{"source":{"kind":"structured_content"},"pointer":"/items","exhaustive":false}})
    }

    #[test]
    fn mcp_arguments_preserve_json_types_and_explicit_response_contract() {
        let template = parse_capability_template(&mapping()).unwrap();
        let args = json!({"parent":"p1","count":2,"enabled":true,"nested":[null,{"id":3}]});
        let mut env = CmlEnv::new();
        env.insert(
            "input".into(),
            serde_json::from_value(args.clone()).unwrap(),
        );
        let CompiledOperation::Mcp(call) = compile_operation(&template, &env).unwrap() else {
            panic!("MCP call expected")
        };
        assert_eq!(serde_json::Value::Object(call.arguments), args);
        env.insert("input".into(), plasm_core::Value::Null);
        assert!(matches!(
            compile_operation(&template, &env),
            Err(CmlError::Mcp(McpTemplateError::ArgumentsObject))
        ));
    }

    #[test]
    fn mcp_contract_rejects_unknown_fields_and_invalid_pointers() {
        for pointer in ["items", "/~2", "/~"] {
            let mut wire = mapping();
            wire["response"]["pointer"] = json!(pointer);
            assert!(matches!(
                parse_capability_template(&wire),
                Err(CmlError::Mcp(McpTemplateError::ResponsePointer))
            ));
        }
        let mut wire = mapping();
        wire["endpoint"] = json!("https://not-catalog-owned.invalid");
        assert!(parse_capability_template(&wire).is_err());
        let mut wire = mapping();
        wire["response"]
            .as_object_mut()
            .unwrap()
            .remove("exhaustive");
        assert!(parse_capability_template(&wire).is_err());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpResponse {
    /// Whether this call exhausts the declared query, not merely its returned page.
    pub exhaustive: bool,
    pub source: McpResultSource,
    /// JSON Pointer within the explicitly selected content, never an inferred path.
    pub pointer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpResultSource {
    StructuredContent,
    /// Preserve a text content block as a typed object, without parsing its text.
    TextBlock {
        index: usize,
    },
    JsonText {
        index: usize,
    },
    /// Embedded resource contents, including URI, MIME type and text or blob.
    EmbeddedResource {
        index: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledMcpCall {
    pub server: String,
    pub tool: String,
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum McpTemplateError {
    #[error("MCP arguments could not be serialized")]
    ArgumentsSerialization(#[source] std::sync::Arc<serde_json::Error>),
    #[error("MCP server binding must be a nonempty identifier")]
    ServerBinding,
    #[error("MCP tool name must be nonempty")]
    ToolName,
    #[error("MCP arguments must evaluate to an object")]
    ArgumentsObject,
    #[error("MCP response pointer must be a valid JSON Pointer")]
    ResponsePointer,
    #[error("MCP pagination must use argument (query) parameters without HTTP-specific paths")]
    PaginationLocation,
    #[error("invalid MCP pagination: {0}")]
    Pagination(#[from] crate::PaginationContractError),
    #[error("MCP collection evidence requires pagination, exhaustive response, positive bound and valid nonempty JSON pointers")]
    CollectionContract,
}

impl McpTemplate {
    pub fn validate(&self) -> Result<(), McpTemplateError> {
        if let Some(collection) = &self.collection {
            let valid_pointer = |p: &str| {
                if !p.starts_with('/') {
                    return false;
                }
                let mut bytes = p.bytes();
                while let Some(b) = bytes.next() {
                    if b == b'~' && !matches!(bytes.next(), Some(b'0' | b'1')) {
                        return false;
                    }
                }
                true
            };
            if self.pagination.is_none()
                || !self.response.exhaustive
                || collection.max_items == 0
                || !valid_pointer(&collection.total_pointer)
                || !valid_pointer(&collection.complete_pointer)
            {
                return Err(McpTemplateError::CollectionContract);
            }
        }
        if let Some(pagination) = &self.pagination {
            if pagination.location != crate::PaginationLocation::Query
                || pagination.body_merge_path.is_some()
                || pagination.response_next_url_field.is_some()
            {
                return Err(McpTemplateError::PaginationLocation);
            }
            pagination.validate()?;
        }
        if self.server.is_empty()
            || !self
                .server
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        {
            return Err(McpTemplateError::ServerBinding);
        }
        if self.tool.trim().is_empty() {
            return Err(McpTemplateError::ToolName);
        }
        let pointer = &self.response.pointer;
        if !pointer.is_empty() && !pointer.starts_with('/') {
            return Err(McpTemplateError::ResponsePointer);
        }
        let mut bytes = pointer.bytes();
        while let Some(byte) = bytes.next() {
            if byte == b'~' && !matches!(bytes.next(), Some(b'0' | b'1')) {
                return Err(McpTemplateError::ResponsePointer);
            }
        }
        Ok(())
    }

    pub fn compile(&self, env: &CmlEnv) -> Result<CompiledMcpCall, CmlError> {
        self.validate()?;
        let value = eval_cml(&self.arguments, env)?;
        let json = serde_json::to_value(value).map_err(|source| {
            McpTemplateError::ArgumentsSerialization(std::sync::Arc::new(source))
        })?;
        let serde_json::Value::Object(arguments) = json else {
            return Err(McpTemplateError::ArgumentsObject.into());
        };
        Ok(CompiledMcpCall {
            server: self.server.clone(),
            tool: self.tool.clone(),
            arguments,
        })
    }
}

#[cfg(test)]
mod pagination_tests {
    use super::*;

    #[test]
    fn pagination_accepts_argument_counters_and_rejects_http_locations() {
        let value = serde_json::json!({
            "server":"fixture", "tool":"list", "arguments":{"type":"object","fields":[]},
            "response":{"source":{"kind":"structured_content"},"pointer":"/items","exhaustive":true},
            "pagination":{"strategy":"page_number","params":{"page":{"counter":1},"size":{"fixed":2,"role":"page_size"}}}
        });
        let mapping: McpTemplate = serde_json::from_value(value.clone()).unwrap();
        mapping.validate().unwrap();
        let mut guarded = value.clone();
        guarded["collection"] = serde_json::json!({"total_pointer":"/total","complete_pointer":"/complete","complete_value":true,"max_items":10});
        serde_json::from_value::<McpTemplate>(guarded.clone())
            .unwrap()
            .validate()
            .unwrap();
        guarded["collection"]["total_pointer"] = "invalid".into();
        assert!(matches!(
            serde_json::from_value::<McpTemplate>(guarded)
                .unwrap()
                .validate(),
            Err(McpTemplateError::CollectionContract)
        ));
        for location in ["body", "link_header", "response_next_url", "block_range"] {
            let mut invalid = value.clone();
            invalid["pagination"]["location"] = location.into();
            let mapping: McpTemplate = serde_json::from_value(invalid).unwrap();
            assert!(matches!(
                mapping.validate(),
                Err(McpTemplateError::PaginationLocation)
            ));
        }
    }
}
