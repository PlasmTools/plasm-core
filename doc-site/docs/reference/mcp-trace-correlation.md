# MCP / HTTP trace correlation (`trace_id`)

Plasm exposes in-memory MCP session traces and optional durable sink rows keyed by a stable `**trace_id**` (UUID). The id is **not** a random v4 per tool call: it is an RFC 4122 **name-based UUID (v5)** so the same logical session always maps to the same root in multi-tenant analytics without relying solely on server-side filtering.

## Current format (v2)


| Transport                   | Preimage (UTF-8 string hashed with v5)                                            | Namespace constant (Rust)                                             |
| --------------------------- | --------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| MCP (logical session)       | `"{tenant_id}\nlogical:{logical_session_id}"` — canonical agent/window identity   | `TRACE_ID_NS_MCP_LOGICAL_V1` in `plasm-oss/crates/plasm-agent-core/src/trace_hub.rs` |
| MCP (transport-only, tests) | `"{tenant_id}\n{mcp_session_id}"` — empty `tenant_id` uses `anonymous`            | `TRACE_ID_NS_MCP_TRANSPORT_V2`                                        |
| HTTP execute                | `"{tenant_id}\n{prompt_hash}\n{execute_session_id}"` — empty tenant → `anonymous` | `TRACE_ID_NS_HTTP_EXECUTE_V2`                                         |


Production MCP tools use `**trace_id_for_mcp_logical_session`** and `TraceHub::ensure_logical_session`. `MCP-Session-Id` is stored on summaries as **transport correlation** only, not as the trace root. `trace_id_for_mcp_transport_session` / `ensure_session` remain for legacy/tests.

## Migration from earlier roots

**v1 (superseded):** MCP used only `mcp_session_id` bytes; HTTP used `"{prompt_hash}\n{execute_session_id}"` with separate namespaces (`…09d0` / `…09d1`).

**v2:** Adds `**tenant_id`** (or `anonymous`) to the preimage and uses **new namespace UUIDs** (`…09d2` / `…09d3`). Any UUIDs computed under v1 **differ** from v2 for the same strings.

**Operational notes:**

- **Iceberg / trace sink:** Historical rows keep old `trace_id` values; new ingests use v2. `**GET /v1/traces/{id}`** detail is built only from `event_kind = mcp_trace_segment` rows (payload = canonical `[TraceEvent](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-trace/src/lib.rs)`); older audit kinds are not interpreted.
- **Cross-release dashboards:** key by `tenant_id` + `mcp_session_id` (and timestamps) if they must join v1/v2 `trace_id` roots, or re-backfill v2 ids if you adopt a one-off migration job.
- **In-memory `TraceHub`:** Process restart clears prior ids; only one format exists per deployment after upgrade.
- **SSE:** Completed traces expose snapshot `seq` equal to the last emitted sequence (including `terminal`); see `TraceHub::sse_snapshot_payload`.

## Tenant drift on one logical or transport trace key

If the same hub key (logical session id or legacy transport id) is reused with a **different** `TraceSessionMeta::tenant_id`, `ensure_logical_session` / `ensure_session` finalizes the prior active trace before opening the new root (different v5 `trace_id`).