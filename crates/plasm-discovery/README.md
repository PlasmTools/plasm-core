# plasm-discovery

Stepwise **typed** catalog browse over CGS: intent decomposition, phrase / lexical indexes, graph-aware qualifier checks, and clarification gates (`AgentDiscovery`).

## Role in the product

| Path | Status | Where |
|------|--------|-------|
| Intent discovery (semantic auto-seed) | **Primary** for MCP seed selection | `plasm_context` intent-only `new` — monorepo `docs/intent-discovery.md` |
| Lexicon browse (`plasm-core::discovery`) | **Secondary** | MCP `discover_capabilities` (auto-seed **off** only); HTTP `POST /v1/discover`; terminal search; auto-seed breakout preview |
| This crate (`POST /v1/discover-typed`) | **Legacy / optional** — not on MCP | Typed JSON browse via `AgentDiscovery`. Prefer intent discovery or lexicon HTTP. Do not restore MCP `typed: true`. |

Keep this crate for typed HTTP and internal eval coupling until a dedicated delete PR; it is **not** the MCP seed path.

OSS release binaries ship lexical typed browse capability via this crate for `/v1/discover-typed` only. Intent-only MCP seed selection is the separate `semantic-auto-seed` path (`PLASM_DISCOVERY_SEMANTIC_AUTO_SEED` + OpenRouter).
