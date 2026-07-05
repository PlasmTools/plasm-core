//! Shared MCP Streamable HTTP SSE helpers for e2e tests.

use reqwest::Client;
use serde_json::{json, Value};

pub async fn mcp_sse_json_by_id(
    client: &Client,
    base: &str,
    mcp_session: &str,
    body: Value,
    id: u64,
) -> Value {
    let resp = client
        .post(format!("{base}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("MCP-Session-Id", mcp_session)
        .json(&body)
        .send()
        .await
        .expect("mcp request");
    let text = resp.text().await.expect("mcp body");
    parse_sse_result_for_id(&text, id)
}

pub fn parse_sse_result_for_id(text: &str, id: u64) -> Value {
    let mut result = json!({});
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(j) = serde_json::from_str::<Value>(rest) else {
            continue;
        };
        if j.get("id").and_then(|v| v.as_u64()) == Some(id) {
            result = j.get("result").cloned().unwrap_or(json!({}));
            break;
        }
    }
    result
}

pub async fn mcp_tool_meta(
    client: &Client,
    base: &str,
    mcp_session: &str,
    tool: &str,
    args: Value,
    id: u64,
) -> Value {
    let result = mcp_sse_json_by_id(
        client,
        base,
        mcp_session,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args }
        }),
        id,
    )
    .await;
    let meta = result.get("_meta").cloned().unwrap_or(json!({}));
    let structured = result
        .get("structuredContent")
        .or_else(|| result.get("structured_content"))
        .cloned()
        .unwrap_or(json!({}));
    json!({ "_meta": meta, "structuredContent": structured, "mcp_result": result })
}

pub async fn mcp_read_resource_json(
    client: &Client,
    base: &str,
    mcp_session: &str,
    uri: &str,
    id: u64,
) -> Value {
    let result = mcp_sse_json_by_id(
        client,
        base,
        mcp_session,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "resources/read",
            "params": { "uri": uri }
        }),
        id,
    )
    .await;
    let raw = result
        .pointer("/contents/0/text")
        .and_then(|v| v.as_str())
        .expect("resource read text payload");
    serde_json::from_str(raw).expect("resource read json")
}
