# MCP client conformance (informal)

Living notes on **Cursor**, **Claude Code**, and Plasm’s alignment with MCP transport evolution ([SEP-2567](https://modelcontextprotocol.io/seps/2567-sessionless-mcp), [SEP-2575](https://modelcontextprotocol.io/seps/2575-stateless-mcp)). This is **not** a formal certification matrix.

## Protocol baseline (2025-11-25)

Both Cursor and Claude Code use **Streamable HTTP** with protocol version **`2025-11-25`**: `initialize` → `MCP-Session-Id` on follow-ups, optional **GET SSE** listener, OAuth Bearer on hosted Plasm.

Plasm advertises the same protocol version in MCP `initialize` responses.

## Client behavior (observed)

| Capability | Cursor (desktop) | Claude Code | Notes |
|------------|------------------|-------------|-------|
| Transport | Streamable HTTP | Streamable HTTP | Migrated off deprecated HTTP+SSE split endpoint |
| `MCP-Session-Id` | Yes | Yes | Required for stateful 2025-11-25 servers |
| GET SSE | Yes | Yes | Used for server→client notifications |
| OAuth (MCP transport) | Yes (`platform.plasm.tools/plasm/mcp`) | Yes | Bearer refresh must not affect routing |
| MCP Apps | Yes (`ui://plasm/*`, AppBridge) | Limited | Cursor: App fetch fails when transport session dies |
| Session reinit on 404/-32016 | Partial | Buggy ([#50450](https://github.com/anthropics/claude-code/issues/50450), [#60949](https://github.com/anthropics/claude-code/issues/60949)) | Shared Redis transport helps all clients |
| Explicit state handles | Per-transport slot `s0`, … (legacy) | `l_<token>` wire ref | **Stateless `l_<token>`** from `plasm_context` |

## SEP-2567 alignment

| Concern | Legacy (2025-11-25) | Stateless (SEP-2567) | Plasm status |
|---------|----------------------|----------------------|--------------|
| Transport session | Required `MCP-Session-Id` for tool continuity | Optional correlation only | **`MCP-Session-Id` optional** for execute continuity |
| Explicit state handles | Per-transport slot `s0`, … | Application-provided handle | **`l_<token>`** wire ref from `plasm_context` |
| Cross-connection resume | Sticky transport or re-`plasm_context` | Same handle across connections | **Supported** via `l_<token>` + Redis bindings |

Plasm execute semantics use explicit stateless handles (`plasm_context` → `logical_session_ref` → `plasm` / `plasm_run`). Transport sessions remain optional for **2025-11-25** client correlation until hosts ship stateless mode.

## Multi-replica Plasm (Redis transport)

**Problem:** With 2+ `plasm-mcp` replicas and nginx `upstream-hash-by: $http_mcp_session_id`, `initialize` (no session header) and follow-ups (session header) often land on **different pods** → HTTP 500 / JSON-RPC **-32016 Session not found** (~40–50% failure).

**Fix:** `PLASM_MCP_TRANSPORT_REDIS_URL` + `RedisSessionStore` hydrates SDK transport sessions on any pod; ingress **`streamableHttpStickiness: none`**.

Smoke gate: `scripts/smoke/mcp-multireplica-transport-live.sh` (50× init → GET SSE → `tools/list` → `resources/read`, no cookie jar). Execute continuity: `scripts/smoke/mcp-multireplica-execute-live.sh` (HTTP cross-pod rehydrate + stateless `l_<token>` plan). Rollout survival: `scripts/smoke/mcp-rollout-survival-live.sh` (delete one pod; same `MCP-Session-Id` + `l_<token>` must keep working via Redis).

## Rollouts and Cursor “disconnected”

**Ingress stickiness is not the fix for rollouts.** With Redis transport, **`streamableHttpStickiness: none`** is correct — re-enabling `upstream-hash-by` on 2+ replicas reintroduces the initialize vs follow-up split (~50% `-32016`).

During **`RollingUpdate`**, the pod holding a client’s **GET SSE** TCP connection is terminated. That connection **cannot** migrate to another pod. Redis preserves **session metadata** (`MCP-Session-Id` → hydrated runtime on any pod); it does **not** preserve the live SSE socket.

| Symptom | Cause | Server-side mitigation | Client workaround |
|---------|-------|------------------------|-------------------|
| Cursor MCP shows **disconnected** | GET SSE reset on pod SIGTERM | `preStop` sleep + longer graceful shutdown (v0.3.4+); Redis session survives | Toggle MCP reconnect in Cursor settings |
| MCP Apps **Not connected** while tools work | AppBridge tied to GET SSE listener | Same as above | Reopen MCP / refresh Apps panel |
| `-32016` after rollout | Session deleted from Redis (should not happen on streamable HTTP EOF) | `RedisSessionStore::delete` = local-only; explicit HTTP DELETE clears Redis | Re-`initialize` |

**Execute continuity** across transport reconnect: reuse **`logical_session_ref`** (`l_<token>`) from `plasm_context` — bindings live in Redis separately from the transport SSE socket.

**Do not** scale to 1 replica or re-enable header hash stickiness as a rollout workaround in production.

## SEP-2567 / SEP-2575 readiness

| SEP | Status | Change | Plasm |
|-----|--------|--------|-------|
| [SEP-2575](https://modelcontextprotocol.io/seps/2575-stateless-mcp) | Final | Removes `initialize`, GET SSE; `subscriptions/listen` for notifications | Track for ~July 2026 protocol bump |
| [SEP-2567](https://modelcontextprotocol.io/seps/2567-sessionless-mcp) | Final | Removes `Mcp-Session-Id`; explicit handles in tool args/results | **`logical_session_ref`** (`l_<token>`) ≡ SEP-2567 handle |

Plasm execute semantics already use explicit stateless handles (`plasm_context` → `logical_session_ref` → `plasm` / `plasm_run`). Transport sessions remain optional for **2025-11-25** clients until hosts ship stateless mode.

**Future:** `PLASM_MCP_STATELESS=1` — per-request auth + `_meta`, no transport session, no GET SSE (when SDK/clients support the next protocol version).

## Program surface gotchas (pokeapi / language matrix)

Teaching-table symbols (`e#`, `m#`, `p#`, `r#`) describe **postfix** program shape — not arbitrary function-call syntax on entity names.

| Mistake | Why it fails | Correct shape |
|---------|--------------|---------------|
| `Type(p10) \| limit(3)` | Applies `limit` before projection; may compile to get-by-name / wrong dispatch | `Type \| limit(3)(p10, p9)` — postfix projection **after** the transform chain |
| `fire = Type \| filter(...)` then `fire(p10, p9)` | Root `fire(...)` is parsed as entity lookup, not binding projection | `fire[p10, p9]` or `fire(p10, p9)` after compiler desugar (binding field roots) |
| Bare relation nav with `p#` after `.` | Homograph filter vs relation | Copy relation from exemplar: `issues.r#` or wire name when binding name matches |

These are **authoring / teaching** issues, not MCP transport bugs. Run Explorer and TSV output depend on successful live execution after `wait()` — graph-backed spill rehydration must populate entity rows before publish (see `graph_spill_e2e`).

## Related docs

- `deploy/docs/mcp-ingress-edge.md` — ingress stickiness vs Redis
- [docs/mcp-logical-sessions.md](mcp-logical-sessions.md) — logical vs transport session identity
- [docs/mcp-session-reuse.md](mcp-session-reuse.md) — steady-state MCP tool discipline
