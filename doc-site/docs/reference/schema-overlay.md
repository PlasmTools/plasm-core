# Schema overlay (runtime CGS extension)

Catalogs with dynamic workspace schema declare a **`schema_overlay:`** block in `domain.yaml`. At execute session open the host:

1. Executes the declared **`source`** pipeline via the normal CML/HTTP stack (same auth and backend as the session).
2. Projects rows from the merged JSON response into entity definitions using Minijinja templates and JSON paths.
3. Merges the overlay with [`CGS::with_overlay`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema_overlay.rs) and pins the session with [`effective_catalog_cgs_hash_hex`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema_overlay.rs). **`augment_base`** merges dynamic fields into the existing bootstrap entity name; **`per_scope_entity`** adds new typed entities.
4. Routes decode for scoped capabilities through **`decode.scope`** → **`schema_overlay_scope_index`**.

No vendor-specific logic lives in `plasm-core` or `plasm-agent-core`; catalogs express *what* to project. **Overlay configuration always comes from vendor API responses** — not env vars, HTTP bodies, or MCP seed fields.

**Authoring:** When to add `schema_overlay:` to a catalog — bootstrap shape, projection modes, checklists, and reference APIs — is documented in [plasm-oss/skills/plasm-authoring/reference.md](../authoring/reference.md#runtime-schema-overlay-schema_overlay) (canonical skill; monorepo path). This file covers runtime behavior and spec keys.

## Spec

| Key | Role |
|-----|------|
| `source.capability` | Single-step source (Fibery, Notion): one unscoped fetch |
| `source.steps` | Multi-step pipeline when schema fetch needs a scope param (ClickUp, Jira) |
| `source.steps[].collect` | Store response rows under a name (`items_path` required) |
| `source.steps[].for_each` | Iterate a prior collect; `bind` templates receive `{ row, parent, env }` from API rows |
| `source.steps[].merge` | Accumulate `for_each` responses (e.g. `append_array` on `fields` or `projects`) |
| `projection.mode` | `per_scope_entity` (default) or `augment_base`; `column_schema` deferred |
| `projection.items_path` | Walk merged response JSON to the generator row array |
| `projection.nested_items_path` | Optional nested array path per top-level row (`{ row, parent }` in templates) |
| `entity.from_template` | Static entity to clone (`id_field`, `key_vars`, base fields) |
| `entity.name.template` | Minijinja → Plasm entity name (`per_scope_entity` only) |
| `entity.scope_key.template` | Minijinja → scope key for decode routing |
| `entity.static_fields` | Fixed fields merged onto the template (`value_ref` + template) |
| `entity.dynamic_fields.from` | `kind: array` or `kind: object_map` + `path` |
| `entity.dynamic_fields.skip` | `wire_name_path` + `values_in` |
| `entity.dynamic_fields.extract` | `top_level_key`, `path_segments`, or `name_value_array` |
| `decode.scope.params` | Ambient param names required for decode routing |
| `decode.scope.key` | Minijinja template building the scope index lookup key |
| `decode.capabilities` | Capabilities that use overlay entity decode |

## Source patterns

| Pattern | Mechanism | Example catalogs |
|---------|-----------|------------------|
| Single unscoped fetch | `source.capability` only | Fibery `schema_query`, Notion `database_search` |
| Multi-fetch pipeline | `source.steps`: list → scoped fetch per row → merge | ClickUp `team_query` → `custom_field_query`; Jira `project_query` → `issue_createmeta_get` |

Example multi-step block:

```yaml
schema_overlay:
  source:
    steps:
      - capability: team_query
        collect: teams
        items_path: [teams]
      - capability: custom_field_query
        for_each: teams
        bind:
          team_id: "{{ row.id }}"
        merge:
          kind: append_array
          path: [fields]
  projection: { ... }
```

Agents and MCP clients supply **`{ api, entity }` seeds only** — no parallel overlay configuration channel.

## Projection modes

| Mode | Outcome | Example catalogs |
|------|---------|------------------|
| `per_scope_entity` | One typed entity per schema row (clone `from_template`) | Fibery, Notion, Jira |
| `augment_base` | Dynamic columns merged onto a single base entity | ClickUp custom fields |
| `column_schema` | *(Deferred)* spreadsheet column headers | Google Sheets |
| Linear issue custom fields | *(Deferred)* | Public GraphQL schema has no custom-field definition query |

## Field catalog sources

| `from.kind` | Schema shape | Example |
|-------------|--------------|---------|
| `array` | Nested array of field defs; empty `path` treats each generator row as one field | Fibery `fibery/fields` |
| `object_map` | JSON object map (keys = field names) | Notion `properties` |

## Field extract kinds

| `extract.kind` | Decode behavior |
|----------------|-----------------|
| `top_level_key` | Response field at top-level key matching wire name |
| `path_segments` | `wire_path` from static + templated segments (e.g. `properties/{{ field_key }}`) |
| `name_value_array` | `wire_path` to array + `FieldDeriveRule::NameValueArrayLookup` (e.g. ClickUp `custom_fields`) |

## Minijinja filters (overlay projector)

- **`join_sanitize(separator, split_on)`** — split a string, sanitize segments, join (e.g. `Space/Name` → `Space__Name`)
- **`sanitize_identifier`** — wire name → Plasm field identifier

## Session resolver

[`schema_overlay_session.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/schema_overlay_session.rs) runs before entity validation when opening execute sessions:

| Surface | Entry point |
|---------|-------------|
| HTTP `POST /execute` | [`http_execute.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/http_execute.rs) |
| MCP `plasm_context` | same execute session create (via [`apply_capability_seeds`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/http_execute.rs)) |
| Federated attach | [`federate_execute_session`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/http_execute.rs) |
| Remote `plasm` CLI | HTTP client → server execute path above |
| Local `plasm-repl` | [`plasm-repl/src/lib.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-repl/src/lib.rs) at startup |

Optional TTL cache: `PLASM_SCHEMA_OVERLAY_TTL_SECS` (default **600**). Multi-step pipelines cache on a digest of all step HTTP responses.

## Reference catalogs

- [Fibery](https://github.com/PlasmTools/plasm-core/tree/main/apis/fibery/domain.yaml) — `per_scope_entity`, array field catalog, `top_level_key` extract
- [Notion](https://github.com/PlasmTools/plasm-core/tree/main/apis/notion/domain.yaml) — `per_scope_entity`, `object_map` on `properties`, `path_segments` extract
- [Jira](https://github.com/PlasmTools/plasm-core/tree/main/apis/jira/domain.yaml) — `project_query` → `issue_createmeta_get` per project, `nested_items_path` on createmeta
- [ClickUp](https://github.com/PlasmTools/plasm-core/tree/main/apis/clickup/domain.yaml) — `team_query` → `custom_field_query` per team, `augment_base` on `Task`

**Note:** `decode.scope` ambient params (e.g. Notion `database_id` on `database_query`) are **operational** — they route decode during program execution after schema was already fetched. They are not overlay configuration.

## Deferred

| Item | Blocker |
|------|---------|
| Pagination on list steps at session open | First-page only today; may need explicit pagination loops |
| Linear `augment_base` overlay | Public GraphQL schema lacks custom-field definition query |
| Lazy per-scope overlay | Host complexity + cache invalidation |
| `column_schema` mode | Needs row entity model in google-sheets catalog |
| Custom spec-defined Minijinja filters | Security review |

## Tests

Matrix fixtures (no `apis/*` in core tests):

- [`fixtures/schemas/fibery_schema_overlay/`](https://github.com/PlasmTools/plasm-core/tree/main/fixtures/schemas/fibery_schema_overlay/)
- [`fixtures/schemas/notion_schema_overlay/`](https://github.com/PlasmTools/plasm-core/tree/main/fixtures/schemas/notion_schema_overlay/)
- [`fixtures/schemas/jira_schema_overlay/`](https://github.com/PlasmTools/plasm-core/tree/main/fixtures/schemas/jira_schema_overlay/)
- [`fixtures/schemas/augment_base_overlay/`](https://github.com/PlasmTools/plasm-core/tree/main/fixtures/schemas/augment_base_overlay/)
- [`fixtures/schemas/clickup_schema_overlay/`](https://github.com/PlasmTools/plasm-core/tree/main/fixtures/schemas/clickup_schema_overlay/)

```bash
cargo test -p plasm-core schema_overlay
cargo test -p plasm-agent-core schema_overlay_session
cargo test -p plasm-runtime schema_overlay
```
