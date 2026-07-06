# Incremental teaching-table prompts and reducing prompt churn

This document describes how Plasm serves the **Plasm teaching table** (many-shot, symbol-tuned **TSV** examples) for **HTTP execute** and **MCP execute** sessions, and why that design **reduces prompt churn** for agents and humans.

**Teaching medium:** agent-visible context is always the **TSV table** (`plasm_expr`, one tab, `Meaning`), optionally prefixed by `#` comment contract lines and wrapped in a markdown fence by HTTP/MCP hosts. The legacy compact markdown transcript (`;;`-style blocks) is not emitted on the wire.

## Goals

1. **Less redundant context** — Avoid sending the full teaching table on every tool turn when the session’s catalog entry and seeds have not changed.
2. **Incremental graph exposure** — Treat the CGS as a graph: ship teaching rows **in waves** as more entity types are needed, instead of always expanding to a large 2-hop neighbourhood in the first message.
3. **Stable symbolic indices** — Keep `e#` / `m#` / `p#` / `r#` assignments **monotonic**: once assigned in a session, a symbol does not change meaning when new entities or capabilities enter the slice. Relations use **`r#`** (not `p#`).
4. **Aligned parse + teaching table** — Programs must parse with the **same session [`SymbolMap`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/symbol_tuning/mod.rs)** the teaching TSV taught (`e1.m3(…)` resolves in-grammar; there is no pre-parse `expand_*` string pass).

“**Prompt churn**” here means: **repeated or oversized teaching text** in agent context (duplicate full prompts on session reopen, multi-megabyte tables when only a small neighbourhood is needed, or shifting `m#` indices between waves). Those waste tokens, confuse models, and break trust in symbolic examples.

## Problem (before this design)

- **Full dump** — Rendering the teaching table for the union of 2-hop neighbourhoods around seeds produced large prompts even when the task only needed a few entity types.
- **Repeat sends** — MCP `plasm_context` (open path) could return the entire teaching table again when the server **reused** an existing session (`reused: true`), unless the client omitted the body (we now omit the teaching block on reuse).
- **Index drift** — A naïve rebuild of `SymbolMap` from a growing entity set can **re-sort** method keys globally, which would reshuffle `m#` values between waves. Incremental sessions instead **append** new `(domain, kebab)` and identifier bindings.

## Design overview

### `FocusSpec::SeedsExact`

Teaching-table slicing can use an **exact** entity list (no automatic 2-hop union). That list is the first **wave** of exposure: only those entity blocks appear in the initial teaching string.

Implementation: [`FocusSpec::SeedsExact`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/symbol_tuning.rs) and [`entity_slices_for_render`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/symbol_tuning.rs).

### `TeachingExposureSession`

A session-scoped structure in **plasm-core** ([`TeachingExposureSession`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/symbol_tuning.rs)) allocates:

- **`e#`** — Order of **first exposure** of each **qualified** `(registry entry_id, entity)` pair. Colliding entity names across catalogs (e.g. `github:Issue` and `linear:Issue`) receive **distinct** `e#` symbols; teaching rows and surface filters always use the session registry `entry_id`, not bare `CGS::entry_id` from YAML fixtures.
- **`m#`** — New `(domain, kebab)` capability pairs, sorted **only among newly added** pairs, then assigned the next free `m` indices.
- **`p#`** — New fields and capability params visible in the cumulative slice (sorted among **new** names, then next free `p` indices).
- **`r#`** — Declared **relation** navigation slots (separate counter from `p#`).

Existing assignments are never rewritten. Rendering uses [`render_teaching_prompt_bundle_for_exposure`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/prompt_render.rs); later waves pass `emit_entity_blocks` so only **new** entity blocks are appended (and the main “Valid expressions” preamble is omitted on those waves).

### Teaching exemplar anchors (CGS binding surface)

Whether teaching rows should include an **entity anchor** exemplar (for example `Entity($)` / symbolic `e#` usage) must not be decided in the prompt layer by naming a transport (for example “GraphQL”). **plasm-core** exposes transport-neutral predicates on the capability’s mapping template:

