# Python, discovery and MCP integration

The integration target is `crates/plasm-agent-core/tests/mcp_python_protocol.rs`.
It uses real loopback HTTP on both sides of Plasm, an independent JSON-RPC peer,
and the vendored Monty worker to execute admitted Python Programs. It does not
use the Python MCP SDK or a live MCP-Universe server.

## Reproduce

Supply an absolute `PLASM_MONTY_BINARY` path to a worker built from the vendored
source. `scripts/ci/build-monty-runtime.sh ABSOLUTE_INSTALL_ROOT` builds it when
needed. The ordinary test uses explicit internal catalogue exposure:

```sh
PLASM_MONTY_BINARY=/absolute/path/to/monty \
  cargo test -p plasm-agent-core --test mcp_python_protocol --offline
```

The ignored discovery test additionally requires an isolated PostgreSQL database
with pgvector. It writes synthetic catalogue and model-boundary cache records;
never point it at a shared or production discovery database.

```sh
PLASM_MONTY_BINARY=/absolute/path/to/monty \
PLASM_DISCOVERY_DATABASE_URL=postgresql://USER:PASSWORD@127.0.0.1:PORT/DATABASE \
OPENROUTER_API_KEY=offline-test-no-provider-calls \
HTTPS_PROXY=http://127.0.0.1:1 HTTP_PROXY=http://127.0.0.1:1 \
NO_PROXY=localhost,127.0.0.1 \
PLASM_MCP_TEST_EVIDENCE=/absolute/path/to/evidence.json \
  cargo test -p plasm-agent-core --test mcp_python_protocol --offline \
  -- --include-ignored --test-threads=1
```

Provider traffic is blocked by the closed proxy. The test uses synthetic unit
vectors and cached selection/support decisions, while exercising actual packed
catalogue loading, pgvector retrieval, routing, intent-only `plasm_context`,
Python review, `plasm_run`, `resources/read`, and logical-session extension.
This verifies deterministic integration, not semantic model quality.

Assertions cover rejection without backend dispatch, review without dispatch,
exactly one dispatched query, integer column type, one returned row with ID 7,
unknown source coverage, artifact retrieval and stable logical-session identity.

Local validation on 2026-10-18 after the rmcp cutover: both tests passed with
`--include-ignored` (2 passed, 0 failed, 0 skipped). Existing Monty deprecation
and linker unwind-table warnings remain. No model calls or commits were made.

## SDK interoperability

The former SDK-to-host `tools/list` timeout is resolved by the authorized full
cutover to official rmcp 3.5.1. The runtime client now completes the stateful
exchange. The independent official-client probe also passes against both 2025
initialization and 2026 discovery routers. The raw HTTP test peer skips empty
SSE priming events and selects the response by JSON-RPC request ID.

The runtime client probe is an unconditional regression gate. The optional
`PLASM_MCP_CLIENT_PROBE_BINARY` points to the standalone official-client probe.
Both probes were enabled for the passing local integration run.

## Remaining gates

Parent/child composition with colliding identities, successful writes followed
by rereads, and pinned upstream server conformance remain separate ladder gates.
These tests do not establish catalogue readiness or benchmark performance.
