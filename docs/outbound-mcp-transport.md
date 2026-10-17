# Outbound MCP transport foundation

`plasm-runtime::mcp_transport` provides an initialized, network-only Streamable
HTTP client using the existing pinned `rust-mcp-sdk 0.9.0`. The runtime enables
its client feature; no dependency version was upgraded. There is no subprocess
configuration, stdio feature request, older SSE fallback or transport probing.

The host supplies `McpHttpConfig` (endpoint, headers, deadline), opens an
`McpSession`, then calls `list_tools` and `call_tool`. Discovery returns one page
with its cursor intact. Tool results retain the SDK's typed full envelope;
`isError: true` is a received tool failure, whereas `McpTransportError::Protocol`
retains the typed SDK cause. No JSON-looking text is implicitly converted to rows.

The supported protocol version is the pinned SDK's `2025-11-25`; initialization
rejects incompatible versions. Protocol-owned headers cannot be overridden by
host credentials. The client advertises no sampling, elicitation or task support,
and rejects task-augmented calls before dispatch. Requests have bounded waiting
and no automatic tool retry. A timeout retires the connection; it does not prove
that the remote operation did not execute. `shutdown` closes local client
resources; it does not certify remote-session deletion or rollback. Drop cleanup
is best-effort when a Tokio runtime remains available; use explicit shutdown.

## Executable protocol evidence

```
cargo test -p plasm-runtime --test mcp_http_protocol --offline
```

The independent TCP peer parses actual HTTP requests and emits JSON-RPC, JSON
responses and SSE response frames. It does not mock a transport trait or use the
SDK server. Tests cover initialization ordering, session/version headers, both
response encodings, paginated tool discovery, content/metadata preservation,
tool versus RPC errors, closed-session rejection and one tool POST after HTTP
failure. Broader adversarial protocol cases remain in the integration ladder.

## Remaining boundary

This is a transport component, not yet CML/engine integration. Still required:
an explicit MCP mapping/compiled operation, host-owned named server bindings,
execution dispatch, fingerprinting, typed result projection and abstract
compiler-to-wire fixtures. Commands and credentials must not enter catalogues.

The MCP-Universe gateway at the surveyed revision serves older HTTP/SSE endpoints,
not Streamable HTTP. No live deployment was connected. See the
[catalogue family and endpoint evidence](../apis/mcp-universe/README.md) and
[integration test ladder](../apis/mcp-universe/INTEGRATION-TESTING.md).
