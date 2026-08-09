# Notion API — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [Notion REST API](https://developers.notion.com/). This CGS focuses on **pages, databases, users, and comments**, with **Enhanced Markdown** for read/write content. The block-by-block API is intentionally **not** mapped—use `page_get_markdown` / `page_update_markdown` instead.

```bash
export NOTION_API_TOKEN=secret_...
cargo run -p plasm-repl -- \
  --schema apis/notion \
  --backend https://api.notion.com \
  --repl
```

Every mapping sends `Notion-Version: 2026-03-11` (see `mappings.yaml`). Create a [Notion integration](https://www.notion.so/my-integrations), share target pages/databases with it, and use the integration’s secret as `NOTION_API_TOKEN`.

---

## What the CGS design is

A CGS describes **business objects and operations**, not raw endpoints. **`domain.yaml`** declares entities, fields, relations, and capability kinds (`get`, `query`, `search`, `create`, `update`, `action`). **`mappings.yaml`** compiles each capability to HTTP with CML (paths, query, body, headers, pagination).

### Auth

Bearer token from the environment. The schema declares:

```yaml
auth:
  scheme: bearer_token
  env: NOTION_API_TOKEN
```

Use **`NOTION_API_TOKEN`**, not a generic `NOTION_TOKEN`—that is what `plasm-runtime` resolves.

### Schema overlay

At execute session open, when **`schema_overlay:`** is present, the host runs **`database_search`**, projects each database’s **`properties`** schema into **per-database typed `Page` entities**, and merges them into the session CGS. Scoped **`database_query`** decode routes through **`decode.scope`** (`database_id`). See [docs/schema-overlay.md](../../../docs/schema-overlay.md).

### Notion-Version header

Notion requires `Notion-Version` on **every** request. All capabilities in `mappings.yaml` attach:

`Notion-Version: 2026-03-11` (aligned with the domain file comment and current integration docs).

### The Page / Database model (important)

| Concept | In this CGS |
|--------|----------------|
| **Page** | One entity type for **both** standalone pages **and** database **rows**. The official API uses the same `page` object; `parent.type` distinguishes workspace/page vs database. This schema models shared metadata (`id`, `url`, timestamps, trash, …). **Per-database `properties` are typed at session open** via **`schema_overlay:`** (see [schema-overlay.md](../../../docs/schema-overlay.md)); bootstrap `Page` remains the generic row shape. |
| **Database** | Container with a schema; **`database_query`** returns **Page** rows (`POST /v1/databases/{id}/query`). Relation `Database.pages` uses `via_param: database_id` for scoped query. |
| **User** | Person or bot (`type`: `person` \| `bot`). |
| **Comment** | Thread comment; **`comment_query`** is scoped by `block_id` (Notion accepts **page IDs** as block IDs). Relation `Page.comments` uses `via_param: block_id` → `GET /v1/comments?block_id=…`. |

### Content strategy: Markdown API, not blocks

- **`page_get_markdown`** / **`page_update_markdown`** map to `GET` / `PATCH /v1/pages/{id}/markdown` (Enhanced Markdown).
- **`provides:`** on those actions fills `Page.markdown` and `Page.truncated`. The runtime **additively merges** with `page_get` so metadata and content can coexist on one `Page` in the graph cache.
- **Blocks** (`/v1/blocks/...`) are **omitted** from this schema by design (see `domain.yaml` header comments).

### Pagination

| Capability | Style | Notes |
|------------|--------|--------|
| `user_query` | Cursor (`start_cursor`, `page_size`, `has_more`, `next_cursor`) | `GET /v1/users` |
| `comment_query` | Same | `GET /v1/comments?block_id=…` |
| `database_query` | Single POST body page | `page_size` in body; **no** cursor walk in CML yet for this POST |
| `page_search` / `database_search` | POST `/v1/search` | Hardcoded `filter` to `object: page` vs `object: database`; up to **100** results per request; **no** multi-page cursor in this mapping |

### Search vs query

- **`page_search`** / **`database_search`** — global search by title (optional `query`), sort by `last_edited_time`, optional `page_size`.
- **`database_query`** — filter/sort **inside** one database (scope `database_id`); returns **pages** as rows.

---

## What is implemented

### Entities

| Entity | Key | Notable fields | Relations |
|--------|-----|----------------|-----------|
| `Page` | `id` (UUID) | url, public_url, created_time, last_edited_time, in_trash, archived, **markdown**, **truncated** | created_by, last_edited_by → `User`; comments → `Comment` (`via_param` block_id) |
| `Database` | `id` | url, public_url, created_time, last_edited_time, in_trash, is_inline | created_by, last_edited_by → `User`; pages → `Page` (`via_param` database_id) |
| `User` | `id` | name, type, avatar_url, email | — |
| `Comment` | `id` | discussion_id, created_time, last_edited_time | created_by → `User` |

### Capabilities (summary)

| Capability | Kind | Entity | Purpose |
|------------|------|--------|---------|
| `page_get` | get | Page | Metadata + structure (no full markdown until markdown action) |
| `page_create` | create | Page | Create child page or DB row |
| `page_update` | update | Page | Properties, icon, cover, archive |
| `page_trash` | action | Page | `in_trash: true` |
| `page_get_markdown` | action | Page | Load Enhanced Markdown → `markdown`, `truncated` |
| `page_update_markdown` | action | Page | `update_content` / `replace_content` bodies per Notion API |
| `database_get` | get | Database | Schema + metadata |
| `database_query` | query | Page | Rows for one database (`database_id` scope) |
| `user_get` | get | User | By id |
| `user_query` | query | User | List workspace users (cursor pagination) |
| `comment_query` | query | Comment | By `block_id` (cursor pagination) |
| `comment_create` | create | Comment | New thread / reply |
| `page_search` | search | Page | Global page search |
| `database_search` | search | Database | Global database search |

---

## Coverage and limitations

**In scope:** Workspace-level user listing, page/database search, per-database query, page CRUD and trash, markdown read/write, comments scoped to a page/block id.

**Out of scope (by design):**

- **Block API** — no `blocks/{id}/children` traversal in this CGS.
- **Rich dynamic properties** on database rows — not expanded into per-column Plasm fields; the API returns JSON `properties` which this schema does not flatten.
- **Cursor pagination inside `POST .../databases/{id}/query` and `POST /search`** — not wired in CML; you get a single response page (≤100 rows) per call.
- **OAuth user flows** — token is assumed already obtained (integration secret).

---

## Verification

- Schema load + `cgs.validate()` are exercised for `apis/notion` in `plasm-core` (`loader::tests::test_apis_split_schemas_smoke`).
- **Live** calls require a real integration token and pages/databases shared with that integration; exercise with `plasm` in `--mode live` after sharing a test page with the integration.

---

## Example REPL ideas

```text
# List workspace users (cursor pagination — use --limit / --all as supported by agent)
user query

# Search pages by title
page search --query "Roadmap"

# Get page metadata
page <page-uuid>

# Load full markdown content into cache (populates Page.markdown)
page <page-uuid> get-markdown

# Comments on a page (block_id = page id)
comment query --block_id <page-uuid>
```

Exact CLI subcommand names follow `plasm`’s generated command tree from capability names (e.g. `page_get_markdown` → kebab-case per agent rules).
