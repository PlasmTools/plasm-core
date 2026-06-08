# Tool model HTTP API

`plasm` exposes an operator-facing JSON projection aligned with teaching table prompt rendering and the dynamic CLI (`cli_builder`), not raw CGS parsing in clients.

Hosted **Tool Explorer** (`/tools/:entry_id`), project MCP configuration, and outbound OAuth flows consume this endpoint via `PlasmWeb.PlasmMcpDataPlane.fetch_tool_model/3` on [Plasm Cloud](https://platform.plasm.tools).

## `GET /v1/registry/{entry_id}/tool-model`

- **Query**
  - `focus` — `all` (default), `single`, or `seeds`.
  - `entity` — repeat for `single` (exactly one name) or `seeds` (one or more). Omit for `all` (and do not send `entity=` with `focus=all`).
- **Response (summary)**
  - `entry` — `entry_id`, `label`, `tags` (same as registry list rows).
  - `focus` — `mode` and `resolved_entities` (entity names included in this slice).
  - `overview` — `entity_count`, `relation_edge_count`, `verb_count`.
  - `execute` — static LLM execute continuation notes (pagination, async plan runs, review gate). Same semantics as the teaching TSV preamble and MCP `program_contract`; not derived from per-entity CGS.
  - `auth` — scheme, OAuth metadata, `connect_profile` (Phoenix outbound OAuth eligibility).
  - `entities` — per-entity CLI-shaped `verbs`, declared `relations`, derived `reverse_traversals`, `entity_ref_links`, and `domain_lines` (parallel to teaching table).
  - `domain.model` — full `DomainPromptModel` (structured teaching table metadata: kinds, cross-entity hints, relation materialization summaries).

### `execute` block (continuations)

Every tool-model response includes an `execute` object describing **host-only** program expressions agents use after the first execute response:

| Field | Meaning |
|-------|---------|
| `summary` | Continuations are host-minted handles — not vendor API cursors or job ids. |
| `pagination` | MCP: `page(s0_pgN)` from tool results. HTTP-only execute: plain `page(pgN)` when no MCP logical session. |
| `long_operations` | Async plan runs: `wait=false` or review auto-async → compact op lines (`+` accept · `~` running · `=` unchanged · `!` done). Poll `wait(sN_oM)` every few seconds; optional `GET …/operations/{handle}/stream` SSE or MCP `notifications/plasm/op`. HTTP without `plasm_context` uses synthetic slot **`s0`** (`s0_oN`). |
| `review_gate` | Dry verdict **review** requires `plan_commit_ref` (`pcN`) from matching plan dry-run or `force=true`. Commit ids hash semantic plan DAG only. |

Full workflow: [plasm-long-operations.md](plasm-long-operations.md). Surface grammar: [plasm-language-definition.md](plasm-language-definition.md#host-continuations-page-wait-cancel). Teaching TSV preamble (first wave): [incremental-teaching-prompts.md](incremental-teaching-prompts.md).

Paginated query verbs in `entities[].verbs` append the pagination note to `about` when the capability declares HTTP pagination — same copy as `execute.pagination`.

## HTTP execute (related, not this route)

Tool-model describes **what** agents can express; **`POST /execute/:prompt_hash/:session`** runs programs.

| Query / body | Role |
|--------------|------|
| `mode=plan` | Plan dry-run; mints `plan_commit_ref` in `_meta.plasm`. |
| `wait=false` | Background live execute; accept response includes `wait(s0_oN)` (HTTP `s0` slot). |
| `force=true` | Bypass review soft gate. |
| `plan_commit_ref=pcN` | Accept matching dry-run plan after **review** verdict. |

Program bodies may also be top-level `wait(…)` / `cancel(…)` continuations — dispatched before plan compile.

## Errors

Errors use `application/problem+json`; unknown `entry_id` matches discovery `404` semantics; invalid focus/entity combinations return `400` with type `https://plasm.invalid/problems/plasm-tool-model-bad-request`.

## Tests

- Tool-model build smoke: `cargo test -p plasm-agent-core tool_model`
- Long-op dual surface: `cargo test -p plasm-e2e --test long_operation_e2e`
