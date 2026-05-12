---
name: plasm-authoring
description: Author and validate Plasm domain models (CGS, `domain.yaml`), capability mappings (CML, `mappings.yaml`: HTTP REST, GraphQL via `transport: graphql`, composed reads via `transport: view`), and `views:` DAGs. Test interactively with `plasm-repl` against Hermit mocks or live/sandbox backends. Use when extracting schemas from OpenAPI specs, writing or editing `domain.yaml` / `mappings.yaml`, validating mappings, iteratively developing typed agent tooling, or driving Cursor / Claude / Codex / other coding agents — point them at this skill as the full playbook; do not duplicate per-API runbooks into agent prompts.
---

# Plasm Authoring

Iteratively author, validate, and test a **typed agent surface** (path expressions + DOMAIN teaching) from an API specification. This skill is the **single source of truth** for CGS / CML authoring inside `plasm-core`. The monorepo root delegates to this file; do not re-author CGS doctrine elsewhere.

The two authored files are:

- **`domain.yaml`** — CGS, the semantic model. Entities, fields, relations, capability declarations (`query`, `get`, `search`, `create`, `update`, `delete`, `action`), top-level **`values:`** registry (semantic slots), and optional top-level **`views:`** composed read DAGs.
- **`mappings.yaml`** — CML, the transport wiring. HTTP / GraphQL templates per capability, plus **`transport: view`** stubs that point at a **`views:`** key (no `method` / `path` for those rows).
- **Runtime query semantics** (no extra YAML file). **Pagination** lives on CML query mappings (`pagination:` block). **Hydration** (default concurrent GET per query row) applies when CGS declares **both** `query` and `get` for the same entity unless execution opts out. Continuations and page sizing are expressed in **Plasm** (DOMAIN / `page(pg#)` / postfix limits where taught) — not by authoring synthetic CLI flags.

For complete schema reference (types, operators, CML grammar, variable resolution, pagination block, default query hydration, action output, views, auth schemes), read [reference.md](reference.md).

## Companion skills

This skill is the authoring core. Use these companion skills for follow-on work:

- [plasm-catalog-e2e-test](../plasm-catalog-e2e-test/SKILL.md) — Hermit-first then live / sandbox transport testing.
- [plasm-catalog-polish](../plasm-catalog-polish/SKILL.md) — autonomous diagnostic / fix loop for an existing catalog.
- [plasm-catalog-score](../plasm-catalog-score/SKILL.md) — rubric scoring of catalog quality.
- [plasm-catalog-reprint](../plasm-catalog-reprint/SKILL.md) — full-cutover regeneration of a weak catalog.
- [plasm-catalog-retro](../plasm-catalog-retro/SKILL.md) — post-authoring retrospective for systemic press improvements.

The companion Cursor agent at [`.cursor/agents/plasm-forge.md`](../../agents/plasm-forge.md) wraps this skill for autonomous catalog runs.

## Domain authoring is not deterministic

**`domain.yaml` is not produced by a correct-by-construction pipeline.** There is no supported "OpenAPI → CGS" generator crate or script in this repo, and you must not add one as a substitute for human or LLM **semantic** judgement. Reasonable authors disagree on entity boundaries, which operations merge under one capability, relation shapes, `abstract` entities, and what belongs in the prompt-facing surface versus the wire-only edge. The same applies to **`values:`** identity: each key is a **semantic slot** for the catalog (gloss, prompts, validation intent) — **not** something you derive by collapsing every field that shares a primitive wire shape. Whether two slots **share** one `values` key or get **distinct** keys is an authoring judgement; default toward **separate keys** unless the domain meaning is intentionally one shared space (one enum, one id space, one taxonomy).

**What *is* deterministic (after the YAML exists):** `CGS::validate`, CML template parsing, compilation to HTTP, decoding against declared shapes, `plasm-eval coverage`, and similar checks. Those prove **internal consistency** of an authored model — not that the model is the *right* abstraction for an API.

**Implication:** Expanding an API (e.g. "full GitHub REST") is **iterative authoring** — repeated passes through the loop below — not flipping a codegen switch. If you need a huge RPC-shaped surface for experiments (e.g. MCP prompt size baselines), treat that as a **separate artifact or fork** with its own trade-offs; do not pretend it replaces a curated CGS.

