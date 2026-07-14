# MCP session reuse and host identity

> **Canonical docs live in the monorepo:** [intent discovery](https://github.com/PlasmTools/plasm/blob/main/docs/intent-discovery.md) (primary seed route) and [mcp-session-reuse.md](https://github.com/PlasmTools/plasm/blob/main/docs/mcp-session-reuse.md) (session / `logical_session_ref` contract). This page is a short pointer for the plasm-core doc site — do not treat older discover-first wording as current.

## Flow (summary)

1. **`plasm_context`** `session_mode: "new"` + **`intent` only** when semantic auto-seed is enabled → host routes seeds ([intent discovery](https://github.com/PlasmTools/plasm/blob/main/docs/intent-discovery.md)); **`ready`** mints `logical_session_ref` + teaching TSV.
2. **`plasm`** / **`plasm_run`** with the same `logical_session_ref`.
3. **`discover_capabilities`** is **secondary** — browse / recover after `hard_miss` or pick explicit `seeds`.

Session identity is **`logical_session_ref`** only, selected via **`session_mode`** — not by the `intent` string. See the monorepo session-reuse doc for `SessionReuseKey`, extend, federation, and stale-binding rules.
