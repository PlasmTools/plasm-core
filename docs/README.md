# Internal engineering notes (`docs/`)

These pages are **maintainer-facing** design records. They are **not** synced into
the public MkDocs site under `doc-site/`. User-facing MCP and appliance behavior
lives in [`doc-site/docs/`](../doc-site/docs/index.md).

| Document | Topic |
| --- | --- |
| [mcp-sdk-cutover.md](mcp-sdk-cutover.md) | `rmcp` inbound/outbound cutover; vendored SDK removal |
| [mcp-dependency-selection.md](mcp-dependency-selection.md) | Why `rmcp` and related pins |
| [mcp-python-integration.md](mcp-python-integration.md) | Python-side MCP tooling boundaries |
| [mcp-sse-parser-repair.md](mcp-sse-parser-repair.md) | SSE framing/parser fixes |
| [outbound-mcp-transport.md](outbound-mcp-transport.md) | Catalog outbound Streamable HTTP client |
| [agent-error-contract.md](agent-error-contract.md) | Agent-visible failure envelopes |
| [catalog-membership-contracts.md](catalog-membership-contracts.md) | Catalog session membership laws |
| [compute-materialization-contracts.md](compute-materialization-contracts.md) | Materialization invariants |
| [runtime-row-contract.md](runtime-row-contract.md) | Runtime row identity and completeness |

**Build and CI:** [`build/bazel/README.md`](../build/bazel/README.md) is the
canonical Bazel graph, nextest, cache, and release-export reference.

**Language specification:** normative contracts under
[`doc-site/docs/reference/`](../doc-site/docs/reference/plasm-language-definition.md)
with matrix evidence in
[`crates/plasm-e2e/tests/plasm_language_matrix/PYTHON-COVERAGE.md`](../crates/plasm-e2e/tests/plasm_language_matrix/PYTHON-COVERAGE.md).