## The Loop

```
1. READ spec  →  2. AUTHOR domain.yaml  →  3. AUTHOR mappings.yaml  →  4. VALIDATE  →  5. E2E TEST (Hermit, then live/sandbox)  →  6. EVAL COVERAGE
      ↑                                                                                                                                  │
      └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

### Authoring with an agent (Cursor / Claude / Codex)

Point the agent at **this skill** ([SKILL.md](SKILL.md) + [reference.md](reference.md)) as the single source of truth. The agent prompt should be minimal — for example: read the OpenAPI spec the user provides, then follow the loop above until `domain.yaml` and `mappings.yaml` cover the API surface the user asked for, validating and testing as in Steps 4–6.

**Do not** paste parallel API-specific runbooks (phased tag lists, per-vendor checklists, or duplicate rules) into the prompt; large specs are handled by **repeated passes through the same loop** (read a slice of the spec, extend the two YAML files, validate, test, repeat).

## File Structure

Canonical API catalogs live under `apis/` in this repo (the monorepo `apis/` is only a symlink to `plasm-oss/apis/`):

```
apis/<api-name>/
  domain.yaml      # entities, fields, relations, capabilities (WHAT)
  mappings.yaml    # CML templates per capability (HOW)
  README.md        # commands, auth env vars, scope, sandbox info
  eval/cases.yaml  # natural-language eval cases for plasm-eval
