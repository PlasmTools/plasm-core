# Fibery API — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [Fibery HTTP API](https://developers.fibery.com/guides/http-api/overview). The catalog is **task-oriented**: agents work with spaces, databases, rows, documents, views, webhooks, and files — not raw command names.

**Surface:** 14 bootstrap entities, 44 capabilities, 2 composed read views, runtime **schema overlay** (per-database typed entities after session open).

```bash
cargo run -p plasm -- \
  --schema apis/fibery \
  --backend "https://YOUR_ACCOUNT.fibery.io" \
  --repl
```

On hosted `plasm-mcp` or the OSS appliance, connect **workspace URL + API key** in the MCP / control-station UI (bindings are stored in encrypted `plasm:binding:v1:*` KV; credentials in `plasm:outbound:v1:*`).

Replace `YOUR_ACCOUNT` with your Fibery workspace subdomain. Generate an API token from the workspace menu (**API Tokens**). Fibery expects the token value to include the `Token ` prefix when sent as `Authorization`.

---

## What the CGS design is

**`domain.yaml`** declares entities, relations, capabilities, composed **`views:`**, and a declarative **`schema_overlay:`** block. **`mappings.yaml`** compiles capabilities to HTTP/CML (commands API, documents, views JSON-RPC, webhooks REST, files REST, history/search REST).

See [docs/schema-overlay.md](../../../docs/schema-overlay.md) for the generic overlay mechanism.

### Auth

```yaml
auth:
  scheme: api_key_header
  header: Authorization
  hosted_kv: plasm:outbound:v1:catalog:fibery
```

Connect via MCP UI (workspace URL binding + API key secret). Local REPL: pass `--backend https://your-account.fibery.io`.

### Backend

`http_backend: https://YOUR_ACCOUNT.fibery.io` — account-specific host. Override with `--backend` on `plasm` / `plasm-mcp`.

### Response envelope (`/api/commands`)

Every command POST returns:

```json
{ "success": true, "result": … }
```

Plasm CML must **narrow** `result` before entity decode (there is no OpenAPI spec — validate with fixtures + live smoke):

| Fibery shape | Example commands | CML pattern |
|--------------|------------------|-------------|
| `result` is an **array** | `fibery.entity/query` (get-me, entity_get) | `items_path: [result, "0"]` + `single: true` |
| `result` is an **object** | `fibery.entity/create`, `fibery.entity/update`, schema batch creates | `items: result` + `single: true` |
| `result` is `"ok"` | collection add/remove, many side effects | root `response: single` or no entity decode |
| List in `result` | `fibery.entity/query` (lists), `fibery.entity/batch/…` | `items: result` (no `single`) |

**Do not** use root `response: single` for command responses that return `{ success, result: {…} }` — decode would target the envelope, not the row.

REST paths outside `/api/commands` use their own shapes (`/api/search/v2` → `items`, `/api/webhooks/v2`, `/api/documents/{secret}` raw body, JSON-RPC `result` for views).

### Query DSL and `input` passthrough

- **`user_get_me`** uses Fibery’s `$my-id` in `q/where` (resolved from the API token — do not remove).
- **`entity_get`** binds `$entity-id` via `params` from the Plasm `id` argument.
- Default **`entity_query`** / **`entity_get`** `q/select` is only `fibery/id` + `fibery/public-id`. For full rows or custom filters, pass capability parameter **`input`** with a complete Fibery query object (`args` passthrough).

### Schema overlay and discovery

At execute session open, when **`schema_overlay:`** is present, the host runs **`schema_query`**, projects rows, and merges **per-database typed entities** (e.g. `Cricket__Player`) with **`expression_aliases`** like `Cricket/Player`. Session pin hash includes the overlay digest via `effective_catalog_cgs_hash_hex`.

| Surface | What agents see |
|---------|-----------------|
| **`discover_capabilities`** | Bootstrap entities only (`Database`, `Record`, `User`, `DatabaseContext`, …) |
| **After `plasm_context` / session open** | Overlay typed entities merged into the session CGS |
| **Programs** | Generic `Record(database=…)` **or** typed entity / alias after overlay merge |

Agents should seed **`Database`** or **`Record`**, call **`schema_query`** in-program, or rely on overlay merge at session open to learn database names (`Space/Name`).

### Command API vs REST

