# ClickUp API v2 — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [ClickUp REST API v2](https://clickup.com/api). Covers the core workspace hierarchy: teams (workspaces), spaces, folders, lists, tasks, goals, views, comments, tags, webhooks, time tracking, and membership.

```bash
# Run against the live API (requires CLICKUP_API_TOKEN in env)
export CLICKUP_API_TOKEN=pk_...
cargo run -p plasm-repl -- \
  --schema apis/clickup \
  --backend https://api.clickup.com/api \
  --repl
```

---

## What the CGS design is

A CGS (Capability Graph Schema) is a semantic domain model for an API. It is explicitly **not** a mirror of the OpenAPI spec. Where OpenAPI describes endpoints ("POST /api/v2/team/{team_id}/task"), a CGS describes business objects ("here is an entity called Task, here is what it contains, here is how it relates to other entities, and here are the operations available on it").

The two files:

**`domain.yaml`** — the semantic model. Declares entities, fields, relations, and capability signatures. No HTTP details.

**`mappings.yaml`** — the HTTP wiring. Declares how each capability compiles to an HTTP request using CML (Capability Mapping Language). Path segments, query params, pagination config, response envelope shape.

### Auth

ClickUp personal API tokens are sent verbatim as `Authorization: <token>` — no `"Bearer "` prefix. Set `CLICKUP_API_TOKEN` in the environment.

```yaml
auth:
  scheme: api_key_header
  header: Authorization
  env: CLICKUP_API_TOKEN
```

### Runtime schema overlay

At execute session open, the host runs the **`schema_overlay.source.steps`** pipeline using session auth only: **`team_query`** → per-team **`custom_field_query`**, then merges **custom field columns** onto **`Task`** (`augment_base`). Agents supply `{ api, entity }` seeds only — no overlay configuration. See [docs/schema-overlay.md](../../../docs/schema-overlay.md).

### ClickUp's workspace hierarchy

ClickUp's data model is a strict containment tree:

```
Team (workspace)
  └── Space
        ├── Folder
        │     └── List
        │           └── Task
        └── List (folderless)
              └── Task
```

Every scoped query in the domain follows this shape. `space_query` requires `team_id`, `folder_query` requires `space_id`, `list_task_query` requires `list_id`, and so on. All of these are declared with `role: scope` — they become URL path variables, not query string filters.

### Pagination — the `last_page` pattern

ClickUp does not use standard cursor or offset pagination. Instead it uses zero-indexed page numbers (`page=0, 1, 2, ...`) and signals the final page by including `"last_page": true` in the response body. This is the _inverse_ of a `has_more` boolean.

Plasm's composable pagination system models this exactly:

```yaml
pagination:
  params:
    - type: counter
      name: page
      start: 0
  location: query
  stop_when:
    type: field_equals
    field: last_page
    value: "true"
```

Pass `--all` to walk all pages. Without it, the first page is returned.

### Tag identity — `id_field: name`

ClickUp tags have no numeric ID. Within a space, a tag is uniquely identified by its name. The domain models this with `id_field: name`:

```yaml
Tag:
  id_field: name
```

The runtime cache key for a tag is `Tag:<name>`. The `task_tag_add` and `task_tag_remove` action capabilities route `tag_name` as a URL path segment (`/v2/task/{task_id}/tag/{tag_name}`).

### Singleton user endpoint

`GET /v2/user` returns the authenticated user with no ID argument, wrapped in a `{"user": {...}}` envelope. This is modelled with `kind: singleton` and `response: {items: user}`:

```yaml
user_get_me:
  kind: singleton
  entity: User
```

CLI: `user get-me` — no ID required.

The runtime transparently handles the single-object-under-key envelope pattern: when the declared `items` key maps to a JSON object rather than an array, it is wrapped into a one-element array before decoding.

---

## What is implemented

### Entities

