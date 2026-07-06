# CGS & CML Reference

This is the canonical OSS reference for authoring Plasm API catalogs. The compiler and runtime crates are the ground truth (`crates/plasm-core`, `crates/plasm-cml`, `crates/plasm-compile`, `crates/plasm-runtime`); this file exists to keep agents and humans aligned on doctrine.

## Authoring vs determinism

**Writing `domain.yaml` is not a deterministic process.** OpenAPI (and friends) do not uniquely define a CGS: entity boundaries, capability grouping, relations, parameter roles, `abstract` embed-only types, and **which `values:` keys exist** (including whether unrelated fields share one `value_ref` vs split into distinct slots) are **semantic choices**. Tools may assist (e.g. an LLM reading the spec), but there is no canonical auto-generator in-repo and no guarantee that two valid domain models for the same API are equivalent.

**After** the YAML is written, **validation and compilation** are deterministic: schema checks, CML parse/compile, and runtime request shaping are mechanical consequences of what you authored.

## Task-oriented catalogs (mandatory)

Plasm catalogs model **user tasks and domain entities**, not vendor wire surfaces. CGS exists to compress APIs into a relational graph for agents — not to mirror every REST path or GraphQL operation.

**Core rule:** If capability count tracks the API's query/mutation root (or OpenAPI path list), the model is wrong. Author **what agents ask for** (search, context, dashboard, manage lifecycle), then wire CML to whatever HTTP/GraphQL the vendor exposes.

**Anti-patterns (reject at review):**

