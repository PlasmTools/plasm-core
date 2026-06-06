# Fibery API — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [Fibery HTTP API](https://developers.fibery.com/guides/http-api/overview). The catalog is **task-oriented**: agents work with spaces, databases, rows, documents, views, webhooks, and files — not raw command names.

```bash
export FIBERY_API_TOKEN="Token YOUR_TOKEN"
export FIBERY_HTTP_BACKEND="https://YOUR_ACCOUNT.fibery.io"
cargo run -p plasm -- \
  --schema apis/fibery \
  --backend "$FIBERY_HTTP_BACKEND" \
  --repl
```

On hosted `plasm-mcp`, set the same env vars on the agent process (or connect via the web UI with workspace URL + API key so both are stored in outbound KV).

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
  env: FIBERY_API_TOKEN
```

The token is sent verbatim in `Authorization` (include the `Token …` prefix in the env value).

### Backend

`http_backend: https://YOUR_ACCOUNT.fibery.io` — account-specific host. Override with `--backend` on `plasm` / `plasm-mcp`.

### Schema overlay

At execute session open, when **`schema_overlay:`** is present, the host executes **`source.capability`** (`schema_query`), projects rows via Minijinja (see `domain.yaml`), and merges **per-database typed entities** (e.g. `Cricket__Player`) into the bootstrap CGS. Session pin hash includes the overlay digest via `effective_catalog_cgs_hash_hex`.

Bootstrap entities remain stable for discovery and generic row CRUD:

| Entity | Role |
|--------|------|
| `Record` | Generic row (`database` + `id` key); use for create/update/query when the overlay entity is not needed |
| `Database` | Schema type metadata from `schema_query` |
| Overlay entities | One Plasm entity per Fibery database, fields from live schema |

Agents should call **`schema_query`** first (or rely on overlay merge after session open) to learn database names (`Space/Name`).

### Command API vs REST

| Area | Transport |
|------|-----------|
| Entity CRUD, collections, schema batch | `POST /api/commands` |
| Documents | `GET/PUT /api/documents/{secret}`, `POST /api/documents/commands` |
| Views | `POST /api/views/json-rpc` |
| Webhooks | `GET/POST/DELETE /api/webhooks/v2` |
| Files | `POST /api/files/from-url`, `GET /api/files/{secret}`, `POST /api/files/sign-urls` |
| Search | `POST /api/search/v2` |
| History | `POST /api/history/v2/search` |

### Views (composed reads)

| View | Purpose |
|------|---------|
| `database_context` | Field schema + sample rows for one database |
| `entity_with_document` | Row metadata plus `document_secret` for follow-up `document_get` |

### Coverage gaps

| Gap | Notes |
|-----|-------|
| **Multipart file upload** | `POST /api/files` (local multipart) is **not** mapped; use `file_upload_from_url` or upload outside Plasm |
| **GraphQL** | Per-space GraphQL (`/api/graphql/space/…`) omitted; use `entity_query` command DSL |
| **OAuth** | Static API token only; no OAuth2 user-delegation flow in this catalog |
| **BM-25 search** | `entity_search` maps to `/api/search/v2`; confirm availability on your workspace tier |
| **Rich text on create** | Fibery cannot set document fields at entity create; use `document_set` after `entity_create` |

---

## Verification

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/fibery
cargo run -p plasm-eval -- coverage --schema apis/fibery --cases apis/fibery/eval/cases.yaml
```

Live calls require a real token and a Fibery account host.