| Entity | Key | Notable fields | Relations |
|--------|-----|----------------|-----------|
| `Team` | `id` (string) | name, color, avatar | → `Space` (spaces, via_param) |
| `Space` | `id` (string) | name, private, archived | → `Folder` (folders, via_param), → `List` (lists, via_param) |
| `Folder` | `id` (string) | name | → `List` (lists, via_param) |
| `List` | `id` (string) | name, status | → `Task` (tasks, via_param) |
| `Task` | `id` (string) | name, description, status, priority, due_date, start_date, time_estimate, date_created, date_updated, archived, parent, url, markdown_description | → `Comment` (comments, via_param) |
| `User` | `id` (integer) | username, email, color, profilePicture | — |
| `Member` | `id` (integer) | username, email, color | — |
| `Group` | `id` (string) | name | — |
| `View` | `id` (string) | name, type | — |
| `Goal` | `id` (string) | name, due_date, description, color | → `KeyResult` (key_results) |
| `KeyResult` | `id` (string) | name, type, current | — |
| `Comment` | `id` (string) | comment_text, date | — |
| `Tag` | `name` (string, **no numeric ID**) | tag_fg, tag_bg | — |
| `CustomField` | `id` (string) | name, type | — |
| `CustomTaskType` | `id` (string) | name | — |
| `Role` | `id` (string) | name | — |
| `Attachment` | `id` (string) | title, url | — |
| `Template` | `id` (string) | name | — |
| `TimeEntry` | `id` (string) | start_date, end_date, duration | — |
| `TimeInterval` | `id` (string) | start, end, duration | — |
| `Webhook` | `id` (string) | endpoint, status | — |

### Capabilities

#### Team (workspace)

| Capability | Kind | CLI | Endpoint |
|------------|------|-----|----------|
| `team_query` | query | `team query` | `GET /v2/team` |
| `user_get_me` | singleton | `user get-me` | `GET /v2/user` |
| `team_member_query` | query (scoped) | `member query --team_id <id>` | `GET /v2/team/{team_id}/member` |
| `team_create_space` | create | — | `POST /v2/team/{team_id}/space` |
| `team_create_goal` | create | — | `POST /v2/team/{team_id}/goal` |
| `team_create_webhook` | create | — | `POST /v2/team/{team_id}/webhook` |
| `time_entry_query` | query (scoped) | `timeentry query --team_id <id>` | `GET /v2/team/{team_id}/time_entries` |
| `webhook_query` | query (scoped) | `webhook query --team_id <id>` | `GET /v2/team/{team_id}/webhook` |
| `role_query` | query (scoped) | `role query --team_id <id>` | `GET /v2/team/{team_id}/role` |
| `template_query` | query (scoped) | `template query --team_id <id>` | `GET /v2/team/{team_id}/template` |
| `custom_field_query` | query (scoped) | `customfield query --team_id <id>` | `GET /v2/team/{team_id}/field` |
| `custom_task_type_query` | query (scoped) | `customtasktype query --team_id <id>` | `GET /v2/team/{team_id}/taskType` |
| `group_query` | query (scoped) | `group query --team_id <id>` | `GET /v2/group?team_id={id}` |
| `goal_query` | query (scoped) | `goal query --team_id <id>` | `GET /v2/team/{team_id}/goal` |
| `view_query` | query (scoped) | `view query --team_id <id>` | `GET /v2/team/{team_id}/view` |

#### Space

| Capability | Kind | CLI | Endpoint |
|------------|------|-----|----------|
| `space_query` | query (scoped) | `space query --team_id <id>` | `GET /v2/team/{team_id}/space` |
| `space_get` | get | `space <id>` | `GET /v2/space/{id}` |
| `space_update` | update | `space <id> update` | `PUT /v2/space/{id}` |
| `space_delete` | delete | `space <id> delete` | `DELETE /v2/space/{id}` |
| `space_views` | query (scoped) | `view query --space_id <id>` (scoped) | `GET /v2/space/{space_id}/view` |
| `space_fields` | query (scoped) | `customfield query --space_id <id>` (scoped) | `GET /v2/space/{space_id}/field` |
| `tag_query` | query (scoped) | `tag query --space_id <id>` | `GET /v2/space/{space_id}/tag` |

#### Folder / List