- One capability per GraphQL field, REST path, or SDK method
- Granular mutation capabilities that split one domain verb (e.g. separate add-label and remove-label caps instead of `issue_update` with label parameters)
- UUID-primary `id_field` when the vendor accepts human keys (`ENG-42`, Jira `PROJ-1`, team key `ENG`)
- A fleet of scoped `query` capabilities that duplicate what `kind: search` should express
- Prose playbooks ("call A then B") without a `views:` entry for multi-hop reads
- Static explosion of workspace custom columns (`customfield_*`, per-database property fields) when runtime schema fetch exists — use **`schema_overlay:`** (see [Runtime schema overlay](#runtime-schema-overlay-schema_overlay))

**Task → mechanism (issue trackers and similar):**

| User task | Model as |
|-----------|----------|
| Find / filter work | `kind: search` with name-typed filter params; CML builds vendor filter objects |
| "What's ENG-123?" | Human-key `id_field` on Issue + **`IssueContext` view** |
| "What should I work on?" | **`MyWorkSnapshot` view** (viewer + filtered issues) |
| Manage lifecycle | `create` / `update` / `delete`; human-key `entity_ref` where wire allows |
| Portfolio / status | **`ProjectContext` view**; milestone/initiative/update entities as needed |
| Documents | `Document` entity + search + update |
| User-defined columns on rows (DB properties, custom fields) | Bootstrap generic row + **`schema_overlay:`** at session open |

**Human-key resolution:** Prefer **`id_field` = human-visible key** (Jira `Issue.id_field: key`, Linear `Issue.id_field: identifier`, `Team.id_field: key`) so agents write `Issue(ENG-42)`, `Team(ENG)`. Prefer **string/name filter params** on `kind: search` (`team_key`, `state_name`, `label_name`, `assignee_name`) over UUID **`entity_ref`** for filters — map names to vendor filter objects in **mappings.yaml**. Keep wire `id` fields on entities for decode; UUID discovery is not an agent step.

**GraphQL-specific:** SDL/introspection is **evidence for CML**, not a capability checklist. Do not derive capability ids from the mutation/query root list.

Reference: [Linear #1035](https://github.com/linear/linear/issues/1035) (task-shaped MCP tools) maps to Plasm `search` + `views:` + compressed writes.

## Obsolete or unsupported (do not teach)

| Topic | Status |
|-------|--------|
| **`domain_projection_fields`** | Removed — use **`domain_projection_examples`**, optional **`primary_read`**, and explicit ordered **`provides:`** on the primary Get (see [Entities](#entities)). |
| **`output.type: none`** | Removed — actions need **`provides:`** and/or **`output: { type: side_effect, description: … }`**. |
| **CGS as `.json`** | **Not loaded** — `load_schema` rejects JSON paths; use a directory with **`domain.yaml` + `mappings.yaml`**, a combined authoring **`.yaml`**, or **`.cgs.yaml`** interchange. |
| **`apis/<api>/eval/coverage.yaml` `exclude:`** | **Not implemented** — only **`required_extra`** exists in `plasm-eval` coverage overrides. |
| **`string` + `string_semantics: blob`** | **Legacy** — the loader normalizes this to **`blob`** in the resolved CGS and clears blob string semantics. Prefer a **`values:`** row with **`type: blob`**. |
| **Inline `field_type:` / `type:` on entity `fields:` or on `parameters:` rows** | **Removed from split `domain.yaml` authoring** — wire shapes live only under top-level **`values:`**; slots use **`value_ref:`**. Exception: **`input_schema.input_type.fields`** remain full **`InputFieldSchema`** rows (**`value_ref` + `field_type`** mirrors); they must agree with **`values[value_ref]`** (see [Value domains](#value-domains-values-and-value_ref)). |

## How CGS, CML, and runtime fit together

| Layer | Artifact / crate | Role in list queries |
|-------|------------------|----------------------|
| **CGS** | `domain.yaml` | Declares entities and capability **kinds** (`query`, `get`, …). Optional **`views:`** declares **composed read-only** DAGs over existing capabilities (see [Composed read views](#composed-read-views)). Optional **`schema_overlay:`** merges **workspace-specific typed entities or columns** at execute session open (see [Runtime schema overlay](#runtime-schema-overlay-schema_overlay)). Whether an entity has **both** query and get determines whether **hydration** is eligible — not how HTTP works. |
| **CML** | `mappings.yaml` | Compiles each capability to HTTP/GraphQL — or **`transport: view`** for view-backed queries (no outer HTTP template). Optional composable **`pagination:`** (`PaginationConfig`: `params`, `location`, …) on **query** mappings drives multi-request pagination; list decode shape lives in **`response:`** / decoder config. |
| **Runtime** | `plasm-runtime`, `plasm` | Evaluates CML, executes **views** as internal DAGs (HTTP only for inner nodes), loops pages when execution asks for more rows (postfix limits, session **`page(pg#)`** continuations, or internal caps), decodes rows, merges into `GraphCache`. **LLM / MCP execute** uses opaque **`page(pg#)`** handles instead of exposing raw API pagination field names. Then (by default) runs **concurrent GET** per row to upgrade **summary → complete** when CGS has a **get** for that entity. |

**Pagination wiring** is a **CML** concern; **opaque LLM paging handles** are minted by **`plasm`** execute sessions. **Hydration** is a **runtime** policy gated by **CGS** capability pairs (`query` + `get`).

## CGS (Capability Graph Schema) — domain.yaml

The CGS is the semantic domain model. It declares what entities exist, how they relate, and what operations are available. It contains no HTTP details.

### CRITICAL: Versioning is mandatory

- Every `apis/<api>/domain.yaml` must declare top-level `version: <n>` where `n > 0`.
- Version defaulting is forbidden; omitted/zero versions are invalid for authoring and catalog packaging.
- Increment `version` whenever domain semantics change (entities, fields, relations, capability signatures, parameter typing/roles, auth contract, output/provides behavior).
- Keep version unchanged only for non-semantic text edits (comments/prose) that do not affect runtime behavior, prompts, compile/decode, or dispatch.

### Value domains (`values:`) and `value_ref`

Split **`domain.yaml`** declares a catalog-local registry of **named semantic slots** under top-level **`values:`** (stable keys, usually `snake_case`). Each row carries the **wire** `type:` and gloss-related keys — the same vocabulary as the former inline `field_type` / param `type` — but the **key** is a semantic identity for this catalog, not "dedupe by primitive wire shape alone":

- **`type:`** — `string`, `integer`, `number`, `boolean`, `select`, `multi_select`, `date`, `array`, `entity_ref`, **`blob`**, `uuid`.
- Type-specific keys on the **value row**: `target` (`entity_ref`), `allowed_values` (`select` / `multi_select`; multi_select must be non-empty), `value_format` (`date`), `string_semantics` (`string`), **`items: { value_ref: <key> }`** (`array` — element shape is another `values` row).

**Entity `fields:`** and **`capabilities.*.parameters:`** list entries declare **only** how that slot uses a shape:

- **`value_ref: <key>`** — required; must exist in **`values:`**.
- **`required`**, **`description`**, **`path`**, **`derive`** — on fields (and parameter-specific keys: **`role`**, **`description`** on parameters).
- Presentation / attachment hints (**`agent_presentation`**, **`mime_type_hint`**, **`attachment_media`**) live on the **field slot** when they apply (not duplicated on every reuse of the same value key).

**Semantic slots (authoring judgement):** A **`values:`** key is not "the type `string`" or "the type `integer`" in the abstract — it is a **catalog-local semantic identity**: what teaching gloss, `string_semantics`, `description`, and validation **say** that value *means* in this API. Two different columns can share the same on-wire JSON type (`string`, RFC3339 `date`, …) yet must remain **different keys** when their **meaning** differs (e.g. `owner` vs `repo` vs `html_url`). **Sharing** one key across multiple `value_ref` sites is the same class of decision as **relation cardinality** or **whether two endpoints are one capability**: there is **no** deterministic rule from the wire alone — authors choose when two sites are intentionally **the same domain value space** (one enum, one id space, one taxonomy, aligned gloss). Prefer **distinct keys per field/param by default**; merge only when that identity story is obvious and descriptions stay compatible.

**Sharing `values` keys:** Only point multiple slots at the **same** `values` key when they are intentionally the same domain concept (e.g. one shared enum, or the same `entity_ref` target meaning the same id space) **and** gloss text is compatible. **Never** merge unrelated strings, integers, or dates solely because the wire type matches — use distinct keys per slot (`nv_<entity>_<field>`, `nv_<capability>_<param>`) so `description` / `string_semantics` stay truthful.

**Canonical `values:` keys (optional entropy control):** the monorepo carries an optional `scripts/dedupe_primitive_domain_values.py` helper (outside this OSS submodule) whose `--canonicalize-primitives` mode collapses duplicate anonymous rows in the same `domain.yaml` when two or more keys share the same normalized body:

- **Primitives** → fixed names: `nv_wire_str_short`, `nv_wire_str_markdown`, `nv_wire_int`, `nv_wire_num`, `nv_wire_bool`, `nv_wire_date_rfc3339` (empty `values:` `description`; no `items` / `target`; `allowed_values` absent or `[]`; only the scalar keys required for that shape).
- **Closed sets** → `nv_wire_sel_<16hex>` / `nv_wire_msel_<16hex>` from a SHA-256 of the normalized `{ type, allowed_values }` body (`allowed_values` sorted and deduped for fingerprinting).

**`--write`** rewrites every `value_ref` (including nested `items.value_ref`), removes merged keys, bumps `version:`, and reorders `values:` **topologically** so `items.value_ref` targets appear before parents (required by `load_schema`). Re-run `cargo test -p plasm-core` on touched catalogs before committing. Rows with non-empty `description`, arrays, entity refs, or extra YAML keys stay bespoke.

**`input_schema` (create / update / action body):** YAML uses full **`InputFieldSchema`** interchange: each object field has **`name`**, **`value_ref`**, **`field_type:`** (singleton map), plus mirrors (`value_format`, `allowed_values`, `array_items`, `string_semantics`, …). Those mirrors must match **`CGS::values[value_ref]`** — `CGS::validate` / registry denormalization rejects drift. Prefer defining the shape once under **`values:`** and copying the mirrored keys from that row.

Combined **`.cgs.yaml`** interchange may still show denormalized **`field_type`** on entity fields for serde round-trips; **authoring** new split domains should use **`values:` + `value_ref`**.

**`description` on `values:` rows:** Optional prose for tooling and teaching gloss. The loader maps `DomainNamedValue.description` into [`NamedValueSchema.description`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs). For **entity fields**, if the field slot's `description` is empty, [`field_schema_from_domain_field`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/loader.rs) uses the named value's description as [`FieldSchema.description`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs); a **non-empty** slot `description` overrides. For **parameters**, the same precedence applies via [`input_field_schema_from_domain_parameter`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/loader.rs). Prefer one canonical gloss on the **`values:`** row when a value domain is dedicated to a single slot; use the slot only when you need a one-off override. **Do not** dedupe unrelated primitives into one `values` key just because the wire type matches — conflicting glosses are a sign you split keys incorrectly.

### Entities

An entity is a typed domain object with a primary key, fields, and relations.

```yaml
values:
  <value_key>:
    type: <scalar type>       # same vocabulary as Field Types below
    target: <EntityName>      # when type is entity_ref
    allowed_values: [...]     # select / multi_select (multi_select: non-empty)
    value_format: <scalar or { temporal: ... }>   # required when type is date
    string_semantics: <...>   # on string rows — prompts / summaries
    items:
      value_ref: <element_value_key>   # when type is array

entities:
  <EntityName>:               # PascalCase
    id_field: <field_name>    # logical primary key for refs / expressions; must exist in fields unless id_from is set
    id_from: <path>           # optional — when list/detail JSON rows have no top-level id
    fields:
      <field_name>:
        value_ref: <value_key>
        required: <bool>      # default false
        path: ...             # optional wire path (see below)
        derive: ...           # optional
        description: "..."  # optional
    relations:
      <relation_name>:
        target: <EntityName>  # must be a defined entity
        cardinality: one|many
    domain_projection_examples: false   # optional — default true
    primary_read: <get_capability_id>    # optional — overrides which Get drives projection teaching
```

#### Teaching-table-facing descriptions (entities and capabilities)

Symbolic teaching table / TSV teaching attaches **`entities.<Name>.description`** to the **projection witness** banner line. **`capabilities.<id>.description`** feeds compact capability legends. Both must stay **agentic**: short, imperative, domain-vocabulary — not implementation manuals and not vendor documentation.

**Purpose, not contents:** The type system, relations, **`provides:`**, symbolic **`e#` / `p#`** lines, parameter gloss, and **`discovery:`** already teach **shape**. **`description`** must answer **what this entity is for in agent workflow**: which goal it supports or what class of task it grounds — **without naming relations, fields, or parameters** that already appear on teaching lines.

| Surface | Write | Do **not** write |
|---------|-------|-------------------|
| **Entity `description`** | Role / intent only: what class of task or decision this entity grounds — no relation, field, or parameter names that teaching table already prints | Payload inventories, relation "next step" hints, lists of related entities, REST-ish tours, capability ids, step-by-step APIs, HTTP status codes, `transport:`, explicit MCP seed instructions |
| **Capability `description`** | What this operation **does** or **when** to pick it, in user/domain terms | "Call `foo_query` first", URL paths, error-code trivia (use `discovery.target_terms` for NL hints) |

**`views:` `description`** on a view definition should state **what composed projection** the agent gets — not list inner capability ids.

**Teaching projection (default on):** For each entity with a primary Get and non-empty ordered **`F`** from `CGS::domain_projection_heading_fields` in [`crates/plasm-core/src/schema.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs), the prompt renderer teaches **`F`** on the **projection witness row** — a validated get/query exemplar with trailing `[p#,…]` in `plasm_expr` and `· projection` in Meaning (not a separate entity heading line). Expressions still use `Entity(…)[subset]` for actual reads. **`F`** comes from that Get's explicit **`provides:`** list (order preserved); if `provides` is empty, **`F`** defaults to `id_field` first, then remaining fields lexicographically. Set **`domain_projection_examples: false`** to suppress projection brackets. Optional **`primary_read:`** names the **Get capability id** to override which Get defines **`F`**. Standalone `p#` gloss rows (including alias symbols referenced only in brackets) are emitted before the witness row uses them.

**TSV projection witness (query-only entities):** Symbolic `plasm_expr` / `Meaning` teaching uses `CGS::domain_projection_teaching_wire_fields`, which returns the same **`F`** as the heading when a primary Get exists. If there is no Get, **`F`** still comes from `effective_ordered_response_fields` on a representative read capability: the primary unscoped Query, otherwise the first Query by capability name, then Search the same way.

**`from_parent_get` pitfall:** The JSON path must match the **parent GET response** for that relation. Array-of-ref shapes differ by API (e.g. PokéAPI Pokémon `moves[].move` vs Type `moves[]` as bare `{name,url}`). Copying one entity's `materialize.path` to another without checking the wire JSON yields empty relations at decode time.

**Cardinality `one` + nested child:** When the child ref is not top-level `{relation_name}.name` (e.g. under `meta.ailment` on a move), declare `materialize: { kind: from_parent_get, path: [...] }` on that **one** relation. Only `from_parent_get` is allowed on cardinality `one`; query-scoped materialization remains for **many** relations.

**`id_from` (optional):** sequence of JSON object keys from the row object to a scalar `string` or `number` used as the stable id (e.g. a canonical URL). YAML may be a list `[location_area, url]` or a dotted string `location_area.url`. When `id_from` is present and non-empty, you do not need a `fields` entry named `id_field` solely for decoding.

**Constraints:**

- `id_field` must name a field in `fields`, **or** `id_from` must be a non-empty path
- Every relation `target` must be a defined entity (no dangling refs)
- Entity names are case-sensitive and must be unique

### `path` and `derive` (wire response shaping)

By default, each field is read from a top-level JSON key matching the field name on the decoded row. Override the location with **`path`** on the field slot (next to `value_ref`) in `domain.yaml` (loads as [`FieldSchema.wire_path`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs)): either a dotted string (`owner.login`) or a YAML list of object keys (`[payload, headers]`).

**`derive`** runs on the extracted JSON value **before** optional scalar [`Transform`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-compile/src/decoder.rs) steps. Rules ([`FieldDeriveRule`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs), `type` tag, `snake_case`):

| `type` | Input shape | Behavior |
|--------|-------------|----------|
| `segments_after_prefix` | JSON string | Strip a URL prefix, split on `/`, take `part_index` (GitHub Issue `repository_url` → `owner` / `repo`). |
| `name_value_array_lookup` | JSON **array** of objects | Find the first element where `match_key_field` equals `equals` (defaults: `match_key_field` = `name`, `value_field` = `value`). Optional `case_insensitive` ASCII fold (RFC 5322 header names). Return `value_field` from that object; if no match, field decodes as null. Fits Gmail `payload.headers`, AWS-style `[{ "Key": "…", "Value": "…" }]` tags, etc. |
| `object_key_lookup` | JSON **object** | Return `obj[key]`; optional `case_insensitive` resolution of the key string against object keys. |

**`provides` vs full row decode:** HTTP GET responses are decoded using **all** entity fields that have `path` / `derive` wiring. Capability **`provides`** controls summary-vs-complete detection for list/search ([`CGS::effective_provides`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs)) and teaching projection; it does not strip extra decoded fields from the cached entity row.

**`description` on entities and capabilities:** Optional but recommended when it helps agents. Write **short domain prose** framed for agents choosing tools and traversing the graph, not for humans reading vendor API reference. The same rule applies to `output.description` for `side_effect` actions: state the **domain effect** (e.g. "message moves to Trash"), not the transport shape ("PATCH, empty body", "returns 204"). **Exception:** `auth.token_url` and similar machine OAuth fields may contain a provider token URL.

#### Gloss: do not restate typed structure

**Entity `description`** (projection banner): Same discipline as fields — never use the banner to summarize what's inside the projection (which refs, which booleans), and never repeat relation names already shown as `p#`.

Entity field descriptions (and similar gloss fed from slots) must not inventory shapes the schema already teaches (e.g. "map keyed by …", "JSON containing …", repeating `select` alternatives). Prefer **omitting** the field `description` when the parent entity (or `values:` row) carries enough agent-facing meaning; use one sentence only when the slot needs workflow nuance beyond type (staleness, trust boundary, "refresh before …"). Primitive semantics stay on `values:` rows (`string_semantics`, allowed enums, date meaning).

**Prompt-facing copy (symbolic TSV / MCP teaching table):** Treat `description` on entities, read capabilities (`query` / `get` / `search`), and `values:` slots as **agent selection hints only**. Do not explain list-vs-detail payload shapes, cursor/page mechanics, request-body JSON shapes, "full vs summary" list entries, or `provides:` behavior there. `create` / `update` / `delete` / `action` capability descriptions may stay richer where they disambiguate `m#` choice.

### Field Types

In split `domain.yaml`, the **`type:`** column below is the keyword you put on a **`values:`** row. Entity fields and capability parameters resolve that type via **`value_ref`**. Runtime `FieldType` / operator tables are unchanged.

| Type | YAML value | Typical expression input | Operators | Description |
|------|------------|---------------------------|-----------|-------------|
| String | `string` | string literal / variable | `=`, `!=`, `contains`, `exists` | Free text |
| UUID | `uuid` | string | `=`, `!=`, `contains`, `exists` | Canonical UUID primary keys — wire values are strings; use for stable opaque ids (e.g. Linear `id`). No `string_semantics`. |
| Integer | `integer` | number literal | `=`, `!=`, `>`, `<`, `>=`, `<=`, `exists` | 64-bit integer |
| Number | `number` | number literal | `=`, `!=`, `>`, `<`, `>=`, `<=`, `exists` | Floating point |
| Boolean | `boolean` | `true` / `false` | `=`, `!=`, `exists` | True/false |
| Select | `select` | enum token from `allowed_values` | `=`, `!=`, `in`, `exists` | Single enum. Requires `allowed_values`. |
| MultiSelect | `multi_select` | array of enum tokens | `contains`, `in`, `exists` | Multiple enum. Requires non-empty `allowed_values`. |
| Date | `date` | string or integer per `value_format` | `=`, `!=`, `contains`, `exists` | **Requires `value_format`:** `rfc3339`, `iso8601_date`, `unix_ms`, or `unix_sec`. Predicate inputs are normalized to the wire shape (forgiving parse, UTC). Display of API responses is not rewritten via `value_format`. |
| Array | `array` | array literal / binding | `contains`, `in`, `exists` | Homogeneous list. Requires nested `items:`. |
| EntityRef | `entity_ref` | id value or nested ref expr | `=`, `!=`, `exists` | Foreign key to another entity. Requires `target: EntityName`. |
| **Blob** | **`blob`** | attachment-shaped value / binding | `=`, `!=`, `exists` | Opaque binary or base64-heavy payloads. Do not use `string_semantics`. |

### Blob / binary (`values:` row `type: blob`)

Use **`type: blob`** when the wire value is **not** human prose (base64/base64url, opaque octets, or the reserved attachment object), including:

- Entity fields populated from APIs that return base64 attachment bodies, binary-safe strings, or a JSON object with reserved **`__plasm_attachment`** metadata (`uri`, `mime_type` / `media_type`, optional `bytes_base64`).
- Capability parameters with the same shape (e.g. Gmail `raw`, GitHub Contents `content` as base64 in JSON).

**Do not** use `blob` for HTML/markdown message bodies meant to be read as text (keep `string` + `string_semantics: markdown` or `document`).

**Authoring knobs (entity field slots — alongside `value_ref`):**

| Key | Applies when resolved type is | Notes |
|-----|------------------------------|-------|
| `mime_type_hint` | `string` or `blob` | Hint for MCP/HTTP tabular summaries when the cell is reference-only or split. |
| `attachment_media` | `blob` only | Optional coarse class: `generic`, `image`, `audio`, `video`, `document`. |
| `agent_presentation` | `string` or `blob` | Optional override; `blob` defaults to reference-only summaries when unset. |

**Execute summaries (table / TSV):** for CGS `blob` entity fields, the agent formatter emits two columns, `{field}_ref` and `{field}_mime`, so URI (or `(in artifact)`) and MIME stay split.

**HTTP runtime:** on 2xx responses whose body is not JSON, the default transport may coerce the body into a JSON object `{ "__plasm_attachment": { "bytes_base64": "…", "mime_type": "…" } }` unless the body looks like HTML/XML. Design decoders / `provides` so this shape can land on a `blob` field when APIs return raw octets.

**Fixtures:** see `fixtures/schemas/test_schema.cgs.yaml` entity `BlobAsset` and `fixtures/schemas/capability_with_input/` optional `artifact` field.

### Array element typing (`items:` under `values:`)

Every `values:` row with `type: array` must include `items: { value_ref: <key> }` where `<key>` names another `values:` row for the element shape. Array slots only `value_ref:` the array row.

```yaml
values:
  url_string:
    type: string
    string_semantics: short
  photo_urls:
    type: array
    items:
      value_ref: url_string
  user_ref:
    type: entity_ref
    target: User
  assignee_ids:
    type: array
    items:
      value_ref: user_ref
  flag_enum:
    type: select
    allowed_values: [a, b]
  flags_arr:
    type: array
    items:
      value_ref: flag_enum
  instant_rfc3339:
    type: date
    value_format: rfc3339
  dates_arr:
    type: array
    items:
      value_ref: instant_rfc3339

entities:
  Pet:
    fields:
      photoUrls:
        value_ref: photo_urls
        required: true
```

**Loader constraints:** the element `values:` row must not be `type: array` or `multi_select`. For element `type: select`, `allowed_values` on that row is required and non-empty. For element `type: date`, `value_format` belongs on the element value row.

**`multi_select`:** on the `values:` row itself, `allowed_values` is required and must be non-empty (this is not the same as `array` of `select`).

### Authoring surface: Plasm expressions

Validate catalogs with `plasm-repl`, MCP `execute`, or any host that evaluates Plasm programs against CGS — not by designing command-line flag matrices. Capability `parameters:`, `input_schema`, relations, and `mappings.yaml` define what the compiler and runtime wire to HTTP; teaching table teaches the `e#` / `m#` / `p#` shapes agents actually emit.

`entity_ref` enables forward relation navigation and reverse traversal when query parameters align with FK fields (see [Foreign key fields](#foreign-key-fields-entity_ref)).

### Capabilities

A capability declares an operation available on an entity.

```yaml
capabilities:
  <entity>_<operation>:       # unique name, conventionally entity_verb
    kind: <kind>              # see Capability Kinds below
    entity: <EntityName>      # must be a defined entity
    parameters:               # optional
      - name: <param>
        value_ref: <value_key>
        required: <bool>
        description: <string> # optional
        role: <role>          # optional — see Parameter Roles
```

Wire shape for each parameter is `values[value_ref]`.

**Capability-level `description:`** (the operation, not each parameter): keep short and imperative; see [Teaching-table-facing descriptions](#teaching-table-facing-descriptions-entities-and-capabilities).

**`description` on capability parameters:** Optional. When the prompt uses a symbolic `PromptRenderMode` (compact or tsv, via `--symbol-tuning compact|tsv` on `plasm-mcp` / `plasm-repl` / `plasm-eval`), each parameter gets a `p#` gloss line in teaching table. The gloss shows the parameter type and, after a middle dot, either this `description` or the wire `name`. Use the same style as entity field descriptions: short domain prose. **Do not** restate `name:`, wire type, or enum members.

### Parameter Roles

| `role:` | Semantics | Examples |
|---------|-----------|----------|
| `filter` | Equality/range predicate on entity field values **(default)** | `status`, `archived`, `due_date_gt` |
| `search` | Free-text relevance query — server ranks results | `q`, `query`, `search` |
| `sort` | Sort field selector | `order_by`, `sort_by` |
| `sort_direction` | Ascending/descending companion to `sort` | `sort`, `direction` |
| `response_control` | Payload shape/detail control — does not filter results | `embed`, `fields`, `inc`, `exc` |
| `scope` | Parent-entity pivot wired into the URL path (always `entity_ref`, required) | `team_id`, `space_id` |

`role:` is informational metadata — it does not change how the parameter is transmitted over HTTP. Transmission is controlled entirely by the CML `query:` or `path:` block in mappings.yaml.

### Foreign key fields (`entity_ref`)

Use `entity_ref` when a field stores another entity's primary key. Declare the referenced entity in `target`. The CGS validates that `target` names a defined entity.

For `query` capabilities, if a parameter has the same name as an entity field and both are `entity_ref`, their `target` values must match. That ties the HTTP/query parameter to the domain FK and enables static reverse-traversal lookup: `CGS::find_reverse_traversal_caps("Pet")` returns every query capability whose parameters include `EntityRef(Pet)`.

Example (two-sided pattern):

```yaml
values:
  scalar_i64:
    type: integer
  pet_entity_ref:
    type: entity_ref
    target: Pet

entities:
  Order:
    id_field: id
    fields:
      id:
        value_ref: scalar_i64
        required: true
      petId:
        value_ref: pet_entity_ref
        required: false

capabilities:
  order_findByPetId:
    kind: query
    entity: Order
    parameters:
      - name: petId
        value_ref: pet_entity_ref
        required: true
```

**Self-referential `entity_ref`** (tree hierarchies) is fully supported. The validator only rejects refs to unknown entities; same-entity refs participate in relation navigation like other FKs. Applies to ClickUp `Task.parent → Task`, Jira `Issue.parent_key → Issue`, Linear `Issue.parent → Issue`, Notion `Page.parent_id → Page`, GitHub `Repo.parent_id → Repo`.

#### When to use `entity_ref`

- Any field ending in `_id`, `Id`, `_key`, or whose name matches another entity's `id_field`
- Path parameters that scope a sub-resource (e.g. `team_id` on Space in ClickUp)
- Explicit `$ref` links in the OpenAPI spec
- Parent/scope IDs (`workspace_id`, `database_id`)
- Author/creator/assignee fields storing a User's account ID

**When NOT to use `entity_ref`:**

- Quantities, counts, limits, page sizes — these are `integer`
- IDs that reference entities outside the current CGS scope
- IDs for which the target entity has no `get` capability — deep navigation often requires a `get`

### Capability Kinds

| Kind | Semantics | Typical Plasm role | Requires entity key |
|------|-----------|--------------------|---------------------|
| `query` | Filter/list a collection by field predicates | Query / keyed query rows | No |
| `search` | Full-text relevance search | Search capability surface | No |
| `get` | Fetch single by ID/key | Get by id / compound key | Yes |
| `create` | Create new entity | Create / bind payload | No |
| `update` | Modify existing entity | Update with id + payload | Yes |
| `delete` | Remove entity | Delete by id | Yes |
| `action` | Any other operation | Method / side-effect call | Usually yes |

### Composed read views

**Purpose:** Model a first-class read projection that corresponds to no single upstream REST/GraphQL operation, but does map cleanly onto several existing `query` / `get` capabilities. This belongs in `domain.yaml` as **`views:`** — the same layer as entities and capabilities — not as an undocumented runtime shortcut.

**Authoring rule:** If agents need first-class query/get symbols over a composed DAG, you **must** add `views:` plus capability ids that map with `transport: view`. Use normal `kind: query` and `kind: get` on the same `entity:` when instances are keyed like ordinary resources. Do not substitute long `description:` playbooks alone.

Expose **next hops as relations** (`relation_outputs:` → decoded `Ref` edges on the composed projection), not public `*_id` scalar fields or opaque JSON histogram blobs.

#### CGS: `views:` and synthetic capabilities

- **`views:<key>`** — unique map key per composition.
  - **`description:`** — domain-only prose.
  - **`capability:`** — must equal one `capabilities:` id on `entity` (historically the `kind: query` symbol); additional `get` capabilities may reference the same `view:` key.
  - **`entity:`** — read-model entity whose `fields:` / `relations:` are the agent-facing projection.
  - **`scope:`** — optional list of scope parameters: `name`, optional `value_ref:`, optional **`required: true`** (default false), and optional **`inject:`** (`session_ui_origin` or `session_transport_origin`) so the execute host fills tenant origin from the pinned session backend (agents omit duplicate host strings). Only keys marked required must appear on the outer view invocation; omit optional scope params when unused.
  - **`nodes:`** — ordered steps; each has `id`, `capability` (existing cap id), and `bind:` mapping that capability's parameter names to either:
    - `kind: scope` `param: <name>` — take from the outer view invocation's scope, or
    - `kind: literal` `value: <JSON>` — fixed predicate/env fragment, or
    - **`kind: node_field`** `node:` `field:` — take a field from the **first row** of an earlier node (declaration order = dependency order; forward refs rejected at load), or
    - **`kind: computed`** `template:` — Minijinja string evaluated against outer scope plus prior node first-row fields (same filters as output templates; node ids are also top-level template keys, e.g. `{{ sprint_row.id }}`).
  - **`output:`** — maps entity field names to:
    - `kind: scope` `param:` — copy a scope parameter into the row
    - `kind: node_row_count` `node:` — integer count
    - `kind: node_field` `node:` `field:` — take a field from one row (first row for query nodes)
    - `kind: node_field_histogram_json` — JSON object of distinct values → counts
    - `kind: node_any_row_field_equals` — boolean
    - `kind: node_row_count_positive` — boolean
    - **`kind: computed`** `template:` — Minijinja string evaluated **after** all non-computed `output:` bindings (scope + node bindings) are materialized. The template context includes scope keys and prior output field names. Result is stored as a string field on the composed row. Use for assembled URLs, derived labels, and other strings that are not a single upstream field.
  - **`relation_outputs:`** (optional) — synthesize `CachedEntity.relations` `Ref` targets:
    - `kind: first_node_row_where`
    - `kind: node_rows_where`
    - `kind: node_all_rows`
    - `kind: node_single_row`

Inner nodes may be `query` or `get` capabilities that already have normal CML mappings; the runtime issues HTTP for those only. **`kind: action` is not supported** as a view node today — model explorer-style calls as **`kind: action`** on the target entity (e.g. datasource explorers) or as a standalone capability when a single HTTP op is enough.

#### View computed templates (filters and time)

Computed templates run in `plasm-runtime` with **strict** undefined behavior. Prefer multiline `template: |` blocks in YAML for readability.

Built-in filters (view templates only):

| Filter | Usage | Behavior |
|--------|--------|----------|
| `urlencode` | `{{ s \| urlencode }}` | Form-style percent-encoding |
| `json_encode` | `{{ v \| json_encode }}` | JSON text for a Plasm/JSON value |
| `strip_trailing_slash` | `{{ base \| strip_trailing_slash }}` | Trim trailing `/` |
| `wire_time` | `{{ from \| wire_time('unix_ms') }}` | Pass through `now`, `now-1h`, and all-digit strings unchanged; otherwise normalize via core temporal rules for the named wire format (`unix_ms`, `rfc3339`, …) |
| `wire_query_suffix` | `{{ query_params_json \| wire_query_suffix }}` | Parse a JSON object string; append `&k=v` pairs (empty string when absent/invalid) |

**Temporal:** Predicate slots and `value_ref: temporal` still use `normalize_temporal_value` at plan/compile time. View scope params typed as plain strings (e.g. `nv_grafana_time_range`) should use **`wire_time`** in templates when the wire may be relative (`now-1h`) or already epoch milliseconds.

**Authoring pitfalls:** Do not use `\| default('')` on JSON scope fields you pass to `wire_query_suffix` — use `{% if query_params_json %}…{% endif %}` instead. Choose scope `TAG` names that cannot appear as trimmed lines inside heredoc payloads when binding row templates elsewhere.

Conformance fixture: `fixtures/schemas/plasm_language_matrix_views` (`echo_slug` computed field). Production examples: `apis/cloudflare` `security_surface_status`, `apis/grafana` `views.deeplink_generate.output.url`.

#### CML: `transport: view`

```yaml
my_view_query:
  transport: view
  view: <same_key_as_views_map>

my_view_get:
  transport: view
  view: <same_key_as_views_map>
```

Omit `method`, `path`, `query`, `body`, `pagination`, and `response` on these rows.

#### Parent relations: `get_scoped_bindings` (cardinality **one**)

To hydrate a composed row from a parent entity (e.g. `Zone.security_overview`), declare `cardinality: one` with:

```yaml
materialize:
  kind: get_scoped_bindings
  capability: <named_get_on_child_entity>
  bindings:
    <get_scope_param>: <parent_field_or_id>
```

#### Versioning and auth

- Bump top-level `version:` when adding or changing `views:`, synthetic capabilities, composed entity fields, node/output wiring, or `relation_outputs:`.
- Declare `oauth.requirements.capabilities` for every outward-facing capability id when inner capabilities carry scope requirements.

#### Reference catalog

See `apis/cloudflare/domain.yaml` (`views.security_overview` with computed `security_surface_status`), `apis/grafana/domain.yaml` (`views.deeplink_generate` with computed `url`), and `mappings.yaml` (`transport: view`).

#### Composed views for work tracking and mail

Use this table before adding `views:` to issue trackers (Jira, Linear) or mail (Gmail) catalogs:

| Agent need | Prefer | Example |
|------------|--------|---------|
| List row → detail upgrade | **Hydration** (entity has `query` + `get`) | Gmail `message_list` → `message_get`; Linear `issue_query` → `issue_get` |
| Child collection on parent GET | **`from_parent_get`** or **`query_scoped`** relation | Jira `Issue.comments`; Linear `Issue.labels` |
| Multi-GET aggregate (counts, booleans, status label) | **`views:`** with `node_row_count` / `node_any_row_field_equals` / `kind: computed` | Jira `views.issue_transition_context`; Cloudflare `views.security_overview` |
| Assembled browse / deeplink URL | **`views:`** with **`output.kind: computed`**; scope host with **`inject: session_ui_origin`** when REST/UI share the session origin | Jira `views.issue_browse_link` (`site_base` injected); Linear `views.issue_navigation_link`; Grafana `views.deeplink_generate` |
| Reply / threading context before an action | **`views:`** read model **or** action **`preflight`** | Gmail `message_reply`: `preflight: [{ kind: hydrate_invoke_target, get: message_get, prefix: parent }]` |
| Singleton + one scoped GET | **`views:`** with empty bind + literal bind | Gmail `views.mailbox_snapshot` (`profile_get` + `label_get` id=`INBOX`) |
| Sprint / cycle board (filter inner list from prior node output) | **`views:`** with **`node_field`** / **`computed`** node binds on inner query caps | Jira `views.sprint_board_snapshot` (`issue_jql` JQL from sprint row); Linear `views.cycle_board_snapshot` (`issue_by_cycle_query` cycle from `cycle_get`) |

**Mail-specific:** Header extraction stays on entity fields via **`derive: name_value_array_lookup`** on `Message` — do not duplicate in views unless composing across capabilities.

**Session origin precedence** (browse/deeplink scope injection): `session ui_origin` → `session transport_origin` → `CGS.http_backend` placeholder. Local REPL/CLI `--backend` does not override execute-session injection — set the session backend on `POST /execute` when testing injected scope.

**Issue-tracker-specific:** When the vendor GET already returns a fat graph (Linear `issue_get` GraphQL), views add value for **cross-capability snapshots** (issue + transitions) and **computed URLs**, not for re-fetching the same GET payload.

Conformance fixture rows: `fixtures/schemas/plasm_language_matrix_views` (`lang_triage_context`, `lang_item_link`, `lang_owner_filter_demo`).

### Runtime schema overlay (`schema_overlay:`)

**Use when** the vendor exposes **user-defined columns** (or admin-defined custom fields) that vary by workspace, database, project, list, or sheet — and row payloads are **generic maps** or nested field bags on a stable bootstrap entity. At **execute session open**, the host runs an existing **schema-fetch capability**, projects the JSON into extra entity fields (or per-scope typed entities), and merges the result into the session CGS. **No vendor-specific Rust** — the catalog declares projection rules in YAML.

**Do not use when:**

| Situation | Prefer instead |
|-----------|----------------|
| Fixed OpenAPI / GraphQL schema (GitHub, Linear core fields, Slack, …) | Static `entities:` + `path:` / `derive:` on fields |
| Multi-hop read that does not depend on workspace schema | **`views:`** composed reads |
| Per-contract EVM catalogs | Static **`apis/evm-*`** trees; compile stays in-tree ([`plasm-compile`](https://github.com/PlasmTools/plasm-core/tree/main/crates/plasm-compile)) |
| You can list every column in `domain.yaml` without a runtime schema call | Static fields only |
| Hierarchical field inheritance needs multiple schema sources merged | **`source.steps`** multi-fetch pipeline (list → scoped fetch per row → merge) |

**Decision guide (dynamic schema APIs):**

| Pattern | `projection.mode` | Typical `source` | Field catalog | Example catalogs |
|---------|-------------------|------------------|---------------|------------------|
| One typed row entity per database / table | `per_scope_entity` | Workspace-wide schema list | `from.kind: array` | Fibery (`schema_query`) |
| One typed row entity per container property map | `per_scope_entity` | Search/list databases with embedded schema | `from.kind: object_map` | Notion (`database_search` → `properties`) |
| One typed row entity per project + issue type | `per_scope_entity` + `nested_items_path` | `project_query` → `issue_createmeta_get` per project | `from.kind: object_map` on nested `fields` | Jira |
| Extra columns on one shared entity | `augment_base` | `team_query` → `custom_field_query` per team | `from.kind: array`; `extract: name_value_array` for row values | ClickUp |
| Linear custom fields on issues | `augment_base` *(planned)* | — | — | **Deferred** — public GraphQL schema has no `customFields` query yet |
| Spreadsheet header row as columns | `column_schema` | **Not implemented** — needs row entity model first | — | Google Sheets (future) |

**Bootstrap shape (author first, overlay second):**

1. **Generic row entity** — e.g. Fibery `Record`, Notion `Page`, Jira `Issue` — with `payload` / `properties` JSON or fixed core columns agents can use without overlay.
2. **Schema-fetch capability** — already modeled in `capabilities:` + `mappings.yaml` (`query`, `get`, or `search`).
3. **Scoped read** — query/get whose **scope param** matches `decode.scope.params` (e.g. `database`, `database_id`, composite `project` + `issuetype`).
4. **`schema_overlay:` block** — declarative projection; see spec table below.

**Spec (authoring surface):**

| Key | Role |
|-----|------|
| `source.capability` | Single-step unscoped fetch (Fibery, Notion) |
| `source.steps` | Multi-fetch pipeline: `collect` list step + `for_each` scoped steps with row-driven `bind` |
| `source.steps[].merge` | Accumulate scoped responses (e.g. `append_array` on `fields` / `projects`) |
| `projection.mode` | `per_scope_entity` (default) or `augment_base` |
| `projection.items_path` | JSON path to generator row array |
| `projection.nested_items_path` | Optional; expands each top-level row via nested array — templates get `{ row, parent }` |
| `entity.from_template` | Bootstrap entity to clone |
| `entity.name.template` | Minijinja → Plasm entity name (`per_scope_entity`) |
| `entity.scope_key.template` | Scope index key for decode routing |
| `entity.dynamic_fields.from` | `kind: array` or `kind: object_map` + `path` |
| `entity.dynamic_fields.extract` | `top_level_key`, `path_segments`, or `name_value_array` → `wire_path` + optional `derive:` |
| `decode.scope.params` + `decode.scope.key` | Ambient params → composite lookup key; **empty `params`** allowed when `key` is static (e.g. `global` for workspace-wide `augment_base`) |
| `decode.capabilities` | Capabilities that route decode through overlay entities (list explicitly; empty defaults to Fibery-style `entity_query` / `entity_get` only) |

**Minijinja filters (overlay projector):** `join_sanitize(separator, split_on)`, `sanitize_identifier`.

**Authoring checklist:**

- [ ] Bootstrap entity + schema capability exist and validate **without** overlay
- [ ] Single-step: `source.capability` is `query`, `get`, or `search`; multi-step: list + scoped capabilities declared in `source.steps`
- [ ] Row-driven `bind` templates use **`{{ row.<field> }}`** from prior API responses — never client-supplied overlay scope
- [ ] `type_map` covers vendor field types; `default` value_ref for unknown types
- [ ] `skip.values_in` excludes system columns already on bootstrap entity (ids, summary, …)
- [ ] Matrix fixture under `fixtures/schemas/<name>_schema_overlay/` (JSON sample + bootstrap `domain.yaml`) — **not** `apis/*` in `plasm-core` tests
- [ ] Bump `version:` when changing overlay spec (affects session pin hash / teaching table)

**Reference catalogs:** `apis/fibery/`, `apis/notion/`, `apis/jira/`, `apis/clickup/` (see each README). **Runtime / MCP wiring** (session resolver, TTL cache): monorepo [docs/schema-overlay.md](../reference/schema-overlay.md) when working from the private `plasm` repo.

**Tests:**

```bash
cargo test -p plasm-core schema_overlay
cargo test -p plasm-runtime schema_overlay
```

Fixtures: `fixtures/schemas/fibery_schema_overlay/`, `notion_schema_overlay/`, `jira_schema_overlay/`, `augment_base_overlay/`, `clickup_schema_overlay/`.

### Capability `preflight` (write-time resolution)

Ordered steps on **`create`**, **`update`**, **`action`**, and **`delete`** capabilities run in the runtime **before** CML compile (after path/input env assembly, before `plasm_execute_*` merge). Declared in `domain.yaml` as `preflight:` ( **`invoke_preflight` was removed** — no compat alias).

| Step kind | Purpose |
|-----------|---------|
| `hydrate_invoke_target` | GET `invoke.target`; merge `{prefix}_{field}` (e.g. `parent_threadId`). Skipped on `create`. |
| `hydrate_entity_ref_param` | When param set: GET `entity_ref` row; merge wire keys (`teamId: id`). |
| `query_pick` | Scoped `query`/`search`; exact match on first page; merge wire ids (`stateId`, …). Fail on 0 or >1 matches. |
| `label_ids_delta` | Start from prior hydrate labels; add/remove by name via lookup query; merge `labelIds`. |

**Linear reference:** `issue_create` / `issue_update` use full stack (`team` → `teamId`, `state_name` → `stateId`, etc.). **Gmail:** `message_reply` uses only `hydrate_invoke_target`.

### Action output: `provides:` vs `output.side_effect`

`kind: action` must declare **how the response is modeled**:

1. **Entity projection** — non-empty `provides:` lists which entity fields the HTTP response populates.
2. **No projection** — the call is effectful (something changes) but the response is empty, opaque, or not mapped onto entity fields. Declare `output` with `type: side_effect` and a non-empty `description:` string that states what changes in the domain (not generic "updates resource", not HTTP status or path trivia).

There is **no** `output.type: none` in the schema: it invited silent, incomplete modeling.

```yaml
capabilities:
  message_trash:
    description: Move a message to TRASH
    kind: action
    entity: Message
    output:
      type: side_effect
      description: "Moves the message to Trash; the response carries no fields mapped onto this entity."

  page_get_markdown:
    kind: action
    entity: Page
    provides: [id, markdown, truncated]   # projection path — no side_effect block needed
```

**Validation:** CGS `validate` rejects (a) `action` with neither `provides` nor `output`, and (b) `side_effect` with missing or whitespace-only `description`.

**`query` vs `search`**: Use `query` when the API filters by field equality/range predicates. Use `search` when the primary input is a free-text relevance query and results are ranked, not field-filtered. Search capabilities are excluded from reverse-traversal FK lookups.

### Multiple query capabilities per entity (disambiguation)

An entity can have multiple `kind: query` (or `kind: search`) capabilities. The compiler and planner pick among them using capability identity, parameter shapes, and `role:` metadata.

| Capability shape | Resolution hint |
|------------------|-----------------|
| No required params (or only optional filters) | Often the default list capability for the entity |
| Required params but no `role: scope` | Additional caps need distinct parameter signatures |
| Required `role: scope` param | Scoped list — typically combined with relation `materialize` |

Among non-scoped caps, at most one may be parameterless (validation rule).

### Required Parameters

When a capability declares `required: true` on a parameter, Plasm expressions must supply that predicate key (or the planner rejects). Types must match the `value_ref` slot (`select` values must be members of `allowed_values`, etc.).

```yaml
values:
  pet_status:
    type: select
    allowed_values: [available, pending, sold]

capabilities:
  pet_findByStatus:
    kind: query
    entity: Pet
    parameters:
      - name: status
        value_ref: pet_status
        required: true
```

### Relations and Navigation

Relations declare how to traverse from one entity to related rows. The target entity's query capability parameters supply filters available **after** navigation.

```yaml
values:
  tag_name:
    type: string
    string_semantics: short

entities:
  Pet:
    relations:
      tags:
        target: Tag
        cardinality: many
  Tag:
    fields:
      name:
        value_ref: tag_name
```

REPL-style navigation (exact surface comes from teaching table for your catalog):

```
Pet(<id>).tags
Pet(<id>).tags{name=…}    # when filters are taught for the target query
```

#### Scoped many-relations — `materialize: query_scoped` / `query_scoped_bindings`

When a REST API uses a sub-resource URL pattern (`/parent/{parent_id}/children`) or a scoped list query, declare `materialize` on the many relation so chain traversal fills the target capability parameters from the parent row.

**Single scope parameter** (`query_scoped`) — `capability` names the exact target `query` / `search`; `param` is its scope field; the value comes from the parent entity's `id_field`:

```yaml
entities:
  Page:
    relations:
      blocks:
        target: Block
        cardinality: many
        materialize:
          kind: query_scoped
          capability: block_children_query
          param: block_id
```

**Multiple scope parameters** (`query_scoped_bindings`) — same required `capability`, plus map each target capability parameter name to a parent entity field name:

```yaml
entities:
  Calendar:
    relations:
      events:
        target: Event
        cardinality: many
        materialize:
          kind: query_scoped_bindings
          capability: event_list
          bindings:
            calendarId: id
```

**Scoped traversal:** parent id / scope fields fill the target capability's scope parameters automatically during relation chain execution.

**Multiline / structured string values** in predicates and method arguments use a bash-inspired tagged `<<TAG` heredoc: `<<TAG\n` … `\nTAG\n` with `TAG` alone on a closing line (trimmed), or `TAG)` / `TAG,` / `TAG}` glued on that line.

**Compound `entity_ref` scope parameters** (one param that unpacks to several path/query slots, e.g. repository identity) use runtime scope splat and optional `scope_aggregate_key_policy` on the capability — distinct from `query_scoped_bindings`.

#### Multiple projections of the same entity — `provides:` and auto-resolution

When multiple API endpoints return disjoint field subsets of the same logical resource (same `id`, different fields), model them as one entity with `required: false` on projection-only fields. Declare `provides:` on each capability to enable auto-resolution.

```yaml
values:
  nv_page_url:
    type: string
    string_semantics: short
  nv_page_created_time:
    type: date
    value_format: rfc3339
  nv_page_in_trash:
    type: boolean
  nv_page_markdown:
    type: string
    string_semantics: markdown
  nv_page_truncated:
    type: boolean

entities:
  Page:
    id_field: id
    fields:
      url:
        value_ref: nv_page_url
      created_time:
        value_ref: nv_page_created_time
      in_trash:
        value_ref: nv_page_in_trash
      markdown:
        value_ref: nv_page_markdown
        required: false
      truncated:
        value_ref: nv_page_truncated
        required: false

capabilities:
  page_get:
    kind: get
    entity: Page
    provides: [id, url, public_url, created_time, last_edited_time, in_trash, archived]

  page_get_markdown:
    kind: action
    entity: Page
    provides: [id, markdown, truncated]
```

**`provides:`** declares which entity fields a capability populates in its response. The runtime builds a reverse index (`field → capability`) and uses it to auto-invoke the correct capability when a projected field is absent from cache.

**Auto-resolution** in action:

```
plasm> Page("abc")[markdown]
# "markdown" absent from cache → auto-invokes page_get_markdown("abc")
# additive merge → Page:abc now has markdown + all metadata fields
{"markdown": "# Full page content..."}
```

**`provides:` defaults** when omitted (backward-compatible):

- `get` / `query` / `search` → provides all entity fields (optimistic)
- `create` / `update` / `delete` / `action` → provides nothing (declare explicitly)

**Recommendation for `kind: get`:** Declare an explicit ordered `provides:` listing every scalar field the detail response materializes, with `id_field` first.

For `action`, if you rely on the default empty `provides`, you **must** add `output: { type: side_effect, description: "…" }`.

**Three-way capability contract** — full field-level provenance:

| Annotation | Direction | Meaning |
|------------|-----------|---------|
| `parameters:` | input | What the API endpoint accepts |
| `provides:` | output | Which entity fields the response populates |
| `mutates:` | write set | Which entity fields this capability changes *(roadmap)* |

**Recognition**: path `/resource/{id}` + `/resource/{id}/suffix`; both return same `id`; disjoint fields.

---

## CML (Capability Mapping Language) — mappings.yaml

CML defines how each capability translates to an HTTP request (or GraphQL over HTTP when `transport: graphql`). It is a declarative template language — no loops; conditionals are `if` with `exists`, `equals`, or `bool` conditions, total evaluation.

### Structure

Each capability name from domain.yaml gets one entry — either a normal HTTP/GraphQL template or a view stub (see [Composed read views](#composed-read-views)):

```yaml
<capability_name>:
  method: GET|POST|PUT|PATCH|DELETE
  path: <path_segments>
  query: <cml_expr>       # optional
  body: <cml_expr>        # optional
  headers: <cml_expr>     # optional
  pagination: <pagination_block>   # optional; query capabilities only
```

**View-backed query:**

```yaml
<capability_name>:
  transport: view
  view: <views_map_key>
```

### Pagination (CML) — mappings.yaml only

Pagination is transparent in the domain model: `domain.yaml` still uses `kind: query` for list capabilities. HTTP pagination is declared only in CML.

When a mapping includes `pagination`, the runtime merges page parameters from `pagination.params` (counter / fixed / `from_response` keys and `location`) for follow-up HTTP requests.

**LLM / MCP execute:** paginated queries return one upstream page by default. When more pages exist, the host mints an opaque session handle (`pg1`, `pg2`, …) and surfaces `has_more` plus a compact `page(pgN)` follow-up. Clients continue with `page(pgN)` or `page(pgN, limit=50)`.

**`plasm-repl` / expressions:** use postfix limits / continuation forms taught in teaching table, or session `page(...)` — not synthetic `--limit` / `--all`.

Default without an explicit continuation: first page only.

#### Pagination block schema

Rust ground truth: [`PaginationConfig`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-cml/src/cml.rs) in `mappings.yaml` under `pagination:`.

```yaml
pagination:
  location: query            # query | body | link_header | response_next_url | block_range
  response_next_url_field: "@odata.nextLink"   # optional; when location: response_next_url
  body_merge_path: [variables, o, paginate]   # optional; when location: body
  response_prefix: [data, issues, pageInfo]   # optional; scope for stop_when / from_response
  stop_when:
    field: hasNextPage
    eq: false
  params:
    offset:
      counter: 0
      step: 20
    limit:
      fixed: 20
    after:
      from_response: endCursor
```

Decode shape for list bodies remains on the mapping's `response:` / decoder.

#### `location` (summary)

| `location` | Role |
|------------|------|
| `query` (default) | Merge `params` into the query string. |
| `body` | Merge `params` under `body_merge_path` (or top-level JSON body). |
| `link_header` | Next page from `Link: …; rel="next"` (Live mode; replay caveats). |
| `response_next_url` | Next page from an absolute URL string in the JSON body (e.g. Graph `@odata.nextLink`). First-page `params` still apply; continuation fetches the stored URL as-is (Live mode; replay caveats). |
| `block_range` | EVM log ranges (`from_block` / `to_block`). |

#### Inference heuristics (LLM / authoring)

| OpenAPI / response signal | Likely `pagination.params` / `location` shape |
|---------------------------|-----------------------------------------------|
| Query params `offset` + `limit` | Counters + fixed limit, `location: query` |
| Query param `page` (no `offset`) | `page` counter + optional `per_page` / `size` fixed |
| Params `cursor`, `start_cursor`, `after` | `from_response` continuation fields |
| Params `starting_after` / `ending_before` | Keyset-style `after` / `before` params |
| Schema `Paginated*` with `count`, `next`, `previous`, `results` | Offset/page + `response_prefix` if nested |
| `has_more` + `data` | `stop_when` + `from_response` on nested `pageInfo` |
| `next_cursor` + `results` | Cursor param + `from_response` |
| `@odata.nextLink` + `value` | `location: response_next_url`, optional `response_next_url_field`, `$top` / `$select` as first-page `params` |
| No list pagination parameters | omit `pagination` |

#### GraphQL (`transport: graphql`)

GraphQL list capabilities use the same composable `pagination:` shape as HTTP (see `apis/graphqlzero`, `apis/linear`):

- **`location`**: typically `body` with variables merged under `body_merge_path` (e.g. `[variables]` or `[variables, o, paginate]`).
- **`params`**: maps keys merged at that path — e.g. Relay `first` / `after` with `{ from_response: endCursor }`.
- **`response_prefix`**: optional path from the root JSON response (e.g. `[data, issues, pageInfo]`).

**CML `object` fields: `Value::Null` keys are omitted at eval time.** In [`eval_cml`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-cml/src/cml.rs), when building a `type: object`, any key whose sub-expression evaluates to `Value::Null` is not inserted into the parent object. So the common optional pattern `type: if` / `condition: exists` / `else_expr: { type: const, value: null }` produces no key for missing inputs — well-typed omit semantics, not only on the wire.

**HTTP JSON body: null keys are still stripped before POST** (`strip_null_fields` in [`crates/plasm-runtime/src/http_transport.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-runtime/src/http_transport.rs)) as a safety net for any remaining `null`.

**Explicit JSON `null` to clear a field:** A key whose value must be a literal `null` in JSON is not representable if the only way to express it is `Value::Null` inside a CML object (it will be omitted). A future extension could add a dedicated CML/`Value` form for explicit null.

### Query result hydration (runtime)

This is **not** part of CML or `domain.yaml`. After a query succeeds, if the CGS defines a `get` capability on the same entity, the runtime defaults to:

1. Merging decoded list rows into `GraphCache` as `completeness: summary`.
2. For each returned `Ref`, issuing the `get` mapping (concurrent, up to `ExecutionConfig::hydrate_concurrency`, default 5) unless the cache already holds `complete` for that ref.
3. Merging GET responses as `complete` and returning entities in query result order.

**Opt out (list-shaped output only):**

- Host / IR: `QueryExpr.hydrate = Some(false)` for one query, or `ExecutionConfig.hydrate = false` for the whole engine.

**When hydration does not run:** the entity has query but no get mapping.

**Interaction with pagination:** pagination collects the ordered list of refs first; hydration runs after pages are merged.

**Cache semantics:** `CachedEntity.completeness` is `summary` or `complete`. Merge never overwrites `complete` with `summary`. `execute_get` returns a cache hit only for `complete` rows.

### Path Segments

An ordered list of literal strings and variable references:

```yaml
path:
  - type: literal
    value: pet                # → /pet
  - type: var
    name: id                  # → /pet/{id}
  - type: literal
    value: uploadImage        # → /pet/{id}/uploadImage
```

### CML Expressions

#### Variable reference

```yaml
type: var
name: <variable_name>
```

#### Constant

```yaml
type: const
value: <any_value>
```

#### Object (key-value pairs)

```yaml
type: object
fields:
  - - key_name
    - type: var
      name: value_var
  - - another_key
    - type: const
      value: fixed_value
```

#### Conditional (`if`)

```yaml
type: if
condition:
  type: exists
  var: <variable_name>
then_expr: <cml_expr>
else_expr: <cml_expr>
```

**Conditions** (`CmlCond` in `plasm-cml`): `exists` (variable bound), `equals` (compare two expressions), `bool` (truthy eval). Prefer `exists` for optional query params.

#### Array join (CSV / pipe serialisation)

```yaml
type: join
sep: ","          # separator (use "|" for pipe-delimited)
expr:
  type: var
  name: genres    # must resolve to Value::Array
```

In the `query:` block:

```yaml
# Emits ?genres=1,2,3 (CSV)
query:
  type: object
  fields:
    - - genres
      - type: join
        sep: ","
        expr: { type: var, name: genres }

# Emits ?ids=1|2|3 (pipe)
    - - ids
      - type: join
        sep: "|"
        expr: { type: var, name: ids }
```

**Repeated-key arrays** (`?embed=a&embed=b`): Use a plain `var` without `join`. The HTTP execution layer automatically expands `Value::Array` query param values into repeated `key=value` pairs:

```yaml
# Emits ?embed=cast&embed=episodes
    - - embed
      - type: var
        name: embed
```

### Variable Resolution

The execution engine populates the CML environment before template evaluation:

| Operation | Variables set |
|-----------|---------------|
| **Query** | `filter` (compiled BackendFilter), each predicate field=value pair, `projection` |
| **Get** | `id`, plus all path var names from the CML template set to the ID value |
| **Create** | `input` (Value::Object from compiled create/update/action expressions) |
| **Delete** | `id`, plus all path var names |
| **Update/Action** | `id`, path var names, `input` |

If the spec uses `{petId}` in the path, the CML template should use `name: id` (normalized) OR `name: petId` (the engine sets both).

### Compilation: CML → HTTP Request

```
CML template + environment variables
    ↓ eval_path_segment() per segment
    → URL path string
    ↓ eval_cml() on query expr
    → URL query parameters
    ↓ eval_cml() on body expr (when body_format is json or form_urlencoded)
    → JSON or scalar-map request body
    ↓ eval_cml() on each multipart.parts[].content (when body_format is multipart)
    → compiled multipart parts (null parts omitted)
    ↓ assemble
    → CompiledRequest { method, path, query, body, body_format, multipart, headers }
```

The compiled request is deterministic: same template + same env = same HTTP request. This enables blake3 fingerprinting for record/replay.

### Examples

**Full mapping:**

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
```

For predicate `status = "available"`: env `{status: "available"}` → path `/pet/findByStatus` → query `?status=available` → `GET /pet/findByStatus?status=available`.

**Path variable:**

```yaml
pet_delete:
  method: DELETE
  path:
    - type: literal
      value: pet
    - type: var
      name: id
```

For Pet id `"10"`: env `{id: "10", petId: "10"}` → path `/pet/10` → `DELETE /pet/10`.

**Request body:**

```yaml
pet_create:
  method: POST
  path:
    - type: literal
      value: pet
  body:
    type: var
    name: input
```

For input `{name: "Fido", status: "available"}`: env `{input: {...}}` → `POST /pet` with that JSON.

**Do not combine `body: { type: var, name: input }` with a scalar parameter also named `input`.** On create/invoke, the runtime binds the full param object to `env["input"]`, then splats each param key into the env — including `input` again — so `env["input"]` becomes the scalar field value, not the aggregate object. CML `body: { type: var, name: input }` then serializes a JSON string/number instead of `{ "input": "…", … }`.

Use an explicit object body with named fields instead (same pattern as optional-param null stripping elsewhere):

```yaml
research_create:
  method: POST
  path:
    - type: literal
      value: research
  body:
    type: object
    fields:
      - [input, { type: var, name: input }]
      - [model, { type: if, condition: { type: exists, var: model },
                  then_expr: { type: var, name: model },
                  else_expr: { type: const, value: null } }]
```

Reserve `body: { type: var, name: input }` for capabilities whose **`input` parameter is the entire JSON body** (`values:` row with `type: json`, or inline `input_type: object` with nested payload) and whose param names do not collide with aggregate keys after splat. `schema validate` rejects scalar `input` params on this mapping shape ([`BodyVarInputParamCollision`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/error.rs)).

### Request body formats (`body_format`)

Default is `json`: `body:` is evaluated to a Plasm `Value` and POSTed as `application/json` (nulls stripped on the wire).

**`form_urlencoded`:** `body:` must evaluate to a flat object of string/number/bool fields; the runtime sends `application/x-www-form-urlencoded`.

**`multipart`:** do not set `body:`. Instead set `multipart:` with a `parts:` list. Each part has:

- `name`: form field name (required).
- `file_name`: optional `Content-Disposition` filename.
- `content_type`: optional MIME for the part.
- `content`: a CML expression. If it evaluates to null, the part is omitted.

**File bytes:** evaluate `content` to an attachment-shaped JSON object with reserved `__plasm_attachment` and non-empty `bytes_base64`. URI-only attachments are rejected for outbound multipart.

Example (OpenAPI-style upload + optional string field):

```yaml
body_format: multipart
multipart:
  parts:
    - name: additionalMetadata
      content:
        type: if
        condition: { type: exists, var: additionalMetadata }
        then_expr: { type: var, name: additionalMetadata }
        else_expr: { type: const, value: null }
    - name: file
      file_name: upload.png
      content:
        type: var
        name: file
```

Rust ground truth: [`HttpBodyFormat`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-cml/src/cml.rs), [`MultipartBodySpec`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-cml/src/cml.rs), wire build in [`http_transport.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-runtime/src/http_transport.rs).

---

## Authentication

Declare authentication once at the top level of `domain.yaml` under the `auth:` key. For public HTTP APIs (no outbound credentials), use `scheme: none` so tooling can tell intentional "no auth" from a missing block. Omitting `auth` entirely is accepted for backward compatibility but is ambiguous. Credential-bearing schemes read secrets at request time from environment variables or hosted KV via `SecretProvider`. No secrets are stored in schema files.

**Constraint:** `auth: { scheme: none }` cannot be combined with a top-level `oauth:` block.

**Connect UX (`oauth_bearer` vs `bearer_token`):** Both inject `Authorization: Bearer …` at runtime. Use **`oauth_bearer`** with an `oauth:` block when operators connect **only via OAuth** (Google Workspace, LinkedIn, Reddit, X, etc.) — UIs show OAuth only. Use **`bearer_token`** with `oauth:` when operators may **also paste a PAT / API token** (GitHub, Cloudflare, Slack, Figma).

### Supported schemes

| Scheme | YAML `scheme:` value | Injected as | Env var fields |
|--------|----------------------|-------------|----------------|
| No outbound credentials (public API) | `none` | *(nothing)* | — |
| Static API key in a header | `api_key_header` | `<header>: <value>` | `header`, `env` |
| Static API key in query param | `api_key_query` | `?<param>=<value>` | `param`, `env` |
| Bearer token (operator PAT / API token) | `bearer_token` | `Authorization: Bearer <token>` | `env`, `hosted_kv` |
| OAuth access-token injection | `oauth_bearer` | `Authorization: Bearer <token>` (from OAuth link / hosted KV envelope) | `env`, `hosted_kv` |
| OAuth 2.0 client credentials | `oauth2_client_credentials` | `Authorization: Bearer <token>` (token cached + auto-refreshed) | `token_url`, `client_id_env`, `client_secret_env`, `scopes` (optional) |

### Examples

```yaml
# Public / open HTTP API (e.g. PokéAPI, D&D 5e)
auth:
  scheme: none

# API key sent as a query parameter
auth:
  scheme: api_key_query
  param: key
  env: RAWG_API_KEY

# Bearer token
auth:
  scheme: bearer_token
  env: CLICKUP_API_TOKEN

# Static API key in a named header
auth:
  scheme: api_key_header
  header: X-Api-Key
  env: MY_SERVICE_API_KEY

# OAuth 2.0 client credentials
auth:
  scheme: oauth2_client_credentials
  token_url: https://accounts.spotify.com/api/token
  client_id_env: SPOTIFY_CLIENT_ID
  client_secret_env: SPOTIFY_CLIENT_SECRET
  scopes:
    - user-read-private
```

### How auth injection works

Auth is injected **before** CML-declared `headers:` so per-capability mappings can override credentials if ever needed. Pagination continuation requests (Link header follow-ups) receive the same credentials automatically.

For `oauth2_client_credentials`, the runtime:

1. Checks a per-`AuthResolver` in-memory cache (`tokio::sync::RwLock<Option<CachedToken>>`).
2. If the cached token is still valid (30-second safety margin), uses it directly.
3. Otherwise exchanges `client_id` + `client_secret` for a fresh token via `POST token_url`, caches it, then proceeds.

### Runtime extension

The `SecretProvider` trait in `plasm-runtime::auth` is `dyn`-compatible. To use a secret store other than env vars, implement `SecretProvider` and pass it to `AuthResolver::new(scheme, Arc::new(my_provider))`.

---

## Execution Pipeline

```
Plasm program / expression (parse + recover)
  → build Expr (Query/Get/Create/Delete/Invoke)
  → type_check_expr validates against CGS
  → normalize predicate (flatten, DeMorgan, dedup)
  → compile predicate to BackendFilter
  → populate CML environment
  → eval CML template → CompiledRequest
  → execute HTTP (live/replay/hybrid)
  → normalize response (bare array → {results: [...]})
  → decode response via schema-driven decoder (fields from CGS entity)
  → merge decoded entities into graph cache (stable Ref identity)
  → after **query**, optional concurrent **GET** per row when entity has **get** (unless `QueryExpr.hydrate == Some(false)` / engine hydrate off)
  → format output (json/table/compact)
```

Per compiled capability, the same CGS + CML + input yields the same primary HTTP request (fingerprint-based replay). Pagination and hydration add further requests whose count depends on result size, cache state, and execution options — each follow-up request is still compiled and replayed like any other GET.