- [`template_domain_exemplar_requires_entity_anchor`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs) — true when the template needs an anchor for teaching examples: HTTP **path** template variables **or** a GraphQL operation **`variables`** block that binds an `id` (or equivalent single-entity key).
- [`template_invoke_requires_explicit_anchor_id`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs) — used for expression pre-parse / shadow-invoke rules when an explicit anchor id is required (path vars **or** any GraphQL operation variable list), matching the compile path’s expectations.

[`CapabilitySchema::domain_exemplar_requires_entity_anchor`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs) and [`invoke_requires_explicit_anchor_id`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs) delegate to those helpers. Teaching synthesis consults the schema-level predicate (for example via `path_vars_empty` in [`prompt_render`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/prompt_render.rs)) so **prompt synthesis stays free of GraphQL-specific conditionals**.

When the cumulative slice includes structured string semantics, the preamble adds **`<<TAG`** heredoc rules in [`prompt_render`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/prompt_render.rs): copy-pastable fenced `text` blocks show tagged form only. The only multiline/raw string form in path expressions is bash-inspired `<<TAG` + newline + body + closing line (trimmed `TAG`), with the same close optionally glued before `)` / `,` / `}`.

**Grammar note:** The opener is **`<<`** (two characters) plus a tag, not `<<<`. Legacy `d<<<` is removed—use **`<<TAG`** only (never `<<` + newline alone).

### `$` and `~"text"` (teaching only)

Teaching TSV rows use **`$`** and **`~"text"`** as **fill-in cues**, not values to copy into executable programs. The parser accepts bare `$` as the string `"$"` ([`expr_parser`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/mod.rs)); submitting `e#~$` runs a real search for that character (e.g. Linear `issue_search` → `title contains "$"`), which often returns **zero rows**.

- **Never emit bare `$`** in `plasm` programs — substitute concrete ids, filter keys, or search strings from context (team list, prior bindings, user intent). `plasm_run` receives only **`run_ref`** (`pcN` or a page handle).
- **Full-text search** rows teach **`e#~"text"`** (quoted meta-literal). Replace `text` with real terms, e.g. `e2~"billing"`, not `e2~$`.
- **Search-only entities** (no `query` capability, e.g. Linear `Issue`): there is no `e#{}` “list all”. Use scoped filters shown in the teaching table (`e#{p#=…}`) and/or real `~"…"` search text. Resolve filter values from the workspace (e.g. list `Team` first — do not assume doc-example keys like `ENG`).

MCP initialize teaches these grammar rules once; `plasm_context` waves return teaching TSV rows/symbols only and do not repeat the grammar preamble.

### Execute session state (plasm)

[`ExecuteSession`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/execute_session.rs) holds:

- `prompt_text` — Cumulative teaching text (wave 1 + optional `## Expanded capabilities` sections).
- `teaching_exposure` — The [`TeachingExposureSession`] used for teaching rendering and session [`SymbolMap`] alignment (`parse_session_line`, wire-surface display via [`wire_surface_for_session_with_optional_exposure`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/prompt_pipeline.rs)).
- `domain_revision` — Increments each time more entities are exposed (wire field name; teaching-table revision counter).

Session identity (`prompt_hash`, `session` id) stays stable across waves; the hash is still derived from the **initial** prompt text for routing (see agent code paths).

## MCP tools