```

`fixtures/schemas/` holds **test-only** single-file CGS examples (e.g. `test_schema.cgs.yaml`, `capability_with_input.cgs.yaml`, `plasm_language_matrix*`); do not use it for new REST API authoring unless you are intentionally adding a tiny fixture for tests.

### NL eval cases (`plasm-eval`)

Goal-oriented harness cases live in **`apis/<api>/eval/cases.yaml`** with **`schema: <api>`** matching the directory name. Each case has a natural-language **`goal`**, soft **`expect:`** scoring, and optional **`covers:`** — a list of **expression-form buckets** this case is meant to exercise (e.g. `query_filtered`, `get`, `chain`, `multi_step`). Bucket IDs are snake_case and align with CGS-derived requirements.

- **Coverage (deterministic, no LLM):** `plasm-eval coverage --schema apis/<api> --cases apis/<api>/eval/cases.yaml` prints a human- and LLM-readable report: required buckets derived from **CGS** vs the union of per-case **`covers`**. Exit code is non-zero if any required bucket is missing (`--warn-only` to soften). Use **`--format json`** for scripts or agent consumption.
- **Optional `apis/<api>/eval/coverage.yaml`:** only **`required_extra:`** is implemented — it adds buckets beyond the CGS-derived set. There is **no** `exclude:` override.
- **Scaffold:** `plasm-eval scaffold --schema apis/<api>` emits a commented YAML fragment (CGS-derived buckets + one example case). Use `--write` to create `apis/<api>/eval/cases.yaml`; `--force` overwrites.

## Step 1: Read the OpenAPI Spec (or GraphQL SDL / vendor docs)

Read the spec file directly. For large specs, read section by section — paths first, then schemas, then descriptions. Use `wc -l` to gauge size, then `grep "^  /"` to list all paths before diving in.

**IMPORTANT: Do not write scripts, binaries, or "generator crates" to emit `domain.yaml` / `mappings.yaml` from OpenAPI (or any spec) as if the mapping were unique or mechanical.** There is no single deterministic reduction from RPC to CGS. You must read the spec and **author** the domain — merging endpoints, naming entities, choosing relations, classifying parameters, and deciding scope for agents. Mechanical dumps mirror the RPC surface and bypass the judgements this skill is for; they are explicitly out of scope for canonical `apis/<name>/` trees.

**Query parameters in particular are a non-deterministic inference step.** OpenAPI specs vary enormously in how they document query semantics:

- Some use standard `parameters:` with `in: query`
- Some put params in `requestBody` JSON schemas (e.g. Tavily `/search`)
- Some use vendor extension fields (`x-*`)
- Some rely entirely on `description` prose and examples
- Some leave parameters undocumented and rely on external docs

You must read **all of the above** — not just the canonical `parameters:` array — to correctly classify each param into `role: filter | search | sort | sort_direction | response_control | scope` and decide where it goes (CML `pagination` block, `parameters:`, or CML `query:` / `path:`). A spec where params live in extension fields or request bodies is a harder but equally valid authoring exercise.

**From the spec, identify:**

- **Schemas** (components/schemas): these become entities. Note field names, types, required fields, enums, and `$ref` links.
- **Operations** (paths): these become capabilities. Note HTTP method, path, parameters (path/query/body), request body schema, response schema, extension fields, and `description` prose.
- **Relations**: `$ref` fields (Pet.category → Category), foreign key patterns (Order.petId → Pet), path nesting (/pet/{id}/tags).
- **Shared enums**: the same enum values appearing in both a schema field and a query parameter (Pet.status and findByStatus?status= are the same `PetStatus` type).

**Also identify:**

- **Authentication**: check `securitySchemes` in `components` and `security:` at the operation or global level. Common patterns:
  - `apiKey` (in: `query`) → `api_key_query`
  - `apiKey` (in: `header`) → `api_key_header`
  - `http` (scheme: `bearer`) → `bearer_token`
  - `oauth2` (clientCredentials flow) → `oauth2_client_credentials`
  - Genuinely public → `scheme: none`

**Key principle:** An OpenAPI spec describes RPC operations. You are extracting the **domain model** — the business entities, how they relate, and what operations exist on them. Multiple endpoints operate on the same entity. A field like `petId: integer` is a relation, not just a number.

## Step 2: Author domain.yaml

Write the domain model. No HTTP details here — only what exists and what you can do.

**Value registry:** under **`values:`**, each stable key defines a **named semantic slot** (wire `type` plus optional `target`, `allowed_values`, `string_semantics`, `description`, …). Entity **fields** and capability **parameters** only **`value_ref:`** that slot — slot-level keys (`required`, `path`, `role`, …) say how *this* use site differs. Treat **one key ↔ one intended meaning** in the domain; sharing a key across sites is a **deliberate** merge (same gloss / semantics), never a mechanical "all strings dedupe" shortcut. See [reference.md — Value domains](reference.md#value-domains-values-and-value_ref).

### CRITICAL: Versioning is mandatory

- Every `apis/<api>/domain.yaml` **must** declare top-level `version: <n>` with `n > 0`.
- **Never rely on defaults.** Omitted/zero versions are invalid for authoring and packaging.
- When you change domain semantics (entities, fields, relations, capability signatures, parameter types/roles, auth contract, output/provides behavior), you **must increment** `version`.
- Treat any change that can affect prompt shape, compile/decode behavior, or runtime dispatch as a version bump event.
- If you only change prose / comments with no semantic / runtime impact, keep `version` unchanged.

**`description` strings:** On entities, capabilities, and `output` for side-effect actions, write **concise language for an agentic surface**: what the **entity** or operation is **for** in the task (goal, anchor, decision), not an inventory of typed fields and relations — the schema and teaching table already show those. Avoid tabular jargon (**"row"**) in DOMAIN-facing prose. Avoid embedding REST paths, methods, status codes, bare **`http://`** / **`https://`** links, or "see GET /…" notes — those belong in **`mappings.yaml`** comments or vendor docs, not in the CGS. **`auth.token_url`** in `domain.yaml` is the intentional exception (machine OAuth endpoint string). **Do not** repeat shapes already taught by **`value_ref`**, projection **`provides:`**, **`input_schema`** unions, or parameter names — omit field / parameter descriptions when types carry the story (see [reference.md — Gloss: do not restate typed structure](reference.md#gloss-do-not-restate-typed-structure)).

