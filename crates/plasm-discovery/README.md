# plasm-discovery

Stepwise typed discovery over CGS catalogs: intent decomposition, phrase / lexical indexes, graph-aware qualifier checks, and clarification gates (`AgentDiscovery`).

OSS release binaries are **lexical-only**. Intent-only MCP `plasm_context` seed selection uses the separate LLM path (`semantic-auto-seed` / `PLASM_DISCOVERY_SEMANTIC_AUTO_SEED`).

See repository docs for HTTP `/v1/discover-typed` and MCP `discover_capabilities` with `typed: true`.