- `plasm_context`: **Call first** on each MCP connection. Pass **`intent`** (host-chosen, stable for the same agent context — see [`docs/mcp-session-reuse.md`](mcp-session-reuse.md)) and required `seeds` array of `{ api, entity }`. The server returns **`logical_session_ref`** as a stateless wire handle `l_<token>` (e.g. `l_AAAAAAAAQACAAAAAAAAAAQ`) for subsequent `plasm` calls; canonical UUID + trace identity are server-side (see [`docs/mcp-logical-sessions.md`](mcp-logical-sessions.md)).
- On a **fresh open** (no live execute binding for that logical id), the **primary** `api` is the **lexicographically first** distinct catalog id among seeds — this keeps [`SessionReuseKey`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/execute_session.rs) stable if the host reorders an equivalent seed set. **Secondary catalogs** in the same call are federated/expanded in lexicographic `api` order (after the primary), so multi-API open order does not depend on seed list order. Tool output returns **delta-only** teaching waves (no full prompt replay on federate/expand), while session symbol maps stay append-only. **MCP** `_meta.plasm.continuity` always includes `stale_binding_recovered` and `new_symbol_space` (and `discard_cached_plasm_symbols` when `new_symbol_space` is true) — when that flag is set, **discard** any prior `e#`/`m#`/`p#`/`r#` cached in the agent. Tenant MCP config scopes allowed APIs; a disallowed API fails the whole call. The teaching TSV contract teaches **named** `p#=…` / `name=…` slots for creates/updates; do not infer field meaning by permuting `p#` numerically after a new wave.
- `plasm`: Pass **`logical_session_ref`** and **`program`**. Runs a plan-only dry-run using the session’s exposure map. The response always mints an executable **`run_ref`** (`pcN`) for the reviewed plan.
- **`plasm_run`**: Live execute by passing **`logical_session_ref`** and **`run_ref`** only. Do not echo `program`; MCP `plasm_run` does not accept `wait`, `cancel`, `force`, `execute`, `plan_commit_ref`, or `page_handle`. Expensive plans await server-side and return one terminal response. See [plasm-long-operations.md](plasm-long-operations.md).

MCP initialize teaches the plan/run split alongside entity/query grammar: author programs only for `plasm`, then pass the returned **`run_ref`** to `plasm_run`. Phoenix tool-model `execute` notes mirror the same MCP await-by-default discipline ([tool-model-http.md](tool-model-http.md)).

**Intent-scoped exposure** (when `plasm_context` sets `context_intent`): capabilities on **non-seeded** entities still require lexicon overlap with `intent`. Each **seeded** `{ api, entity }` always teaches that entity’s **query / search / get** surface (and `primary_read` when declared). **Create / update / delete / action** on seeded entities require intent lexicon overlap (or appear in `ranked_capabilities` when the ranked gate is enabled). **MCP read-first open** (`read_first_seeded_exposure` on session create) defers seeded mutators unless intent scores strongly (≥ [`READ_FIRST_SEEDED_MUTATOR_MIN_SCORE`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/discovery.rs)) or the wire name is listed in `ranked_capabilities`. Federate/expand waves use the same read-first policy. Updates / deletes / actions on non-seeded entities remain intent-filtered. See [`derive_intent_exposure_surface_batch`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/discovery.rs).

