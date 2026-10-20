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

#[tokio::test]
async fn catalog_query_dispatches_over_protocol_and_keeps_partial_coverage() {
    assert_catalog_coverage(false, false, false).await;
}

#[tokio::test]
async fn catalog_query_dispatches_over_protocol_and_keeps_exhaustive_coverage() {
    assert_catalog_coverage(true, false, false).await;
}

#[tokio::test]
async fn catalog_pagination_preserves_wire_numbers_and_coverage() {
    assert_catalog_coverage(true, true, false).await;
    assert_catalog_coverage(false, true, false).await;
}

#[tokio::test]
async fn catalog_cursor_pagination_reads_provider_envelope_before_projection() {
    assert_catalog_coverage(true, true, true).await;
}

async fn assert_catalog_coverage(exhaustive: bool, paginated: bool, cursor: bool) {
    use plasm_core::{Expr, QueryExpr};
    use plasm_runtime::mcp_execution::McpBindings;
    use plasm_runtime::{
        ExecuteOptions, ExecutionConfig, ExecutionEngine, SessionMaterialization, StreamConsumeOpts,
    };
    let peer = Peer::start(false, false).await;
    let session = McpSession::connect_http(McpHttpConfig {
        url: peer.url.clone(),
        headers: HashMap::new(),
        deadline: Duration::from_secs(3),
    })
    .await
    .unwrap();
    let fixture = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("test runner must set CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/mcp_transport_matrix");
    let temporary = tempfile::tempdir().unwrap();
    std::fs::copy(
        fixture.join("domain.yaml"),
        temporary.path().join("domain.yaml"),
    )
    .unwrap();
    let mut mappings = std::fs::read_to_string(fixture.join("mappings.yaml")).unwrap();
    if paginated {
        mappings = mappings.replace("  tool: list_items", "  tool: list_items\n  pagination:\n    strategy: page_number\n    params:\n      page: {counter: 1}\n      size: {fixed: 1, role: page_size}");
    }
    if cursor {
        mappings = mappings
            .replace("tool: list_items", "tool: cursor_items")
            .replace("strategy: page_number", "strategy: cursor")
            .replace("page: {counter: 1}", "cursor: {from_response: next}");
    }
    if paginated && exhaustive {
        mappings = mappings.replace("  pagination:", "  collection: {total_pointer: /total, complete_pointer: /complete, complete_value: true, max_items: 2}\n  pagination:");
    }
    std::fs::write(
        temporary.path().join("mappings.yaml"),
        if exhaustive {
            mappings.replace("exhaustive: false", "exhaustive: true")
        } else {
            mappings
        },
    )
    .unwrap();
    let cgs = plasm_core::loader::load_schema_dir(temporary.path()).unwrap();
    let engine = ExecutionEngine::new(ExecutionConfig {
        hydrate: false,
        ..Default::default()
    })
    .unwrap()
    .with_mcp_bindings(McpBindings::new(HashMap::from([(
        "fixture".into(),
        session,
    )])));
    let opts = ExecuteOptions {
        compiled_catalog: Some(Arc::new(
            plasm_compile::compile_cgs_capability_templates(&cgs).unwrap(),
        )),
        ..Default::default()
    };
    let mut materialization = SessionMaterialization::new();
    let result = engine
        .execute(
            &Expr::Query(QueryExpr::all("Item")),
            &cgs,
            &mut materialization,
            None,
            StreamConsumeOpts {
                fetch_all: paginated,
                ..Default::default()
            },
            opts.clone(),
        )
        .await
        .unwrap();
    assert_eq!(result.entities().len(), if paginated { 2 } else { 1 });
    assert_eq!(
        result.coverage(),
        if exhaustive {
            plasm_core::collection_codec::ResultCoverage::Complete
        } else {
            plasm_core::collection_codec::ResultCoverage::Unknown
        }
    );
    let requests = peer.requests.lock().unwrap().clone();
    let calls: Vec<_> = requests
        .iter()
        .filter(|(_, request)| request["method"] == "tools/call")
        .collect();
    assert_eq!(
        calls.len(),
        if cursor || (paginated && exhaustive) {
            2
        } else if paginated {
            3
        } else {
            1
        }
    );
    assert_eq!(
        calls[0].1["params"]["name"],
        if cursor { "cursor_items" } else { "list_items" }
    );
    if cursor {
        assert_eq!(calls[0].1["params"]["arguments"], json!({"size":1}));
        assert_eq!(
            calls[1].1["params"]["arguments"],
            json!({"size":1,"cursor":"second"})
        );
    } else if paginated {
        for (index, call) in calls.iter().enumerate() {
            assert_eq!(
                call.1["params"]["arguments"],
                json!({"page":index + 1,"size":1})
            );
        }
    } else {
        assert_eq!(calls[0].1["params"]["arguments"], json!({}));
    }
    drop(requests);
    if paginated && exhaustive {
        let mut resumed_materialization = SessionMaterialization::new();
        let first = engine
            .execute(
                &Expr::Query(QueryExpr::all("Item")),
                &cgs,
                &mut resumed_materialization,
                None,
                StreamConsumeOpts::default(),
                opts.clone(),
            )
            .await
            .unwrap();
        assert_eq!(first.entities().len(), 1);
        let second = engine
            .execute_pagination_resume(
                *first.pagination_resume.unwrap(),
                &cgs,
                &mut resumed_materialization,
                None,
                StreamConsumeOpts::default(),
                opts.clone(),
            )
            .await
            .unwrap();
        assert_eq!(second.entities().len(), 1);
        assert!(!second.has_more);
        assert!(second.pagination_resume.is_none());
    }
    let journal = plasm_runtime::MutationJournal::default();
    let error = journal
        .scope(engine.execute(
            &Expr::Invoke(plasm_core::InvokeExpr::new("fail_write", "Item", "7", None)),
            &cgs,
            &mut materialization,
            None,
            StreamConsumeOpts::default(),
            opts,
        ))
        .await
        .unwrap_err();
    assert_eq!(journal.snapshot().len(), 1, "{error:?}");
    assert_eq!(
        journal.snapshot()[0].status,
        plasm_runtime::MutationDispatchStatus::ResponseReceived
    );
    let failure = plasm_runtime::ExecutionFailure::from(error);
    assert_eq!(failure.code, "mcp_tool_failure");
    assert_eq!(
        failure.mcp_tool_result.as_ref().unwrap()["_meta"]["receipt"],
        "kept"
    );
}

