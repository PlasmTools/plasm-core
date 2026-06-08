# MCP session reuse and host identity

This document is the **canonical** description of how `plasm_context`, `plasm`, and execute session reuse interact. The MCP **transport** (`MCP-Session-Id`) does not identify a host agent, window, or subagent. That boundary is always defined by the **host** via the **`intent`** string on `plasm_context` and the resulting **logical session**.

See also: [MCP logical sessions](mcp-logical-sessions.md), [incremental teaching table prompts](incremental-teaching-prompts.md), [MCP trace correlation](mcp-trace-correlation.md).

## 1. Flow

0. **`discover_capabilities`** (optional): **one call per user goal** with a single **`intent`** string describing the whole task — not one discover per API or keyword list. Merge table rows into one **`plasm_context`** **`seeds`** array; retry discover only if required entities are missing.

1. **`plasm_context`** with **`intent`** and non-empty **`seeds`**
  - The server is **idempotent** on `(tenant_scope, intent)`: the same string yields the same canonical **`logical_session_id`** and the same per-transport **`logical_session_ref`** (e.g. `s0`).
  - If a live execute session is already bound for that logical id, the server **expands** or **federates** into that session (no fresh-open path).
  - **First open** (new execute row): `` `{sN}` `` + optional stale-binding notice + fenced **full** teaching TSV only — no entity-count or char accounting in the body.
  - **Expand** (new entities on an existing binding): `` `{sN}` `` + fenced **delta** TSV only.
  - **Duplicate seeds (already exposed):** if the request does not add new entity picks, the server returns **one line** only:
    ```text
    _Session unchanged (`e1`…`eN`). Use `plasm` / `plasm_run` with this logical_session_ref._
    ```
    The full teaching table / TSV teaching table and syntax blurb are **not** replayed (token-saving). Federated re-apply of an identical multi-catalog seed set collapses to **one** such line, not one block per catalog wave. Steady state remains **`plasm`** / **`plasm_run`** with the existing **`logical_session_ref`**.
  - **Reused execute open** (`reused: true` on session create): same **one-line** notice—**no** full TSV replay and **no** `PLASM_NOOP_EXPRESSION_HINTS` stack.
  - **Registry ids:** copy canonical `api` from `discover_capabilities`; hosts may pass **`registry_aliases`** (e.g. `pokemon` → `pokeapi`) in seeds—the server resolves to the packed `entry_id` before tenant checks.
  - If there is no binding, or the stored binding points at an **expired or missing** execute row, the server may **open** a new `(prompt_hash, session)`.
  - The **primary** `entry_id` for the first open is chosen in **lexicographic order** among distinct catalog ids in the seed set, so two calls with the same set of catalog seeds in different order still agree on the same primary for [SessionReuseKey](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/execute_session.rs) matching.
  - **`_meta.plasm`** (model-visible): `logical_session_ref`, `continuity`, `domain_revision`, optional `relations`. Execute binding ids and catalog lists are server/trace metadata only — agents use **`logical_session_ref`** with **`plasm`** / **`plasm_run`**.
2. **`plasm`** with **`logical_session_ref`** and **`program`**
  - Steady state for most user turns: use **`plasm`** only; do not repeat `plasm_context` unless you need a new **`intent`** or new seeds.

## 2. `intent` (host contract)

- **Role:** Opaque string selected by the **host** to mean “this agent / window / subagent / task isolation boundary / whatever the host needs to separate.”
- **Stability:** Must stay **stable for the duration** of that context. Rotating **`intent`** per message or per unrelated micro-step breaks idempotent logical session recovery and will fragment reuse.
- **Not inferable:** The Plasm server does **not** infer a stable “ongoing user task” from the transport, workspace path, or prompt text. The host must supply **`intent`** when it wants continuity.

## 3. `SessionReuseKey` (when execute open can reuse a row)

Defined in [execute_session.rs](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/execute_session.rs) as the key for in-memory `try_reuse_session` on **new open**:


| Field                  | Role                                                                                                                 |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------- |
| `tenant_scope`         | Incoming-auth tenant / anonymous scope.                                                                              |
| `entry_id`             | **Primary** catalog id for the open (lexicographically first among seed catalogs).                                   |
| `catalog_cgs_hash`     | Pinned CGS digest.                                                                                                   |
| `entities`             | Sorted, deduplicated primary-catalog entity seeds.                                                                   |
| `principal`            | Present when `PLASM_AUTH_RESOLUTION=delegated`.                                                                      |
| `plugin_generation_id` | Compile-plugin pin when a compile plugin is loaded.                                                                  |
| `logical_session_id`   | **MCP only:** logical UUID string; scopes reuse per logical session. `None` for HTTP-only open without a logical id. |


If any of these differ from a prior open, a **new** execute row may be created. Changing catalog hash or plugin generation is intentional: the old session is not a safe match.

## 4. Stale execute binding (continuity break)

MCP can retain a **logical** handle (`logical_session_ref`) and a **binding** `(prompt_hash, session_id)` in memory or in the host-wide map after the in-memory `ExecuteSessionStore` row was **dropped** (e.g. idle TTL). The next `plasm_context` will treat the binding as missing and may **open** a new execute session.

When that happens, the `plasm_context` result includes:

- Markdown: a prominent notice that the prior in-memory session was missing or expired, that **all earlier `e#` / `m#` / `p#` from this chat are void**, and that the model must re-read the new teaching table/TSV from this response only.
- **`_meta.plasm.continuity`**: on **every** `plasm_context` response, `stale_binding_recovered` (boolean) and `new_symbol_space` (boolean — `true` when a new execute row with a new symbol table was bound, e.g. first open or a non-reused open after an expiry). When `stale_binding_recovered` is true, `previous_execute` may name the old `(prompt_hash, session_id)`. When `new_symbol_space` is true, `discard_cached_plasm_symbols` is also `true` (same meaning as `new_symbol_space` for tool clients that key off a dedicated key).

## 5. Execute-time projection vs MCP/HTTP result summaries

- **Projection (path expressions, batch steps):** selects which **fields and rows** the executor materializes. This is the authoritative narrowing for *what the engine computes*.
- **Table / TSV / Markdown on the wire:** may still **cap**, **summarize lossy** fields, or point at **`resources/read`** for full JSON snapshots, even when the execution already used projection. That is **transport shaping**, not a second projection system. Live MCP/HTTP run Markdown uses **return-label headers** (`## sorted (12 rows)` / `# Results` + `### …`) plus the table only—no REPL `→`, `output:`, or `owner:` lines.

Large-result guidance: prefer projection in expressions first; when the surface must still be shortened for the tool channel, use snapshot links and `_meta.plasm` as today.

## 6. Related code

- `plasm_context` / `plasm` / `plasm_run` (single DAG pipeline): [mcp_server.rs](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/mcp_server.rs), [execute_pipeline/mod.rs](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/execute_pipeline/mod.rs)
- `publish_plasm_result_steps`, `apply_capability_seeds`, `execute_session_create_response_inner`: [http_execute.rs](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/http_execute.rs)
- `SessionReuseKey`, `ExecuteSessionStore::try_reuse_session`: [execute_session.rs](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/execute_session.rs)
- MCP Markdown previews and threshold: [mcp_run_markdown.rs](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/mcp_run_markdown.rs)