Cardinality: **many** logical sessions per MCP **transport** (`MCP-Session-Id`); **one** active Plasm execute binding per **logical session** (see [`mcp_server.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/mcp_server.rs) module docs).

## Federated sessions (multi-catalog)

A single execute session (`prompt_hash` + `session`) can expose entities from **more than one** registry row (`entry_id`) **without** merging their [`CGS`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs) graphs into one artifact.

- **Prompt / symbols** — [`TeachingExposureSession`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/symbol_tuning.rs) tracks which catalog each exposed entity name belongs to via **`entity_catalog_entry_ids`** parallel to **`entities`**; `e#` assignment, `added` detection, intent-surface filters, and federated teaching deltas key on **`(entry_id, entity)`**, not bare entity names. Teaching rendering and the symbol map stay **append-only** (`e#` / `m#` / `p#` / `r#` monotonic **within that session**). Headings and tables can reflect **(registry entry, entity)** so the model knows which API each block refers to. Teaching TSV emission uses [`SymbolMap::entity_sym_for`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/symbol_tuning.rs) / `ident_sym_*_for` with the owning `entry_id`; unqualified [`SymbolMap::entity_sym`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/symbol_tuning.rs) returns a wire name when the same entity label appears in more than one catalog — agents must copy the `e#` from the row for that catalog block, not infer from `Issue` alone.
- **Execution** — The agent keeps one [`CgsContext`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/cgs_context.rs) per `entry_id` (backend URL, auth, and its own `CGS`). [`FederationDispatch`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/cgs_federation.rs) maps exposed entity names to the owning context; the runtime selects HTTP origin (and typecheck graph) **per operation**, not a single merged schema.
- **MCP** — If an execute binding already exists and `seeds` include an `entry_id` not yet in the session, the server federates that catalog into the same session (additional teaching wave, same binding). Seeds for already-loaded entries produce expand waves.
- **HTTP** — Primary flow is still `POST /execute` with one `entry_id`; extending with a second catalog may use the same federate path as MCP where implemented (see [`http_execute.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/http_execute.rs)).

**Not in scope:** global merge semantics for **colliding** entity names across catalogs — prompts are symbolic and **(catalog, entity)** disambiguates; sessions do not rely on a structural union of CGS.

## HTTP parity

`POST /execute` creates sessions the same way (incremental first wave + stored `teaching_exposure`). There is no separate HTTP route for expansion in the minimal design; MCP `plasm_context` invokes the same expand/federate paths server-side.

HTTP execute also supports **`?mode=plan`**, **`?wait=false`**, **`?force=true`**, and **`?plan_commit_ref=pcN`** on live runs — and program bodies **`wait(oN)`** / **`cancel(oN)`** (plain handles, no `l_<token>` prefix) when no MCP `plasm_context` is present. See [plasm-long-operations.md](plasm-long-operations.md).

## MCP: who orders discover vs execute?

The **host agent** (e.g. Cursor) decides **which tool to call and when**. The server surfaces **`plasm_context` first** in tool order and **`initialize` instructions** requiring it before other Plasm tools; it cannot fully enforce ordering. If the model skips search, you may see **only** `plasm_agent::http_execute` “execute expression” lines in logs — that means the client went straight to execute after (or without) a `plasm_context` open that might have happened in an earlier turn or session.

**Observability:** at `INFO`, `plasm_agent::mcp` logs **`discover_capabilities`**, **`plasm_context`**, **`plasm`**, and **`list_registry`** when those tools run, so a healthy flow shows **one discover (or retry if incomplete) → plasm_context → plasm** explicitly. Filter with `RUST_LOG=plasm_agent::mcp=info` (or `info` for the whole crate) to confirm.

## Related code

- CGS template binding helpers (teaching anchor / invoke id): [`plasm-oss/crates/plasm-core/src/schema.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs) (`template_domain_exemplar_requires_entity_anchor`, `template_invoke_requires_explicit_anchor_id`)
- Federation dispatch (multi-context, no CGS merge): [`plasm-oss/crates/plasm-core/src/cgs_federation.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/cgs_federation.rs)
- Symbol tuning and exposure: [`plasm-oss/crates/plasm-core/src/symbol_tuning.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/symbol_tuning.rs)
- Teaching synthesis: [`plasm-oss/crates/plasm-core/src/prompt_render.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/prompt_render.rs)
- Prompt pipeline: [`plasm-oss/crates/plasm-core/src/prompt_pipeline.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/prompt_pipeline.rs)
- HTTP + expand: [`plasm-oss/crates/plasm-agent-core/src/http_execute.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/http_execute.rs)
- MCP: [`plasm-oss/crates/plasm-agent-core/src/mcp_server.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/mcp_server.rs)
- Long-running plan execute: [plasm-long-operations.md](plasm-long-operations.md)
- Phoenix tool-model + execute notes: [tool-model-http.md](tool-model-http.md)

## Summary

**Prompt churn** is reduced by (1) **exact** first-wave teaching size, (2) **append-only** waves via `plasm_context` seed deltas, (3) **no duplicate teaching table** on reused opens, and (4) **monotonic** `e#`/`m#`/`p#`/`r#` so earlier examples remain valid as the session grows. **Federation** adds (5) **multi-catalog** sessions without merging CGS — same monotonic symbol stream, dispatch per [`CgsContext`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/cgs_context.rs).
