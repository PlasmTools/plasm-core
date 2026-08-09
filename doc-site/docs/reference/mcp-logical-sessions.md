# MCP logical sessions vs transport (`MCP-Session-Id`)

## Historical behavior (superseded for MCP tools)

Previously, Plasm bound **one** execute session `(prompt_hash, session_id)` per **MCP transport session** (Streamable HTTP `MCP-Session-Id` / SDK session id). Tool state (`plasm_context` → `plasm`), short artifact refs `plasm://r/{n}`, and trace roots were all keyed by that transport handle.

That model breaks when:

- Multiple agent instances or windows share one MCP connection.
- The same agent needs several independent symbol spaces / execute sessions over one transport.
- Horizontal scaling requires resuming work without sticky transport affinity.

## Current model

1. **Transport session** — Still the Streamable HTTP / SDK session: used for auth, connection lifecycle, and **correlation only** (which physical MCP connection issued a call).

2. **Logical session** — The canonical Plasm session for prompts, execute `(prompt_hash, session_id)`, monotonic `e#`/`m#`/`p#`, run artifacts, and **trace root identity**:
   - **`intent`** — **Host-chosen** stable string for the agent context (window, subagent, or other boundary the host defines); MCP JSON field on `plasm_context`; used for idempotent opens. The server does not infer this from the MCP transport. See [MCP session reuse](mcp-session-reuse.md).
   - **Canonical id** — Server-minted UUID (`logical_session_id` internally): global stable identifier for archives, traces, and cross-replica correlation (when backed by shared storage).
   - **`logical_session_ref`** — Stateless wire handle `l_<token>` (22 URL-safe base64 chars encoding the canonical UUID bytes) returned by `plasm_context` — e.g. `l_AAAAAAAAQACAAAAAAAAAAQ` for UUID `00000000-0000-4000-8000-000000000001`. Agents pass this ref to `plasm` / `plasm_run` and see it in short run URIs. Legacy transport slots (`s0`, …) are rejected.
   - **Paginated list continuations** — Opaque handles are **`l_<token>_pg1`**, **`l_<token>_pg2`**, … (wire ref + sequence): use `page(l_<token>_pgN)` from tool results. HTTP-only execute (no MCP `plasm_context`) uses plain **`pgN`** handles instead.

3. **Flow** — **`plasm_context` first** with `intent` and non-empty `seeds`, then mostly `plasm` with `logical_session_ref`. Short run URIs use `plasm://session/{logical_session_ref}/r/{n}`; canonical `plasm://execute/.../run/{uuid}` remains accepted on `resources/read`. Legacy `plasm://r/{n}` remains for HTTP-only execute paths without a logical id. Those **`plasm://…` URIs are MCP `resources/read` identifiers**, not strings you pass back into the `plasm` tool as Plasm path expressions.

## In-process vs scaled deployments

**MCP transport layer** (SDK `MCP-Session-Id`, GET SSE, JSON-RPC correlation): when `PLASM_MCP_TRANSPORT_REDIS_URL` is set, per-transport stats/index caches are mirrored in Redis so **2+ `plasm-mcp` replicas** work without ingress stickiness. Logical session continuity uses stateless `l_<token>` handles, not transport slot maps.

**Execute / logical session registry** (`logical_execute_bindings`, execute session descriptors): when `PLASM_MCP_TRANSPORT_REDIS_URL` is set, the host mirrors **logical UUID → `(prompt_hash, session_id)`** bindings and **session descriptors** (prompt text, entities, federated entry ids, reuse key, `expires_at_unix`, async operation metadata, …) in the same Redis cluster. Any pod resolves bindings and **rehydrates** the in-memory execute row on demand via [`PlasmHostState::get_execute_session`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/server_state.rs) (HTTP and MCP both use this path).

**Operational limits after rehydrate:** hot graph cache and paging resume tokens are restored from persisted session metadata. Session metadata, teaching exposure, binding maps, federated catalog hashes, and plan-commit records are persisted when Redis is configured. MCP `plasm_run` awaits server-side and returns terminal rows/snapshots; explicit `wait(oN)` / `cancel(oN)` operation continuations are HTTP / remote CLI only. Spilled graph pages reload lazily when `PLASM_GRAPH_CACHE_URL` is set. Rehydrate failures emit `plasm.execute.rehydrate.outcomes_total` metrics.

**Smoke:** product-monorepo `scripts/smoke/mcp-multireplica-execute-live.sh` (HTTP create → cross-pod GET; MCP `plasm_context` → fresh-transport `plasm` plan; token-only `plasm_run` on the reviewed `pcN`).

## Related code

- MCP handler: [`plasm-oss/crates/plasm-agent-core/src/mcp_server.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/mcp_server.rs)
- Logical session registry: [`plasm-oss/crates/plasm-agent-core/src/session_identity.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/session_identity.rs)
- Trace correlation: [`docs/mcp-trace-correlation.md`](mcp-trace-correlation.md)
- Incremental teaching: [`docs/incremental-teaching-prompts.md`](incremental-teaching-prompts.md)
- Session reuse and `SessionReuseKey`: [`docs/mcp-session-reuse.md`](mcp-session-reuse.md)
