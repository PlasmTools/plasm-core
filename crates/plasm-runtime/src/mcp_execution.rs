//! Named, host-owned MCP sessions and explicit catalogue response projection.
use crate::mcp_transport::{CallToolRequestParams, CallToolResult, McpSession, McpTransportError};
use plasm_compile::{CompiledMcpCall, McpResponse, McpResultSource};
use serde_json::Value;
use std::collections::HashMap;
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum McpExecutionError {
    #[error("MCP query projection must be an array")]
    QueryArrayRequired,
    #[error("MCP collection completeness evidence is missing or indicates an incomplete response at {pointer}")]
    IncompleteCollection { pointer: String },
    #[error("MCP collection total at {pointer} must be an unsigned integer")]
    InvalidCollectionTotal { pointer: String },
    #[error(
        "MCP collection total {total} exceeds provider enumeration bound {limit}; narrow the query"
    )]
    CollectionLimit { total: u64, limit: u64 },
    #[error("MCP collection total changed between pages from {previous} to {current}")]
    CollectionTotalChanged { previous: u64, current: u64 },
    #[error("MCP collection returned {observed} rows but declared {total}")]
    CollectionCountMismatch { observed: u64, total: u64 },
    #[error("MCP server binding {server} is not configured")]
    MissingBinding { server: String },
    #[error("MCP transport failed: {0}")]
    Transport(#[from] McpTransportError),
    #[error("MCP tool {tool} reported failure")]
    ToolFailure {
        tool: String,
        result: Box<CallToolResult>,
    },
    #[error("MCP result serialization failed")]
    Serialization(#[source] serde_json::Error),
    #[error("MCP structuredContent is missing")]
    MissingStructuredContent,
    #[error("MCP content block {index} is not text")]
    TextBlockRequired { index: usize },
    #[error("MCP content block {index} is not an embedded text or blob resource")]
    EmbeddedResourceRequired { index: usize },
    #[error("MCP content block {index} is not valid JSON")]
    InvalidJsonText {
        index: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("MCP response pointer {pointer} did not resolve")]
    MissingPointer { pointer: String },
    #[error("MCP pagination is not supported by this mapping")]
    PaginationUnsupported,
}

#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub struct McpCollectionProgress {
    total: Option<u64>,
    observed: u64,
}

impl McpCollectionProgress {
    pub(crate) fn observe(
        &mut self,
        contract: &plasm_compile::McpCollectionContract,
        response: &Value,
        count: usize,
        provider_more: bool,
    ) -> Result<bool, McpExecutionError> {
        if response.pointer(&contract.complete_pointer) != Some(&contract.complete_value) {
            return Err(McpExecutionError::IncompleteCollection {
                pointer: contract.complete_pointer.clone(),
            });
        }
        let total = response
            .pointer(&contract.total_pointer)
            .and_then(Value::as_u64)
            .ok_or_else(|| McpExecutionError::InvalidCollectionTotal {
                pointer: contract.total_pointer.clone(),
            })?;
        if total > contract.max_items {
            return Err(McpExecutionError::CollectionLimit {
                total,
                limit: contract.max_items,
            });
        }
        if let Some(previous) = self.total {
            if previous != total {
                return Err(McpExecutionError::CollectionTotalChanged {
                    previous,
                    current: total,
                });
            }
        }
        let observed = self.observed.saturating_add(count as u64);
        if observed > total || (!provider_more && observed != total) {
            return Err(McpExecutionError::CollectionCountMismatch { observed, total });
        }
        self.total = Some(total);
        self.observed = observed;
        Ok(observed < total)
    }
}

#[derive(Default)]
pub struct McpBindings {
    sessions: HashMap<String, Mutex<McpSession>>,
}

impl McpBindings {
    pub fn new(sessions: HashMap<String, McpSession>) -> Self {
        Self {
            sessions: sessions
                .into_iter()
                .map(|(name, session)| (name, Mutex::new(session)))
                .collect(),
        }
    }

    pub async fn execute(&self, call: &CompiledMcpCall) -> Result<Value, McpExecutionError> {
        self.execute_inner(call, None).await
    }

    pub(crate) async fn execute_recorded(
        &self,
        call: &CompiledMcpCall,
        fingerprint: String,
    ) -> Result<Value, McpExecutionError> {
        self.execute_inner(call, Some(fingerprint)).await
    }