| Capability | Kind | CLI | Endpoint |
|------------|------|-----|----------|
| `folder_query` | query (scoped) | `folder query --space_id <id>` | `GET /v2/space/{space_id}/folder` |
| `folder_get` | get | `folder <id>` | `GET /v2/folder/{id}` |
| `folder_list_query` | query (scoped) | `list query --folder_id <id>` | `GET /v2/folder/{folder_id}/list` |
| `list_query` | query (scoped) | `list query --space_id <id>` | `GET /v2/space/{space_id}/list` |
| `list_get` | get | `list <id>` | `GET /v2/list/{id}` |
| `list_task_query` | query (scoped) | `task list-task-query --list_id <id>` | `GET /v2/list/{list_id}/task` |
| `list_members` | query (scoped) | `member query --list_id <id>` (scoped) | `GET /v2/list/{list_id}/member` |

#### Task

| Capability | Kind | CLI | Endpoint |
|------------|------|-----|----------|
| `task_query` | query (scoped) | `task query --team_id <id>` | `GET /v2/team/{team_id}/task` |
| `list_task_query` | query (scoped) | `task list-task-query --list_id <id>` | `GET /v2/list/{list_id}/task` |
| `task_get` | get | `task <id>` | `GET /v2/task/{id}` |
| `task_update` | update | `task <id> update` | `PUT /v2/task/{id}` |
| `task_delete` | delete | `task <id> delete` | `DELETE /v2/task/{id}` |
| `task_tag_add` | action | `task <id> tag-add --tag_name <name>` | `POST /v2/task/{id}/tag/{tag_name}` |
| `task_tag_remove` | action | `task <id> tag-remove --tag_name <name>` | `DELETE /v2/task/{id}/tag/{tag_name}` |
| `comment_query` | query (scoped) | `comment query --task_id <id>` | `GET /v2/task/{task_id}/comment` |
| `task_intervals` | query (scoped) | `timeinterval query --task_id <id>` | `GET /v2/task/{task_id}/time/tracked` |
| `view_tasks` | query (scoped) | `task view-tasks --view_id <id>` | `GET /v2/view/{view_id}/task` |

### CLI examples

```bash
# List all workspaces (one for personal accounts)
plasm --schema apis/clickup --backend https://api.clickup.com/api \
  team query

# Get the authenticated user
plasm --schema apis/clickup --backend https://api.clickup.com/api \
  user get-me

# List spaces in a workspace
plasm --schema apis/clickup --backend https://api.clickup.com/api \
  space query --team_id 9011608233

# List folders in a space
plasm --schema apis/clickup --backend https://api.clickup.com/api \
  folder query --space_id 90112426319

# Query workspace-wide tasks (paginated, first page)
plasm --schema apis/clickup --backend https://api.clickup.com/api \
  task query --team_id 9011608233

# Query all tasks in a list
plasm --schema apis/clickup --backend https://api.clickup.com/api \
  task list-task-query --list_id <list_id> --all

# Query tasks ordered by due date, with subtasks
plasm --schema apis/clickup --backend https://api.clickup.com/api \
  task query --team_id 9011608233 --order_by due_date --subtasks true

# List goals for a workspace
plasm --schema apis/clickup --backend https://api.clickup.com/api \
  goal query --team_id 9011608233

# Get a specific task by ID
plasm --schema apis/clickup --backend https://api.clickup.com/api \
  task 9az123xyz

# List tags in a space
plasm --schema apis/clickup --backend https://api.clickup.com/api \
  tag query --space_id 90112426319

# List workspace members
plasm --schema apis/clickup --backend https://api.clickup.com/api \
  member query --team_id 9011608233
```

---

## Testing status

### CLI validation