**Agentic DOMAIN copy (execute / MCP teaching):** The prompt renderer attaches **entity `description`** to the symbolic teaching table (projection witness / banner). Treat it as **imperative surface**, not a manual or vendor doc: **one or two short sentences** on **purpose** (why an agent would focus this **entity**) — **never** name **relations** or **fields** that already show up as **`p#`** arrows, bracket projections, or typed columns (that duplicates the graph and confuses "banner" with "nav map"). **Do not** summarize projection contents ("includes refs to …", "typed booleans plus …") — `p#`, relations, and types already do that. **Do not** name other capability ids, spell out call sequences ("use X then Y"), cite **`transport:`**, document HTTP error semantics, or tell agents how to seed MCP — **`discovery:`** blocks (**`operation_terms`**, **`target_terms`**, **`qualifier_terms`** on entities/capabilities), **`apis/<api>/README.md`**, and eval cases carry that operational guidance. Capability **`description:`** should state **effect** or **when to use** in domain terms; move cross-capability playbooks into **`discovery`** on the relevant capability. See [reference.md — DOMAIN-facing descriptions](reference.md#domain-facing-descriptions-entities-and-capabilities).

```yaml
values:
  nv_pet_id:
    type: integer
  nv_pet_name:
    type: string
    string_semantics: short
  nv_pet_status:
    type: select
    allowed_values: [available, pending, sold]
  nv_category_id:
    type: integer
  nv_category_name:
    type: string
    string_semantics: short
  nv_pet_find_by_status_status:
    type: select
    allowed_values: [available, pending, sold]

entities:
  Pet:
    id_field: id
    fields:
      id:
        value_ref: nv_pet_id
        required: true
      name:
        value_ref: nv_pet_name
        required: true
      status:
        value_ref: nv_pet_status
    relations:
      category:
        target: Category
        cardinality: one
      tags:
        target: Tag
        cardinality: many

  Category:
    id_field: id
    fields:
      id:
        value_ref: nv_category_id
        required: true
      name:
        value_ref: nv_category_name

  # Include ALL entities referenced by relations, even simple ones

capabilities:
  pet_findByStatus:
    kind: query
    entity: Pet
    parameters:
      - name: status
        value_ref: nv_pet_find_by_status_status
        required: true
  pet_get:
    kind: get
    entity: Pet
  pet_create:
    kind: create
    entity: Pet
  pet_delete:
    kind: delete
    entity: Pet
```

**DOMAIN projection (prompt teaching, not decode):** Optional per-entity **`domain_projection_examples`** (default **true**) and **`primary_read:`** select which Get capability's ordered **`provides:`** drives the **`Entity  ;;  [f1,…,fN]`** heading in DOMAIN instructions. Set **`domain_projection_examples: false`** to omit that bracket line. Declare explicit ordered **`provides:`** on the primary Get so the heading matches the fields you materialize (see [reference.md — Entities](reference.md#entities)).

**String fields:** on the corresponding **`values:`** row with **`type: string`**, set **`string_semantics:`** for every non-trivial string (`short`, `markdown`, `document`, `html`, `json_text`, …); plain `short` is the default when omitted.

**Field / parameter wire types:** the vocabulary (`string`, `integer`, `number`, `boolean`, `select`, `multi_select`, `date`, `array`, `entity_ref`, **`blob`**, `uuid`) is expressed as **`type:`** on a **`values:`** row, not as inline `field_type` on the slot. For **`entity_ref`**, set **`target: EntityName`** on the value row. For **`blob`**, see [reference.md — Blob / binary](reference.md). For **`array`**, the value row has **`type: array`** and **`items: { value_ref: <element_key> }`**; the element shape is another `values:` row. **`multi_select`** requires non-empty `allowed_values` on its value row.

**Wire narrowing (`value_format`):** for every **`values:`** row with **`type: date`**, set **`value_format`** on that row (`rfc3339`, `iso8601_date`, `unix_ms`, `unix_sec`, or map form). Same for date-typed capability parameters and `input_schema` fields.

### CGS field typing checklist (strict)

Use this on every new or edited entity (and on capability `parameters:` / `input_schema` fields) so the model does not collapse to "stringly typing."

1. **Instants and calendar dates** — If the wire is a timestamp or date, use a **`values:`** row with **`type: date`** and the correct **`value_format`**. **Do not** use `string` for fields named like `date_created`, `date_updated`, `last_modified`, `*expires*`, `*_on`, or `last_*` when the API returns a normal machine date/time.
2. **Enumerations** — If the set of values is closed and known, use **`select`** / **`multi_select`** with **`allowed_values`** on the value row. If the vendor reuses a field across resources with inconsistent or extensible lifecycles, keep **`string`** and do not force a narrow `select` that rejects valid future wire values.
3. **Foreign keys** — If the value is another resource's id and that resource is in the CGS, use **`type: entity_ref`** and **`target:`**.
4. **Reverse list edges (many)** — When a child has `entity_ref` to a parent and the child's primary list query accepts a parameter that filters by that parent's id, declare a **`cardinality: many` relation on the parent** with **`materialize: { kind: query_scoped, capability: <child_query>, param: <parent_id_param> }`**. Do **not** add a `relations` key with the same name as an `entity_ref` field on the same entity.
5. **Opaque bytes and file bodies** — Use **`type: blob`**.
6. **Human text and opaque tokens** — Use **`type: string`** with explicit **`string_semantics:`**.

Apply the same rules to **`parameters:`** `value_ref` targets.

**Capability kinds:** `query` (collection filter), `search` (free-text relevance), `get` (by ID), `create`, `update`, `delete`, `action` (anything else).

### Composed read models (`views:`)

When the agent-facing concept is a **single read row** that **no single vendor endpoint returns**, but it **decomposes** into several **`query` / `get`** capabilities you already modeled, you **must** express it in CGS:

1. Add an **`entities:`** row for that concept (often **`abstract: true`** so discovery does not attach it to parent graphs until explicitly seeded).
2. Declare a **`kind: query`** capability on that entity; **`parameters:`** are the scope inputs the composition needs.
3. Add **`views:<key>`** with ordered **`nodes`** (each runs an existing capability), **`bind`** maps for node inputs, and **`output`** maps that shape entity fields.
4. In **`mappings.yaml`**, wire that capability with **`transport: view`** and **`view: <key>`** only.

**Anti-pattern:** Long playbook text that says "call A, then B, then aggregate" **without** a **`views:`** entry leaves agents without a single **`query`** symbol for the composed row. Canonical example: **`apis/cloudflare`** — **`SecurityOverview`** + **`security_overview_query`** + **`views.security_overview`**. Full grammar: [reference.md — Composed read views](reference.md#composed-read-views).

**`kind: action` output:** Every action must declare either non-empty **`provides:`** or **`output:`** with **`type: side_effect`** and a non-empty `description:` that states **what** the operation changes. There is no `output.type: none`. See [reference.md — Action output](reference.md#action-output-provides-vs-outputside_effect).

### Authentication — top-level `auth:` block

Place a single `auth:` block at the end of `domain.yaml`. The runtime reads secrets at execution time from the named environment variables — no secrets go in schema files. Use `scheme: none` for genuinely public APIs.

```yaml
# API key in query string (e.g. RAWG ?key=..., OMDb ?apikey=..., NYT ?api-key=...)
auth:
  scheme: api_key_query
  param: key
  env: RAWG_API_KEY

# API key as a request header (e.g. X-Api-Key)
auth:
  scheme: api_key_header
  header: X-Api-Key
  env: MY_SERVICE_API_KEY

# Bearer token (e.g. ClickUp, Notion, Tavily)
auth:
  scheme: bearer_token
  env: CLICKUP_API_TOKEN

# OAuth 2.0 client credentials (e.g. Spotify)
auth:
  scheme: oauth2_client_credentials
  token_url: https://accounts.spotify.com/api/token
  client_id_env: SPOTIFY_CLIENT_ID
  client_secret_env: SPOTIFY_CLIENT_SECRET
  scopes: []

# Public API with no outbound credentials
auth:
  scheme: none
```

### Query capability parameters

**Critical rule: only declare parameters the API endpoint actually accepts as HTTP inputs.** Read the OpenAPI operation's `parameters` list and `description` fields. Never generate `parameters:` from entity fields — entity fields describe the domain object, not what the query endpoint accepts.

**No parameters?** If the endpoint is a plain paginated resource index (e.g. PokéAPI `/pokemon/` — no server-side filters, just offset/limit), declare **no `parameters:`**. Pagination belongs in **`mappings.yaml`**.

#### `kind: query` vs `kind: search`

Use **`kind: search`** when the endpoint's primary interface is a **free-text relevance query** (`q`, `query`, `search`) that returns ranked results rather than field-filtered rows. If the endpoint filters by concrete field values (`status`, `archived`, `team_id`), use `kind: query`.

#### Classification: where each query param goes

| Param examples | Role | `role:` annotation | Where it goes |
|----------------|------|--------------------|---------------|
| `offset`, `limit`, `page`, `cursor`, `after`, `before` | **Pagination** | — | CML `pagination` block only — **not** in `parameters:` |
| `status`, `tags[]`, `assignees[]`, `archived`, `type` | **Filter** | `filter` *(default, omit)* | `parameters:` + CML `query:` var |
| `q`, `search`, `query` | **Full-text search** | `search` | `parameters:` (`value_ref` → `values:` string row) + CML `query:` var; use `kind: search` |
| `order_by`, `sort_by` | **Sort field** | `sort` | `parameters:` + CML `query:` var |
| `sort`, `direction`, `asc`/`desc` | **Sort direction** | `sort_direction` | `parameters:` + CML `query:` var |
| `market`, `locale`, `country`, `embed`, `fields`, `inc` | **Response control** | `response_control` | `parameters:` + CML `query:` var |
| `team_id` in `GET /team/{team_id}/space` | **Parent-scoped sub-resource** | `scope` | `parameters:` (`value_ref` → `values:` `entity_ref`, required) + CML `path:` var |

**Scoped sub-resource queries:** When an API has endpoints like `GET /classes/{class_index}/spells` alongside `GET /spells`, declare both as separate `kind: query` capabilities on the same entity. The one with a required `role: scope` parameter is the **scoped list** (pair with relation **`materialize`** so parents supply scope); the unscoped one is the generic index.

**Range filters** like ClickUp's `due_date_gt` / `due_date_lt` or Spotify's `min_energy` / `max_energy` are **separate named parameters** — declare each one individually in `parameters:`, not as one field with operator suffixes.

**Array / multi-value params** (e.g. `genres`, `assignees[]`, `embed[]`): use a **`values:`** row with **`type: multi_select`** or **`type: array`** with **`items: { value_ref: <element_key> }`**. In mappings.yaml:

- **Repeated key** (`?embed=a&embed=b`) — plain `{ type: var, name: embed }`; HTTP layer expands arrays.
- **CSV** (`?genres=1,2,3`) — `{ type: join, sep: ",", expr: { type: var, name: genres } }`.
- **Pipe** (`?ids=1|2|3`) — `{ type: join, sep: "|", expr: { type: var, name: ids } }`.

See [reference.md](reference.md) for the full pattern catalogue (index-only, filter, rich filter, search, sort + response control).

**Rows without a top-level id:** Some list endpoints return objects with no `id` key (only nested URLs or names). Set optional **`id_from`** to a path of object keys (YAML list or dotted string, e.g. `location_area.url`) so decoding can still build stable `_ref`s.

**Checklist before proceeding:**

- [ ] Every relation target is a defined entity
- [ ] Every `values:` row with `type: select` (or `multi_select`) has non-empty `allowed_values`
- [ ] Every required query parameter has `required: true`
- [ ] Capability names are unique and follow `entity_operation` convention
- [ ] Every entity has either a declared `fields` entry for `id_field` or a non-empty `id_from` path
- [ ] If an entity should **not** teach projection brackets in DOMAIN, set `domain_projection_examples: false`
- [ ] Any multi-endpoint read summary is modeled with `views:` + synthetic `query` + `transport: view` (not prose-only runbooks)

### Scoped relation traversal (`materialize`)

When an API uses sub-resource URLs (`/parent/{id}/children`), set **`materialize`** on the **many** relation so chain traversal fills the target query's scope parameter(s). See [reference.md — Scoped many-relations](reference.md#scoped-many-relations--materialize-query_scoped--query_scoped_bindings).

### EntityRef fields

When a field stores another entity's ID, use **`type: entity_ref`** with **`target:`** on the value row. This enables FK navigation, reverse traversal, and cross-entity predicates. Audit every `string`/`integer` field ending in `_id`, `Id`, `_key` — if a matching entity exists with a `get` capability, it should be `entity_ref`. See [reference.md — Foreign key fields](reference.md#foreign-key-fields-entity_ref).

## Step 3: Author mappings.yaml

Write the transport wiring for each capability. Default is REST (`method` + `path`). GraphQL uses top-level **`transport: graphql`** with `endpoint` / `operation` / variables. Composed views use **`transport: view`** + **`view: <key>`** matching `domain.yaml`'s `views:`.

```yaml
pet_findByStatus:
  method: GET
  path:
    - type: literal
      value: pet
    - type: literal
      value: findByStatus
  query:
    type: object
    fields:
      - - status
        - type: var
          name: status

pet_get:
  method: GET
  path:
    - type: literal
      value: pet
    - type: var
      name: id

pet_create:
  method: POST
  path:
    - type: literal
      value: pet
  body:
    type: var
    name: input
```

**Path segments:** `{type: literal, value: "pet"}` (static) or `{type: var, name: "id"}` (resolved at runtime).

**Variables the engine provides:**

- `id` — entity ID (for get/delete/update/action)
- Predicate field names — from query predicates in the Plasm program
- `input` — full input object (create/update with body)
- Path variables: see [reference.md](reference.md) (primary `id`, optional `path_vars`, multi-segment, create `input` keys merged into CML env)

### Pagination & hydration

**Pagination** — declare only in **`mappings.yaml`** (`pagination` block on **query** capabilities). Infer `style`, wire param names, and JSON paths per [reference.md — Pagination](reference.md#pagination-cml--mappingsyaml-only). The runtime merges **`pagination.params`** into follow-up HTTP requests; **`pagination:`** in CML is the single authoring surface for paging behavior.

**Hydration** — after a query, if the entity has both `query` and `get`, the runtime **by default** fetches full rows via `get` unless `QueryExpr.hydrate = Some(false)` or the engine disables hydrate. No extra CGS flag. See [reference.md — Query result hydration](reference.md#query-result-hydration-runtime).

## Step 4: Validate

For split `domain.yaml` + `mappings.yaml`, pass the **catalog directory** `apis/<api>/` to `schema validate`. Pointing at `domain.yaml` alone skips `mappings.yaml` and can falsely fail.

```bash
# CGS validation (catalog directory for split domain+mappings)
cargo run -p plasm-cli --bin plasm -- schema validate apis/<api>

# Optional: exhaustive mapping exercise against an OpenAPI spec
cargo run -p plasm-cli --bin plasm -- validate --schema apis/<api> --spec path/to/openapi.json

# Smoke-load REPL + help
cargo run -p plasm-repl -- --schema apis/<api> --backend http://localhost:1080 --help
```

## Step 5: End-to-End Testing

Hand off to [plasm-catalog-e2e-test](../plasm-catalog-e2e-test/SKILL.md), which is the operational source of truth for the testing ladder:

1. **Hermit** against the OpenAPI spec when one exists in the README or source docs.
2. **Live API** when credentials and rate-limit headroom exist.
3. **Vendor sandbox / test mode** as a substitute when live calls would mutate real data or are otherwise unsafe.

Skips must be recorded with a reason; representative Plasm expressions and outcomes belong in the evidence the e2e skill emits.

## Step 6: Eval Coverage

```bash
cargo run -p plasm-eval -- coverage --schema apis/<api> --cases apis/<api>/eval/cases.yaml
```

Fix missing buckets by adding eval cases (not by softening coverage). For LLM conformance runs, see `plasm-oss/crates/plasm-eval/README.md`.

## Failure Modes (quick reference)

When transport tests fail, check:

- `CmlError::VariableNotFound` → mapping var name doesn't match the engine env. Fix `name` in mappings.yaml.
- `DecodeError` → response shape mismatch. The engine normalizes bare arrays automatically; check `path:` / `derive:` on entity fields.
- `404` → path doesn't match spec. Check `--backend` includes the server prefix.
- `401` / `403` → wrong `auth:` scheme or missing env var.

Then fix `domain.yaml` or `mappings.yaml` and re-run from Step 4.

## When you cannot model the API faithfully

If the desired API shape **cannot** be modeled with today's CGS + CML + runtime (missing expressiveness, not just tediousness), **stop**. Document the gap as a short blocker note (what shape is needed, which capability/entity breaks, which validator or runtime behavior is insufficient). **Do not** patch `plasm-core`, `plasm-cml`, `plasm-runtime`, or validators yourself to "unstick" the mapping unless explicitly directed in a separate task.

After a difficult or interesting catalog, run [plasm-catalog-retro](../plasm-catalog-retro/SKILL.md) to capture systemic improvements.
