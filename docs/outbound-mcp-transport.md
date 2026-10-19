# Outbound MCP transport foundation

`plasm-runtime::mcp_transport` provides an initialized, network-only Streamable
HTTP client using the pinned `rmcp 3.5.1`. The runtime enables
its client and reqwest Streamable HTTP features. There is no subprocess
configuration, stdio feature request, older SSE fallback or transport probing.

The host supplies `McpHttpConfig` (endpoint, headers, deadline), opens an
`McpSession`, then calls `list_tools` and `call_tool`. Discovery returns one page
with its cursor intact. Tool results retain the SDK's typed full envelope;
`isError: true` is a received tool failure, whereas `McpTransportError::Protocol`
retains the typed SDK cause. No JSON-looking text is implicitly converted to rows.

The host selects `McpHttpProtocol::Initialize2025` (`2025-11-25`) or
`Discover2026` (`2026-07-28`). `connect_http` selects the former; explicit
selection uses `connect_http_with_protocol`. Failure never selects another
protocol. Protocol-owned headers cannot be overridden by
host credentials. The client advertises no sampling, elicitation or task support,
and returns `UnfinishedTool` with the complete response if a tool does not finish,
without dispatching a follow-up. Requests have bounded waiting
and no automatic tool retry. A timeout retires the connection; it does not prove
that the remote operation did not execute. `shutdown` closes local client
resources; it does not certify remote-session deletion or rollback. Use explicit
shutdown to observe cleanup errors.

## Executable protocol evidence

```
cargo test -p plasm-runtime --test mcp_http_protocol --offline
```

Validation for the declarative dispatch slice: 50 CML tests, five TCP protocol
tests, and the explicit response-projection unit test passed. The protocol suite
includes a catalogue query and a failed action with mutation-journal and retained
failure-envelope assertions. These counts do not include a live benchmark server
or a Python-program integration test.

The independent TCP peer parses actual HTTP requests and emits JSON-RPC, JSON
responses and SSE response frames. It does not mock a transport trait or use the
SDK server. Tests cover initialization ordering, session/version headers, both
response encodings, paginated tool discovery, content/metadata preservation,
tool versus RPC errors, closed-session rejection and one tool POST after HTTP
failure. Broader adversarial protocol cases remain in the integration ladder.

## Declarative dispatch

CML accepts an explicit MCP mapping:

```yaml
transport: mcp
server: fixture
tool: list_items
arguments:
  type: object
  fields: []
response:
  source: {kind: structured_content}
  pointer: /items
  exhaustive: false
```

The host initializes sessions and installs `McpBindings` on `ExecutionEngine`.
`server` identifies a binding; it is not a URL or process command. Argument
expressions must evaluate to an object. Response projection chooses
`structured_content`, `json_text` or `embedded_resource`; the latter two require
an explicit content-block `index`. Projection then
applies a JSON Pointer. Missing data, malformed JSON and tool errors fail with
typed causes; none becomes an empty successful collection.

Query projections must be arrays. `exhaustive: false` records unknown coverage
and does not populate an exhaustive query index. `true` is an author assertion
that the declared query is exhausted, not that one server page succeeded.
MCP collection pagination uses the shared bounded pagination driver. Declare a
`pagination` block with `strategy: page_number`, `offset` or `cursor`, and typed
`params`. The default `location: query` means tool argument members for MCP;
HTTP body, link-header and next-URL locations are rejected. Cursor fields are
read from the selected provider envelope before the response pointer projects
its row array. Exhausting pages cannot upgrade `exhaustive: false` to complete.
Failed later pages fail the execution; they do not yield a successful aggregate.
Discovery pagination (`tools/list`) remains a separate protocol operation.

A mapping can additionally require provider collection evidence:

```yaml
collection:
  total_pointer: /total
  complete_pointer: /complete
  complete_value: true
  max_items: 500
```

This requires pagination and `response.exhaustive: true`. Every selected provider
envelope must carry the declared completeness value and a stable unsigned total
within the enumeration bound. Traversal stops when the observed row count equals
that total, even if the final page is full. Missing evidence, changing totals,
over-delivery and premature exhaustion are typed failures. Count progress and
the initial total survive host continuation snapshots. These checks certify the
enumeration against provider evidence; they do not create snapshot isolation
for a live backend or rewrite queries to circumvent provider limits.

```yaml
pagination:
  strategy: page_number
  params:
    page: {counter: 1}
    per_page: {fixed: 100, role: page_size}
```

`embedded_resource` selects a resource block with a URI and exactly one of
`text` or `blob`, preserving MIME type and the original content. Blob bytes
remain base64. Resource links are not fetched; prose and directory JSON are not
silently interpreted as embedded file contents.

Receiver identity slots are restored through the declared `IdentityCodec`
before CML compilation. Numeric identifiers become JSON numbers while string
identifiers retain their bytes, including leading zeroes. This rule applies to
Get, Update, Delete and entity-receiver actions, independently of transport.
Receiverless payloads are not inferred to be identities from their field names.

The abstract `mcp_transport_matrix` fixture exercises catalogue compilation and
runtime query dispatch through the independent TCP peer. Python admission,
parent/child identity, mutation accounting and hosted binding configuration
still require their integration-ladder gates. Bindings and materializations
must remain scoped to the same host principal; named recipe fingerprints alone
do not establish credential identity across hosts. Production catalogue
promotion is pending those gates and pinned upstream server contracts.

SDK causes remain typed in `McpProtocolError` variants for initialization,
service and shutdown failures. Tool-failure envelopes are retained as
execution-failure data.

The MCP-Universe gateway at the surveyed revision serves older HTTP/SSE endpoints,
not Streamable HTTP. No live deployment was connected. See the
[catalogue family and endpoint evidence](../apis/mcp-universe/README.md) and
[integration test ladder](../apis/mcp-universe/INTEGRATION-TESTING.md).