Schema loads without panics. All subcommand names, typed flags, and help text verified.

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/clickup
cargo run -p plasm-repl -- --schema apis/clickup --help
```

### Against the live ClickUp API

Tested with `CLICKUP_API_TOKEN` (personal API token, no `Bearer` prefix):

| Command | Result |
|---------|--------|
| `team query` | ✅ Returns workspace |
| `user get-me` | ✅ Returns authenticated user |
| `space query --team_id <id>` | ✅ Returns 5 spaces |
| `folder query --space_id <id>` | ✅ Returns 0 (no folders in test space) |
| `goal query --team_id <id>` | ✅ Returns goals |
| `tag query --space_id <id>` | ✅ Returns tags (0 in test space) |

### LLM eval harness (`plasm-eval`)

NL→Plasm path-expression cases for this schema live in [`eval/cases.yaml`](eval/cases.yaml). They exercise scoped queries (`team_id`, `space_id`, `list_id`), the `User` singleton, workspace-wide `Team` listing, relation chains (e.g. `Team` → `spaces`), and correction scoring.

```bash
# Requires OPENROUTER_API_KEY and `baml-cli generate` (see crates/plasm-eval)
plasm-eval --schema apis/clickup --cases apis/clickup/eval/cases.yaml --attempts 2
```

---

## What remains to be implemented

### High priority — missing read operations

**`task_query` — missing scope via traversal**

`task_query` requires `team_id` (workspace-scoped). For `List.tasks` traversal, the `via_param: list_id` relation exists but the list-scoped `list_task_query` needs `list_id` from the `List` entity Ref. This works correctly via the `via_param` mechanism when navigating from a known List entity in the REPL (`list <id> tasks`).

**`folder_list_query` vs `list_query` disambiguation**

Both `folder_list_query` (scoped by `folder_id`) and `list_query` (scoped by `space_id`) target the `List` entity. The CLI exposes both via the `list` entity's subcommands. A future improvement would auto-route based on which parent ID is provided.

**View tasks not paginated end-to-end**

`view_tasks` has pagination config but the current dispatch for the `view-tasks` subcommand may not pass `view_id` correctly through the scoped query path. Needs end-to-end testing.

**`task_get` with `include_markdown_description`**

`task_get` declares `provides:` for all Task fields including `markdown_description`. The `include_markdown_description=true` query param needs to be passed explicitly in the mapping (currently not wired to a capability parameter).

### Medium priority — write operations

**`list_create_task`** — create a task in a list (POST /v2/list/{list_id}/task)

**`task_update`** — update task properties (PUT /v2/task/{id}). Mapping exists; needs dispatch testing.

**Comment CRUD** — `task_create_comment` (mapping exists), `comment_update`, `comment_delete`.

**`goal_create_key_result`** / `key_result_update` — key result management.

### Lower priority — broader surface

**Checklist items** — each checklist has a separate CRUD surface (`/v2/checklist/{checklist_id}/checklist_item`). Would require a `ChecklistItem` entity and `key_vars: [checklist_id, id]`.

**Custom field values on tasks** — setting/getting custom field values is a parallel operation on tasks (`/v2/task/{task_id}/field/{field_id}`). Would benefit from a `TaskFieldValue` entity with `key_vars: [task_id, field_id]`.

**Tag assignment as compound identity** — adding/removing tags from tasks uses the tag name in the path. A future `via_params` relation could express `Task.tags → Tag` traversal with `tag_name` sourced from the Tag's `name` key_var.

**Attachments** — file upload requires multipart form data; not yet supported in the CML HTTP layer.

**OAuth 2.0** — ClickUp supports OAuth2 for user authorization flows. The domain currently uses `api_key_header` (personal token). An `oauth2_auth_code` scheme would be needed for multi-user contexts.

---

## Known limitations

**`space_query` does not return `team_id`** — ClickUp Space objects in the API response do not include a `team_id` field. The domain model does not declare `team_id` as an entity field on `Space` (it's a path-scope param only). This means Space entities in the cache cannot be traversed back to their Team without re-querying.

**Tags have no numeric ID** — The `id_field: name` pattern means tag cache keys are `Tag:<name>`. If two spaces have a tag with the same name, the cache would conflate them. In practice, the REPL caches are scoped per-session and this is unlikely to cause issues.

**`team_query` response includes full member list** — ClickUp's `GET /v2/team` response nests all workspace members inside the team objects. The domain currently ignores this (members are decoded via `team_member_query`). A future `provides:` annotation could auto-populate Member entities from the team response.

**Subcommand naming** — Several scoped query capabilities produce a `query` subcommand name after stripping the entity prefix (e.g., `space_query` → `query` on the `Space` entity). The CLI auto-rename system fires warnings and registers them as `query` (the default subcommand name) since the capabilities are the primary queries for their entities. The renamed form (e.g., `space-query`) does not exist as a separate subcommand — the standard `space query` form is correct.

**No `--limit` on `task_query`** — The workspace-wide `task_query` and `list_task_query` use ClickUp's page-counter pagination. The `--all` flag walks all pages. A `--limit` flag for item count is not yet wired to the CML pagination config for these capabilities.