#[tokio::test]
async fn bound_dispatch_retains_tool_failure_and_does_not_retry() {
    use plasm_runtime::mcp_execution::{McpBindings, McpExecutionError};
    let peer = Peer::start(false, false).await;
    let session = McpSession::connect_http(McpHttpConfig {
        url: peer.url.clone(),
        headers: HashMap::new(),
        deadline: Duration::from_secs(3),
    })
    .await
    .unwrap();
    let bindings = McpBindings::new(HashMap::from([("fixture".into(), session)]));
    let call = plasm_compile::CompiledMcpCall {
        server: "fixture".into(),
        tool: "tool_error".into(),
        arguments: serde_json::Map::new(),
    };
    let McpExecutionError::ToolFailure { result, .. } = bindings.execute(&call).await.unwrap_err()
    else {
        panic!("typed tool failure expected")
    };
    assert_eq!(result.content.len(), 2);
    assert_eq!(result.meta.unwrap()["receipt"], "kept");
    assert_eq!(
        peer.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, r)| r["method"] == "tools/call")
            .count(),
        1
    );
    fn assert_shared<T: Send + Sync>() {}
    assert_shared::<plasm_runtime::RuntimeError>();
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Peer {
    async fn start(sse: bool, fail_call: bool) -> Self {
        Self::start_with_spacing(sse, fail_call, true).await
    }

    async fn start_with_spacing(sse: bool, fail_call: bool, spaced: bool) -> Self {
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
                } else if request["method"] == "tools/call"
                    && request["params"]["name"] == "expired_session"
                {
                    ("404 Not Found", "application/json", "{}".into(), "")
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
                    } else if request["method"] == "server/discover" {
                        json!({"resultType": "complete", "supportedVersions": ["2026-07-28"], "capabilities": {"tools": {}}, "ttlMs": 0, "cacheScope": "private", "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "wire-peer", "version": "1"}}})
                    } else if request["method"] == "tools/list" {
                        if request["params"]["cursor"] == "page-2" {
                            json!({"tools": [{"name": "second", "inputSchema": {"type": "object"}}]})
                        } else {
                            json!({"tools": [{"name": "first", "inputSchema": {"type": "object"}}], "nextCursor": "page-2"})
                        }
                    } else if request["params"]["name"] == "cursor_items" {
                        let second = request["params"]["arguments"]["cursor"] == "second";
                        json!({"content":[],"structuredContent":{"items":[{"id":if second {8} else {7}}],"next":if second {None} else {Some("second")},"total":2,"complete":true},"isError":false})
                    } else if let Some(page) = request["params"]["arguments"]["page"].as_u64() {
                        let items = if page < 3 {
                            json!([{"id":page + 6}])
                        } else {
                            json!([])
                        };
                        json!({"content":[],"structuredContent":{"items":items,"total":2,"complete":true},"isError":false})
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
                            if spaced {
                                format!("event: message\ndata: {response}\n\n")
                            } else {
                                format!("event:message\ndata:{response}\n\n")
                            },
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
        .list_tools(Some(
            PaginatedRequestParams::default().with_cursor(first.next_cursor),
        ))
        .await
        .unwrap();
    assert_eq!(second.tools[0].name, "second");
    assert!(second.next_cursor.is_none());
    let params =
        |name: &str| CallToolRequestParams::new(name.to_owned()).with_arguments(Default::default());
    let result = session.call_tool(params("tool_error")).await.unwrap();
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.content.len(), 2);
    assert_eq!(result.meta.unwrap()["receipt"], "kept");
    assert!(matches!(
        session.call_tool(params("rpc_error")).await,
        Err(McpTransportError::Protocol {
            source,
            ..
        }) if matches!(source, plasm_runtime::mcp_transport::McpProtocolError::Service(rmcp::ServiceError::McpError(_)))
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
async fn streamable_http_sse_accepts_fields_without_optional_space() {
    let peer = Peer::start_with_spacing(true, false, false).await;
    let mut session = McpSession::connect_http(McpHttpConfig {
        url: peer.url.clone(),
        headers: HashMap::new(),
        deadline: Duration::from_secs(2),
    })
    .await
    .expect("SSE fields without optional whitespace must initialize");
    assert_eq!(
        session.list_tools(None).await.unwrap().tools[0].name,
        "first"
    );
    session.shutdown().await.unwrap();
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
            .call_tool(CallToolRequestParams::new("read").with_arguments(Default::default()))
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
        .call_tool(CallToolRequestParams::new("write"))
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

#[tokio::test]
async fn explicit_2026_discovery_does_not_initialize() {
    use plasm_runtime::mcp_transport::McpHttpProtocol;
    let peer = Peer::start(false, false).await;
    let mut session = McpSession::connect_http_with_protocol(
        McpHttpConfig {
            url: peer.url.clone(),
            headers: HashMap::new(),
            deadline: Duration::from_secs(3),
        },
        McpHttpProtocol::Discover2026,
    )
    .await
    .unwrap();
    session
        .call_tool(CallToolRequestParams::new("read"))
        .await
        .unwrap();
    session.shutdown().await.unwrap();
    let requests = peer.requests.lock().unwrap();
    assert_eq!(requests[0].1["method"], "server/discover");
    assert!(!requests
        .iter()
        .any(|(_, message)| message["method"] == "initialize"));
    let (headers, call) = requests
        .iter()
        .find(|(_, message)| message["method"] == "tools/call")
        .unwrap();
    assert!(headers
        .to_lowercase()
        .contains("mcp-protocol-version: 2026-07-28"));
    assert_eq!(
        call["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        "2026-07-28"
    );
}

#[tokio::test]
async fn expired_session_does_not_reinitialize_or_replay_mutation() {
    let peer = Peer::start(false, false).await;
    let mut session = McpSession::connect_http(McpHttpConfig {
        url: peer.url.clone(),
        headers: HashMap::new(),
        deadline: Duration::from_secs(2),
    })
    .await
    .unwrap();
    assert!(session
        .call_tool(CallToolRequestParams::new("expired_session"))
        .await
        .is_err());
    let requests = peer.requests.lock().unwrap();
    for method in ["initialize", "tools/call"] {
        assert_eq!(
            requests
                .iter()
                .filter(|(_, message)| message["method"] == method)
                .count(),
            1,
            "{method} must not be replayed"
        );
    }
}
