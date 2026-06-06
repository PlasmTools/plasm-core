# Tool model HTTP API

`plasm` exposes an operator-facing JSON projection aligned with teaching table prompt rendering and the dynamic CLI (`cli_builder`), not raw CGS parsing in clients.

## `GET /v1/registry/{entry_id}/tool-model`

- **Query**
  - `focus` — `all` (default), `single`, or `seeds`.
  - `entity` — repeat for `single` (exactly one name) or `seeds` (one or more). Omit for `all` (and do not send `entity=` with `focus=all`).
- **Response (summary)**
  - `entry` — `entry_id`, `label`, `tags` (same as registry list rows).
  - `focus` — `mode` and `resolved_entities` (entity names included in this slice).
  - `overview` — `entity_count`, `relation_edge_count`, `verb_count`.
  - `entities` — per-entity CLI-shaped `verbs`, declared `relations`, derived `reverse_traversals`, `entity_ref_links`, and `domain_lines` (parallel to teaching table).
  - `domain.model` — full `DomainPromptModel` (structured teaching table metadata: kinds, cross-entity hints, relation materialization summaries).

Errors use `application/problem+json`; unknown `entry_id` matches discovery `404` semantics; invalid focus/entity combinations return `400` with type `https://plasm.invalid/problems/plasm-tool-model-bad-request`.
