# MCP-Universe catalogues

Task-independent semantic catalogues for MCP-Universe's server contracts. These
are distinct from the direct vendor HTTP catalogues in sibling directories.

**Status:** authoring briefs only. No child has `domain.yaml` or `mappings.yaml`
yet. The Streamable HTTP runtime client is an initial transport component;
CML MCP compilation and engine dispatch are still required. This is not a
working benchmark integration or a scored submission.

The family root intentionally has no `domain.yaml`, so ordinary top-level API
packing skips it. Once authored, pack explicitly with
`plasm-pack-catalogs --apis-root apis/mcp-universe --output-dir target/mcp-universe-catalogs`.
Do not add these catalogues to appliance/SaaS default package lists.

## Initial authoring order

| Order | Catalogue | What it stresses | First acceptance boundary |
|---|---|---|---|
| 1 | [GitHub](github/README.md) | Scoped identity, parent/child composition, pagination, mutations and read-after-write | Repository → issues → comments; equal issue numbers in different repositories never alias |
| 2 | [Google Maps](google-maps/README.md) | Input-derived identity, positional correspondence, nested results and per-element failures | Search → details → origin/destination matrix without losing either input position |
| 3 | [Yahoo Finance](yahoo-finance/README.md) | Time-indexed observations, heterogeneous records, explicit JSON-text decoding and absence/error distinctions | Instrument → expiry → option chain, preserving instrument and expiry provenance |

These three exercise different mechanical boundaries, rather than maximizing
endpoint count. Playwright is a later candidate for stateful observations and
opaque actions; Notion should wait for its resolved tool/API-version contract.
Neither is a prerequisite for proving the initial mapping architecture.

## Transport boundary

Source baseline: MCP-Universe commit
`0df6c42717f2a618d34f5b271efd05be462b9e4d`.
Its [gateway](https://github.com/SalesforceAIResearch/MCP-Universe/blob/0df6c42717f2a618d34f5b271efd05be462b9e4d/mcpuniverse/mcp/gateway.py)
exposes `GET /{server}/sse` and `POST /{server}/messages/`: older HTTP/SSE,
**not Streamable HTTP**. Its
[manager](https://github.com/SalesforceAIResearch/MCP-Universe/blob/0df6c42717f2a618d34f5b271efd05be462b9e4d/mcpuniverse/mcp/manager.py)
also accepts explicitly configured HTTP server URLs; that does not provide a
Streamable HTTP endpoint for every registered server.

Plasm remains network-only. Server process management, any upstream stdio and
credentials belong to benchmark infrastructure. Before live integration, either
provide a benchmark-owned Streamable HTTP endpoint or explicitly approve an
older HTTP/SSE client. No auto-detection, fallback, process launcher or direct
vendor-API bypass is part of this family. No deployed endpoint has been tested.

See [the integration ladder](INTEGRATION-TESTING.md). Catalogue authoring follows
the canonical [authoring guidance](../../skills/plasm-authoring/SKILL.md).
