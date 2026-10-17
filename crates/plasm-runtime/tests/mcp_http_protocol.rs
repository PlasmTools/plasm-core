//! A TCP-level peer, deliberately independent of the MCP SDK server.
use plasm_runtime::mcp_transport::{CallToolRequestParams, McpHttpConfig, McpSession};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

struct Peer {
    url: url::Url,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Peer {
    async fn start(sse: bool, fail_call: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let header_end = loop {
                    let mut byte = [0u8];
                    if stream.read_exact(&mut byte).await.is_err() {
                        break None;
                    }
                    bytes.push(byte[0]);
                    if bytes.ends_with(b"\r\n\r\n") {
                        break Some(bytes.len());
                    }
                    assert!(bytes.len() < 65536);
                };
                let Some(header_end) = header_end else {
                    continue;
                };
                let headers = String::from_utf8(bytes).unwrap();
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .map(|v| v.parse().unwrap())
                    .unwrap_or(0);
                assert!(length < 65536);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.unwrap();
                let request: Value = if body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&body).unwrap()
                };
                let _ = header_end;
                captured
                    .lock()
                    .unwrap()
                    .push((headers.clone(), request.clone()));
                let (status, content_type, payload, extra) = if headers.starts_with("GET ") {
                    (
                        "405 Method Not Allowed",
                        "application/json",
                        String::new(),
                        "",
                    )
                } else if request["method"] == "notifications/initialized" {
                    ("202 Accepted", "application/json", String::new(), "")
                } else if request["method"] == "tools/call" && fail_call {
                    (
                        "503 Service Unavailable",
                        "application/json",
                        "{}".into(),
                        "",
                    )
                } else {
                    let result = if request["method"] == "initialize" {
                        json!({"protocolVersion": request["params"]["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "wire-peer", "version": "1"}})
                    } else if request["method"] == "tools/list" {
                        if request["params"]["cursor"] == "page-2" {
                            json!({"tools": [{"name": "second", "inputSchema": {"type": "object"}}]})
                        } else {
                            json!({"tools": [{"name": "first", "inputSchema": {"type": "object"}}], "nextCursor": "page-2"})
                        }
                    } else {
                        json!({"content": [{"type": "text", "text": "kept"}, {"type": "resource_link", "uri": "test://result/1", "name": "result"}], "structuredContent": {"items": [{"id": 7}]}, "isError": request["params"]["name"] == "tool_error", "_meta": {"receipt": "kept"}})
                    };
                    let response = if request["params"]["name"] == "rpc_error" {
                        json!({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32602, "message": "invalid input", "data": {"field": "id"}}})
                    } else {
                        json!({"jsonrpc": "2.0", "id": request["id"], "result": result})
                    }
                        .to_string();
                    if sse {
                        (
                            "200 OK",
                            "text/event-stream",
                            format!("event: message\ndata: {response}\n\n"),
                            "Mcp-Session-Id: fixture-session\r\n",
                        )
                    } else {
                        (
                            "200 OK",
                            "application/json",
                            response,
                            "Mcp-Session-Id: fixture-session\r\n",
                        )
                    }
                };
                let response = format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{payload}", payload.len());
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }
}

#[tokio::test]
async fn pagination_and_tool_errors_preserve_protocol_data() {
    use plasm_runtime::mcp_transport::{McpTransportError, PaginatedRequestParams};
    let peer = Peer::start(false, false).await;
    let mut session = McpSession::connect_http(McpHttpConfig {
        url: peer.url.clone(),
        headers: HashMap::new(),
        deadline: Duration::from_secs(3),
    })
    .await
    .unwrap();
    let first = session.list_tools(None).await.unwrap();
    assert_eq!(first.tools[0].name, "first");
    assert_eq!(first.next_cursor.as_deref(), Some("page-2"));
    let second = session
        .list_tools(Some(PaginatedRequestParams {
            cursor: first.next_cursor,
            meta: None,
        }))
        .await
        .unwrap();
    assert_eq!(second.tools[0].name, "second");
    assert!(second.next_cursor.is_none());
    let params = |name: &str| CallToolRequestParams {
        name: name.into(),
        arguments: Some(Default::default()),
        meta: None,
        task: None,
    };
    let result = session.call_tool(params("tool_error")).await.unwrap();
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.content.len(), 2);
    assert_eq!(result.meta.unwrap()["receipt"], "kept");
    assert!(matches!(
        session.call_tool(params("rpc_error")).await,
        Err(McpTransportError::Protocol {
            source: rust_mcp_sdk::error::McpSdkError::RpcError(_),
            ..
        })
    ));
    session.shutdown().await.unwrap();
    assert!(matches!(
        session.call_tool(params("read")).await,
        Err(McpTransportError::Closed)
    ));
    assert_eq!(
        peer.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, message)| message["method"] == "tools/call")
            .count(),
        2
    );
}

#[tokio::test]
async fn streamable_http_json_and_sse_preserve_session_and_version() {
    for sse in [false, true] {
        let peer = Peer::start(sse, false).await;
        let mut session = McpSession::connect_http(McpHttpConfig {
            url: peer.url.clone(),
            headers: HashMap::new(),
            deadline: Duration::from_secs(3),
        })
        .await
        .unwrap();
        let result = session
            .call_tool(CallToolRequestParams {
                name: "read".into(),
                arguments: Some(Default::default()),
                meta: None,
                task: None,
            })
            .await
            .unwrap();
        assert_eq!(
            result.structured_content.unwrap()["items"],
            json!([{"id": 7}])
        );
        session.shutdown().await.unwrap();
        let requests = peer.requests.lock().unwrap();
        let posts: Vec<_> = requests
            .iter()
            .filter(|(head, _)| head.starts_with("POST "))
            .collect();
        assert_eq!(posts[0].1["method"], "initialize");
        assert_eq!(posts[1].1["method"], "notifications/initialized");
        assert_eq!(posts[2].1["method"], "tools/call");
        assert_eq!(posts.len(), 3);
        for (head, _) in posts.iter().skip(1) {
            let head = head.to_lowercase();
            assert!(head.contains("mcp-session-id: fixture-session"));
            assert!(head.contains("mcp-protocol-version: 2025-11-25"));
        }
    }
}

#[tokio::test]
async fn http_failure_does_not_replay_tool_post() {
    let peer = Peer::start(false, true).await;
    let mut session = McpSession::connect_http(McpHttpConfig {
        url: peer.url.clone(),
        headers: HashMap::new(),
        deadline: Duration::from_millis(500),
    })
    .await
    .unwrap();
    assert!(session
        .call_tool(CallToolRequestParams {
            name: "write".into(),
            arguments: None,
            meta: None,
            task: None
        })
        .await
        .is_err());
    session.shutdown().await.unwrap();
    assert_eq!(
        peer.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, message)| message["method"] == "tools/call")
            .count(),
        1
    );
}
