# MCP SDK cutover

The authorized target is one SDK, `rmcp = 3.5.1`, for Plasm's inbound
Streamable HTTP service and outbound catalog transport. No stdio support or
automatic protocol fallback is introduced.

## Verified outbound boundary

`plasm-runtime` uses the official client, with explicit 2025 initialization or
2026 discovery. Expired-session reinitialization and tool-call replay are
disabled. Calls use the one-exchange API; unfinished results remain typed
outcomes rather than triggering sampling, elicitation or another tool POST.
The runtime retains complete tool results, including errors and metadata.
Automatic SSE reconnection is disabled as well.

Independent TCP fixtures cover JSON and SSE responses, SSE fields without
optional whitespace, cursor propagation, 2026 request metadata, errors,
mutation accounting and no replay following an expired session.

Validation: eight `mcp_http_protocol` tests pass, runtime test targets type-check,
and the existing agent-core library type-checks against the changed runtime.
No model-behaviour or production deployment claim follows from these checks.

## Inbound boundary

The server now uses native rmcp models and dispatch. Both vendored MCP crates
and their Cargo patches are removed. The boundaries are:

1. `rmcp::ServerHandler` owns protocol dispatch and standard 2026 discovery.
   The handwritten stateless dispatcher and discovery envelope are removed.
2. Authentication stays in Plasm HTTP middleware. Pass a verified typed
   identity into each request, retaining API-key/OAuth policy, tenant binding,
   revocation checks, OAuth endpoints and DNS-rebinding protection.
3. A request context carries transport correlation and observed client policy.
   Logical execution session references remain independent of MCP sessions.
4. Redis implements `rmcp`'s session-store trait. Preserve cross-instance
   initialization restoration and deletion semantics. Authentication is checked
   again on the receiving instance; it is not restored from serialized claims.
5. Preserve resources, UI capability negotiation, full error evidence, progress
   notifications and disconnect accounting. Do not route through the old SDK
   as a compatibility adapter.
6. Tests retain behavioural assertions through native models and independent
   HTTP peers. Python execution and isolated pgvector discovery pass, including
   official-client probes against both 2025 stateful and 2026 stateless routers.

Redis uses a new record namespace. Existing transport handshakes must reconnect
after deployment; old SDK records are not interpreted as rmcp state. Logical
execution references remain independent of transport handshakes.

No release or commit is implied by this working migration.

## Final local verification — 2026-10-18

- `cargo test -p plasm-agent-core --lib mcp_ --offline -- --include-ignored --test-threads=1`:
  **237 passed**, with the vendored Monty binary and an isolated Redis supplied.
  Includes 2026 discovery/metadata rejection, OAuth, resources, execution,
  cross-instance handshake restoration, reauthentication, durable deletion,
  and corrupt-record-versus-absence checks.
- `cargo test -p plasm-runtime --test mcp_http_protocol --offline`:
  **8 passed** against independent TCP peers.
- `cargo test -p plasm-agent-core --test mcp_python_protocol --offline -- --include-ignored --test-threads=1`:
  **2 passed** with isolated pgvector, blocked provider networking, Monty, and
  the standalone official client probe. The runtime SDK client check is now
  unconditional in the ordinary integration test.
- `cargo check -p plasm-mcp-app -p plasm-saas -p plasm-server --locked --offline`:
  passed. Both workspace lockfiles pass locked/offline metadata resolution;
  neither resolves the removed SDK/schema/transport crates. rmcp has no stdio
  or child-process transport feature enabled.
- Agent-core formatting and both repository whitespace checks pass.

Existing vendored Monty deprecation and macOS linker unwind-table warnings
remain. This is local protocol/integration verification, not a deployment,
benchmark, live-model, or exhaustive upstream-server compatibility result.
Test containers are disposable and separate from application databases.