    async fn execute_inner(
        &self,
        call: &CompiledMcpCall,
        fingerprint: Option<String>,
    ) -> Result<Value, McpExecutionError> {
        let session =
            self.sessions
                .get(&call.server)
                .ok_or_else(|| McpExecutionError::MissingBinding {
                    server: call.server.clone(),
                })?;
        let mut session = session.lock().await;
        let receipt = fingerprint.and_then(crate::execution::DispatchReceipt::begin);
        let result = crate::execution::with_dispatch_receipt(receipt, async {
            let result = session
                .call_tool(
                    CallToolRequestParams::new(call.tool.clone())
                        .with_arguments(call.arguments.clone()),
                )
                .await?;
            crate::execution::record_transport_response_received();
            Ok::<_, McpTransportError>(result)
        })
        .await?;
        if result.is_error == Some(true) {
            return Err(McpExecutionError::ToolFailure {
                tool: call.tool.clone(),
                result: Box::new(result),
            });
        }
        serde_json::to_value(result).map_err(McpExecutionError::Serialization)
    }
}

pub(crate) fn project_response(
    response: Value,
    mapping: &McpResponse,
) -> Result<Value, McpExecutionError> {
    select_response(response, mapping)?
        .pointer(&mapping.pointer)
        .cloned()
        .ok_or_else(|| McpExecutionError::MissingPointer {
            pointer: mapping.pointer.clone(),
        })
}

/// Retain the selected provider envelope for pagination observations.
pub(crate) fn select_response(
    response: Value,
    mapping: &McpResponse,
) -> Result<Value, McpExecutionError> {
    let selected = match mapping.source {
        McpResultSource::TextBlock { index } => response
            .get("content")
            .and_then(Value::as_array)
            .and_then(|blocks| blocks.get(index))
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter(|block| block.get("text").and_then(Value::as_str).is_some())
            .cloned()
            .ok_or(McpExecutionError::TextBlockRequired { index })?,
        McpResultSource::EmbeddedResource { index } => {
            let resource = response
                .get("content")
                .and_then(Value::as_array)
                .and_then(|blocks| blocks.get(index))
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("resource"))
                .and_then(|block| block.get("resource"))
                .filter(|resource| {
                    resource.get("uri").and_then(Value::as_str).is_some()
                        && (resource.get("text").and_then(Value::as_str).is_some()
                            ^ resource.get("blob").and_then(Value::as_str).is_some())
                })
                .ok_or(McpExecutionError::EmbeddedResourceRequired { index })?;
            resource.clone()
        }
        McpResultSource::StructuredContent => response
            .get("structuredContent")
            .cloned()
            .ok_or(McpExecutionError::MissingStructuredContent)?,
        McpResultSource::JsonText { index } => {
            let block = response
                .get("content")
                .and_then(Value::as_array)
                .and_then(|blocks| blocks.get(index));
            let text = block
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .and_then(|block| block.get("text"))
                .and_then(Value::as_str)
                .ok_or(McpExecutionError::TextBlockRequired { index })?;
            serde_json::from_str(text)
                .map_err(|source| McpExecutionError::InvalidJsonText { index, source })?
        }
    };
    Ok(selected)
}

#[cfg(test)]
mod tests {
    #[test]
    fn text_block_preserves_plain_and_json_text_without_claiming_completion() {
        use super::*;
        let mapping = McpResponse {
            source: McpResultSource::TextBlock { index: 0 },
            pointer: String::new(),
            exhaustive: true,
        };
        for text in ["Operation accepted", "{\"id\":42}", ""] {
            let block = serde_json::json!({"type":"text","text":text});
            assert_eq!(
                project_response(serde_json::json!({"content":[block.clone()]}), &mapping).unwrap(),
                block
            );
        }
        for block in [
            serde_json::json!({"type":"image","text":"x"}),
            serde_json::json!({"type":"text","text":4}),
        ] {
            assert!(matches!(
                project_response(serde_json::json!({"content":[block]}), &mapping),
                Err(McpExecutionError::TextBlockRequired { index: 0 })
            ));
        }
    }
    use super::*;
    use serde_json::json;

