# Integration test ladder

This is the acceptance plan, not a claim that catalogue tests already exist.
The executable foundation is `crates/plasm-runtime/tests/mcp_http_protocol.rs`:
a real TCP peer serving JSON-RPC over HTTP, independently of the client SDK.

## 0. Pin independent contracts

For each server record the Universe commit, resolved package version or image
digest, network protocol/version, paginated `tools/list` output and schema hash.
Capture representative tool results from pinned upstream tests/source or a
permitted sandbox; record their origin. A catalogue-authored response is not
independent conformance evidence. Never derive fixtures from benchmark answers.
Unversioned npm declarations must be resolved before mappings are accepted.

Retain complete MCP envelopes: content blocks, `structuredContent`, `isError`,
metadata and RPC errors. Redact credentials and private data. A string containing
JSON requires an explicit, versioned mapping decoder; do not guess from content.

## 1. Protocol conformance, no catalogue

Drive the runtime through real loopback HTTP and an independently implemented
MCP peer. Require initialization/version negotiation, initialized notification,
session headers, JSON and SSE responses, listing cursors, tool/RPC/HTTP failures,
malformed replies, mismatched IDs, deadlines and shutdown. Assert exact request
counts: no tool replay after uncertain failure. Exercise full content retention.
HTTP/SSE and Streamable HTTP require distinct protocol tests if both are enabled.

## 2. Abstract compiler-to-wire contract

Add a minimal `fixtures/schemas/mcp_transport_matrix` when MCP CML dispatch exists.
Use abstract Container → Item → Annotation entities, not live vendor catalogues.
Run a complete Python Program through admission, compilation, execution over the
real protocol peer, decoding and final presentation. Capture JSON-RPC frames and
assert tool names, evaluated arguments, parent identities, row types, coverage and
typed failure causes. Rejected programs must send no tool calls.

Include colliding child identifiers under different parents; reordered/nested
results; missing identity fields; empty success versus failed read; partial
effects followed by failure; and stale-read prevention after mutation. Core
changes require a failing abstract fixture, not a vendor-specific condition.

## 3. Catalogue-specific protocol integration

Explicit catalogue integration tests may load `apis/mcp-universe/<server>`.
Compile actual served Python signatures and send their calls to an independent
stateful MCP peer implementing the pinned subset. Assert the outbound transcript
and decoded result together. Use upstream contract fixtures as the wire oracle.

| Catalogue | Minimal state/fixture | Assertions |
|---|---|---|
| GitHub | Two repositories, colliding issue numbers, two pages, comments and one write | Repository scope survives selection; continuation is honest; one admitted mutation; follow-up reads observe the write; a failed later call preserves earlier effects |
| Maps | Two origins × two destinations; one failed matrix element; detail response omits ID | Both indices and requested identity survive; failure is not zero distance; bounded search/route output is not advertised as exhaustive |
| Finance | Two instruments sharing an expiry; calls/puts; missing quote; malformed JSON text and ordinary error string | Expiry and instrument remain attached; null is not zero; errors are not empty chains; variable metrics decode explicitly; no invented common snapshot |

Do not use an HTTP OpenAPI/Hermit mock as proof of the MCP envelope. Hermit
remains useful only for a separately declared vendor HTTP boundary. These tests
exercise MCP itself.

## 4. Pinned real-server smoke

Run the actual benchmark server behind the chosen network endpoint in isolated
infrastructure. Use read-only requests first. For write coverage use a dedicated
sandbox, unique resources, exact reread verification and recorded cleanup. Keep
benchmark permissions/callbacks/tracing in the execution path. Record unresolved
cleanup rather than hiding it. No production writes or new model calls are
authorized by this test plan.

Compare canonical semantic fields, identity and declared coverage, not volatile
timestamps, live prices or prose byte-for-byte. Include a transport trace proving
no direct vendor bypass. A passing synthetic peer does not substitute for this.

## Evidence and promotion

Each run records source/catalogue/schema hashes, command, tier, cases attempted,
passed/failed/skipped, request counts, typed failures and cleanup. Unavailable
credentials or network endpoints are explicit skips. Promote only the exercised
capabilities after schema validation, abstract dispatch tests, catalogue tests
and real-server smoke pass. Benchmark scoring is a separate later gate; do not
use benchmark task solutions to author this ladder.
