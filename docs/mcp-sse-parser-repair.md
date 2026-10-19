# MCP SSE parser repair

Status: historical diagnosis, superseded by the authorized full rmcp 3.5.1
cutover. The unspaced SSE regression and SDK-to-Plasm probe now pass. See
[MCP SDK cutover](mcp-sdk-cutover.md). The vendoring proposal below was not used.

The existing `rust-mcp-transport 0.9.1` SSE parser accepts `data: VALUE`
but rejects `data:VALUE`. The space after the colon is optional under the
[SSE interpretation rules](https://html.spec.whatwg.org/multipage/server-sent-events.html#event-stream-interpretation).
The same restriction affects `event`, `id`, and `retry` fields.

Minimal protocol reproduction (independent TCP peer, no Plasm host, Python worker,
database or model):

```sh
cargo test -p plasm-runtime --test mcp_http_protocol \
  streamable_http_sse_accepts_fields_without_optional_space --offline
```

Observed: fails in 0.01 seconds during initialization with transport
`Internal("Stream has ended.")`. The spaced-field protocol case passes.
The larger SDK-to-Plasm probe fails on `tools/list` after its 30-second deadline;
an independent HTTP client reaches the same host successfully and its tool list
deserializes into the SDK schema.

Source inspection also identifies incomplete-chunk byte loss: after extracting
complete lines, `process_new_chunk` clears the remaining buffer before restoring
unfinished event lines, discarding a trailing partial line. This needs its own
red regression before repair.

Proposed repair boundary:

1. Vendor the current transport 0.9.1 source with license and provenance, retaining
   its version and dependency requirements. Patch both workspace manifests to the
   same source; do not edit the machine's Cargo registry cache.
2. Correct field parsing and incremental framing in that transport. Test optional
   spacing, fragmented fields/UTF-8, LF/CRLF/CR, multiple events, empty priming
   events and retained IDs/retry values. Preserve the MCP byte-stream framing
   contract when forwarding decoded payloads.
3. Run parser and TCP protocol suites, then the original SDK-to-Plasm probe.
   Make successful SDK initialization/listing/execution an ordinary regression
   gate, rather than an opt-in probe.
4. Rerun Python/discovery integration, remove diagnostic instrumentation and
   update the integration report with measured outcomes.

Do not modify server output to accommodate the broken client, silently switch
transports, retry operations, or claim that the larger timeout is repaired until
the original reproduction passes.