| Area | Transport |
|------|-----------|
| Entity CRUD, collections, schema batch | `POST /api/commands` |
| Documents | `GET/PUT /api/documents/{secret}`, `POST /api/documents/commands` |
| Fibery UI views | `POST /api/views/json-rpc` |
| Webhooks | `GET/POST/DELETE /api/webhooks/v2` |
| Files | `POST /api/files/from-url`, `GET /api/files/{secret}`, `POST /api/files/sign-urls` |
| Search | `POST /api/search/v2` |
| History | `POST /api/history/v2/search` |

### Views (composed reads vs Fibery `View` entity)

| Plasm composed view | Purpose |
|---------------------|---------|
| `database_context` | Field schema + sample rows for one database (`DatabaseContext` entity) |
| `entity_with_document` | Row metadata + `document_secret` for follow-up `document_get` |

These are **not** the same as the bootstrap **`View`** entity (saved Fibery board/grid/timeline views via JSON-RPC).

### Incremental teaching waves (MCP / HTTP execute)

1. **Wave 1:** seed `{ api: fibery, entity: Database }` and/or `Record` for schema + row CRUD.
2. **Wave 2:** seed `DatabaseContext` when the task needs field schema + sample rows (eval **fb-09**).
3. **Overlay-typed entities** appear in the session CGS after open; expand seeds or use `Record` + `database` scope unless you need typed column symbols.

See [incremental-teaching-prompts.md](../../../docs/incremental-teaching-prompts.md).

### Coverage gaps

| Gap | Notes |
|-----|-------|
| **Multipart file upload** | `POST /api/files` (local multipart) is **not** mapped; use `file_upload_from_url` or upload outside Plasm |
| **GraphQL** | Per-space GraphQL omitted; use `entity_query` command DSL |
| **OAuth** | Static API token only |
| **BM-25 search** | `entity_search` maps to `/api/search/v2`; confirm on your workspace tier |
| **Rich text on create** | Use `document_set` after `entity_create` |
| **Eval database names** | Cases fb-03…fb-10 reference example workspaces (`Cricket/Player`, `CRM/Lead`, …) — substitute names from your live schema |

---

## curl smoke tests

Replace host and token:

```bash
export FIBERY_HOST=https://YOUR_ACCOUNT.fibery.io
export FIBERY_TOKEN='Token YOUR_API_TOKEN'

# Schema types (overlay source)
curl -sS -X POST "$FIBERY_HOST/api/commands" \
  -H "Authorization: $FIBERY_TOKEN" -H 'Content-Type: application/json' \
  -d '{"command":"fibery.schema/query","args":{"with-description?":false}}' | jq '.success, (.result|length)'

# Current user (query array in result)
curl -sS -X POST "$FIBERY_HOST/api/commands" \
  -H "Authorization: $FIBERY_TOKEN" -H 'Content-Type: application/json' \
  -d '{"command":"fibery.entity/query","args":{"query":{"q/from":"fibery/user","q/select":["fibery/id","user/name","user/email"],"q/where":["=",["fibery/id"],"$my-id"],"q/limit":1}}}' | jq .

# List rows (minimal select)
curl -sS -X POST "$FIBERY_HOST/api/commands" \
  -H "Authorization: $FIBERY_TOKEN" -H 'Content-Type: application/json' \
  -d '{"command":"fibery.entity/query","args":{"query":{"q/from":"YOUR_SPACE/Database","q/select":["fibery/id"],"q/limit":5}}}' | jq .
```

---

## Eval cases

```bash
cargo run -p plasm-eval -- coverage --schema apis/fibery --cases apis/fibery/eval/cases.yaml
```

| Id | Goal (summary) |
|----|----------------|
| fb-01 | List workspace databases (`Database`) |
| fb-02 | Authenticated user profile (`User` / `user_get_me`) |
| fb-03 | Field defs for one database (`Field`) |
| fb-04 | Query rows with limit (`Record`) |
| fb-05 | Get one row by id (`Record`) |
| fb-06 | Workspace search (`SearchResult`) |
| fb-07 | History for a database (`HistoryEvent`) |
| fb-08 | Webhooks list (`Webhook`) |
| fb-09 | Database context view (`DatabaseContext`) |
| fb-10 | Create row (`Record`) |

---

## Verification

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/fibery
cargo test -p plasm-runtime fibery_
```

Live calls require a real token and a Fibery account host.