    #[test]
    fn collection_counts_require_complete_stable_bounded_evidence() {
        let contract = plasm_compile::McpCollectionContract {
            total_pointer: "/total".into(),
            complete_pointer: "/complete".into(),
            complete_value: json!(true),
            max_items: 4,
        };
        let response = json!({"total":4,"complete":true});
        let mut progress = McpCollectionProgress::default();
        assert!(progress.observe(&contract, &response, 2, true).unwrap());
        let mut resumed = progress.clone();
        assert!(!resumed.observe(&contract, &response, 2, true).unwrap());
        assert!(matches!(
            progress
                .clone()
                .observe(&contract, &json!({"total":3,"complete":true}), 1, false),
            Err(McpExecutionError::CollectionTotalChanged { .. })
        ));
        assert!(matches!(
            progress.observe(&contract, &response, 1, false),
            Err(McpExecutionError::CollectionCountMismatch { .. })
        ));
        for response in [json!({"total":4}), json!({"total":4,"complete":false})] {
            assert!(matches!(
                McpCollectionProgress::default().observe(&contract, &response, 2, true),
                Err(McpExecutionError::IncompleteCollection { .. })
            ));
        }
        assert!(matches!(
            McpCollectionProgress::default().observe(
                &contract,
                &json!({"total":5,"complete":true}),
                2,
                true
            ),
            Err(McpExecutionError::CollectionLimit { .. })
        ));
        assert!(matches!(
            McpCollectionProgress::default().observe(
                &contract,
                &json!({"total":"4","complete":true}),
                2,
                true
            ),
            Err(McpExecutionError::InvalidCollectionTotal { .. })
        ));
    }

    #[test]
    fn embedded_resources_preserve_contents_and_reject_wrong_shapes() {
        let mapping = McpResponse {
            source: McpResultSource::EmbeddedResource { index: 1 },
            pointer: "".into(),
            exhaustive: true,
        };
        for resource in [
            json!({"uri":"test://file", "mimeType":"text/plain", "text":"exact\ntext"}),
            json!({"uri":"test://file", "mimeType":"application/octet-stream", "blob":"AAEC"}),
        ] {
            let envelope = json!({"content":[{"type":"text","text":"description"},
                {"type":"resource","resource":resource.clone()}]});
            assert_eq!(project_response(envelope, &mapping).unwrap(), resource);
        }
        for block in [
            json!({"type":"resource_link", "uri":"test://file"}),
            json!({"type":"resource", "resource":{"text":"missing uri"}}),
            json!({"type":"resource", "resource":{"uri":"test://file","text":"x","blob":"eA=="}}),
        ] {
            assert!(matches!(
                project_response(json!({"content":[{}, block]}), &mapping),
                Err(McpExecutionError::EmbeddedResourceRequired { index: 1 })
            ));
        }
    }

    #[test]
    fn explicit_projection_distinguishes_empty_missing_and_malformed() {
        let mut mapping = McpResponse {
            source: McpResultSource::StructuredContent,
            pointer: "/items".into(),
            exhaustive: false,
        };
        assert_eq!(
            project_response(json!({"structuredContent":{"items":[]}}), &mapping).unwrap(),
            json!([])
        );
        assert!(matches!(
            project_response(json!({"content":[]}), &mapping),
            Err(McpExecutionError::MissingStructuredContent)
        ));
        assert!(matches!(
            project_response(json!({"structuredContent":{}}), &mapping),
            Err(McpExecutionError::MissingPointer { .. })
        ));
        mapping.source = McpResultSource::JsonText { index: 1 };
        let envelope = json!({"content":[{"type":"text","text":"prose"},{"type":"text","text":"{\"items\":[{\"id\":7}]}"}]});
        assert_eq!(
            project_response(envelope, &mapping).unwrap(),
            json!([{"id":7}])
        );
        mapping.source = McpResultSource::JsonText { index: 0 };
        assert!(matches!(
            project_response(
                json!({"content":[{"type":"text","text":"failed"}]}),
                &mapping
            ),
            Err(McpExecutionError::InvalidJsonText { .. })
        ));
        assert!(matches!(
            project_response(
                json!({"content":[{"type":"resource_link","uri":"test://x"}]}),
                &mapping
            ),
            Err(McpExecutionError::TextBlockRequired { .. })
        ));
    }
}
