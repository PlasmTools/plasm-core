# Jira Cloud REST API v3 — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [Jira REST API v3](https://developer.atlassian.com/cloud/jira/platform/rest/v3/intro/).

```bash
export JIRA_AUTH="Basic $(echo -n 'your@email.com:your_api_token' | base64)"
cargo run --bin plasm -- \
  --schema apis/jira \
  --backend https://your-domain.atlassian.net \
  --repl
```

With a **multi-entry** catalog (`--catalog-dir` after packing) or federation via MCP `plasm_context`, Jira credentials come from the **Jira** catalog entry’s [`domain.yaml`](domain.yaml) (`auth:` / `JIRA_AUTH`) on Jira execute sessions, not from another entry’s CGS.

### Empty lists vs HTTP errors

Jira often returns **HTTP 200** with an empty `values` (or similar) list when there are no matching projects or issues — the REST API frequently does **not** say whether that is due to **permissions**, **filters**, or a genuinely empty site. **Non-2xx** responses usually include structured JSON (`errorMessages`, `errors`, …); the Plasm runtime extracts and **caps** those strings for user-visible errors so large HTML or JSON bodies cannot flood MCP or execute output. If tools return only empty lists, verify the API token’s site access and project roles in Jira.

---

## CGS design notes

### Schema overlay

At execute session open, the host runs **`project_query`** → per-project **`issue_createmeta_get`**, then merges **per-project/issuetype typed `Issue` entities**. Session auth only — no client overlay scope. See [docs/schema-overlay.md](../../../docs/schema-overlay.md).

### Why JQL is `kind: search`

The Jira issue model has no traditional SQL-style filter endpoint. The only way to query issues is via **JQL (Jira Query Language)**: a full-featured query language that handles filters, text search, ordering, and date ranges in a single string. Modelling this as `kind: search` with a required `jql` parameter is semantically accurate — users write JQL directly:

```bash
# Open bugs assigned to me
issue search --jql "issuetype=Bug AND status=Open AND assignee=currentUser()"

# Issues updated in the last week on a project
issue search --jql "project=MYPROJ AND updated >= -7d ORDER BY updated DESC"

# High-priority epics
issue search --jql "issuetype=Epic AND priority=High"
```

On **Jira Cloud**, `GET /rest/api/3/search/jql` often rejects *unbounded* JQL (e.g. `order by created DESC` with no filter). Add a restriction such as `updated >= -365d`, `project = MYPROJ`, or `assignee = currentUser()` so the query is bounded.

### JQL search: `fields` and `expand`

[`GET /rest/api/3/search/jql`](https://developer.atlassian.com/cloud/jira/platform/rest/v3/api-group-issue-search/#api-rest-api-3-search-jql-get) takes required **`jql`** plus optional **`fields`** and **`expand`**.

- **`fields`** — Jira issue field ids to include in each issue’s `fields` object (e.g. `summary`, `status`, `assignee`). Use this to shrink responses when you only need a few columns. In the CLI, pass **`--fields`** multiple times (one value per flag).
- **`expand`** — Optional expansions (Jira-defined strings; often comma-separated in the HTTP API). Use when you need rendered bodies, names maps, or other expanded blobs; see Atlassian’s parameter docs for valid names.

The CGS models these as **`role: response_control`** parameters on `issue_jql` — they do not filter *which issues* match; they only affect the shape of each matched issue.

### The `path:` field annotation

Jira issue responses nest all business fields under a `fields` wrapper:

```json
{
  "id": "10001",
  "key": "PROJ-1",
  "fields": {
    "summary": "Login fails for SSO users",
    "status": { "id": "10001", "name": "In Progress" },
    "priority": { "name": "High" },
    "assignee": { "accountId": "abc", "displayName": "Jane Smith" }
  }
}
```

The standard Plasm decoder reads top-level JSON keys by field name. The `path:` annotation on `FieldSchema` tells the decoder to navigate a dot-separated JSON path instead:

```yaml
summary:
  field_type: string
  path: fields.summary          # reads response["fields"]["summary"]
status:
  field_type: string
  path: fields.status.name      # reads response["fields"]["status"]["name"]
assignee_id:
  field_type: string
  path: fields.assignee.accountId
```

This is a new CGS feature introduced for Jira (also applicable to any API with nested response envelopes). The decoder builds the `PathExpr` from the dot-separated string at schema load time.

### User GET uses query string, not path

`GET /rest/api/3/user` takes `accountId` as a query parameter rather than a path segment. The CML mapping handles this by placing the `id` variable (= accountId value) in the `query:` block instead of `path:`:

```yaml
user_get:
  method: GET
  path: [rest, api, "3", user]
  query:
    type: object
    fields:
      - [accountId, { type: var, name: id }]
```

This works because `kind: get` injects `id` = the entity's `id_field` value into the CML env, and the `query:` block can reference it.

### `key` as id_field

Both `Issue` and `Project` use `key` (e.g. `PROJ-123`, `PROJ`) as `id_field`. Jira's GET endpoints accept either the human-readable key or the numeric ID via `{issueIdOrKey}` and `{projectIdOrKey}`. The key is preferred because:
- It's human-readable and memorable
- It's stable within a project (unlike numeric IDs which may be reassigned)
- It's what users type in JQL and URLs

---

## What is implemented

### Entities

| Entity | Key | Fields | Relations |
|--------|-----|--------|-----------|
| `Issue` | `key` (e.g. PROJ-123) | key, id, summary, description, status, status_category, priority, issuetype, resolution, assignee_id, assignee_name, reporter_id, reporter_name, project_key, project_name, parent_key (→ `Issue`), labels, duedate, created, updated, resolutiondate | → `Comment`, `Worklog`, `Changelog`, `RemoteLink`, `Transition`, `Attachment` (via_param / embedded on issue GET) |
| `Project` | `key` (e.g. PROJ) | key, id, name, description, projectTypeKey, archived, isPrivate, lead_account_id, lead_name | → `Component` (via_param) |
| `User` | `accountId` | accountId, displayName, emailAddress, active, timeZone, accountType | — |
| `Comment` | `id` | id, body, author_id, author_name, created, updated | — |
| `Worklog` | `id` | id, issueId, author_id, author_name, comment, started, timeSpent, timeSpentSeconds, created, updated | — |
| `Changelog` | `id` | id, author_id, author_name, created, items | — |
| `RemoteLink` | `id` | id, globalId, relationship, object_title, object_url, object_summary, application_name, application_type | — |
| `Transition` | `id` | id, name, isAvailable, isConditional, isGlobal, isInitial | — |
| `Component` | `id` | id, name, description, assigneeType, lead_id, lead_name, project | — |
| `Priority` | `id` | id, name, description, isDefault, statusColor | — |
| `Status` | `id` | id, name, description, statusCategory | — |
| `IssueType` | `id` | id, name, description, subtask, hierarchyLevel | — |
| `Version` | `id` | id, name, description, released, archived, releaseDate, startDate, project, overdue | — |
| `Attachment` | `id` | id, filename, mimeType, size, author_id (→ `User`), created | — |
| `IssueVoteState` | `issue_key` | issue_key, vote_count, has_voted | — |
| `IssueEditMeta` | `issue_key` | issue_key, fields (JSON) | — |
| `IssueCreateMetaBundle` | `id` | id, projects (JSON) | — |
| `Group` | `name` | groupId, name, self | — |
| `MyPermissionSet` | `id` | id, permissions (JSON) | — |
| `SavedFilter` | `id` | id, name, description, jql, favourite | — |
| `Dashboard` | `id` | id, name, description | — |
| `Board` | `id` | id, name, type, self | — |
| `Sprint` | `id` | id, name, state, startDate, endDate, boardId | — |
| `Webhook` | `id` | id, jql, url, events | — |

### Capabilities

| Capability | Kind | CLI | Endpoint |
|------------|------|-----|----------|
| `issue_get` | get | `issue PROJ-123` | `GET /rest/api/3/issue/{issueIdOrKey}` |
| `issue_jql` | search | `issue search --jql "..."` [`--fields` …] [`--expand` …] | `GET /rest/api/3/search/jql` |
| `issue_create` | create | `issue create --project_key PROJ --issue_type Task --summary "..."` | `POST /rest/api/3/issue` |
| `issue_update` | update | `issue PROJ-1 update [--summary …] [--assignee_id …] …` | `PUT /rest/api/3/issue/{issueIdOrKey}` |
| `issue_delete` | delete | `issue PROJ-1 delete` | `DELETE /rest/api/3/issue/{issueIdOrKey}` |
| `issue_transition` | action | `issue PROJ-1 transition --transition_id "31"` | `POST /rest/api/3/issue/{issueIdOrKey}/transitions` |
| `project_get` | get | `project PROJ` | `GET /rest/api/3/project/{projectIdOrKey}` |
| `project_query` | query | `project query [--query text] [--typeKey software]` | `GET /rest/api/3/project/search` |
| `project_version_query` | query (scoped) | `version query --projectIdOrKey PROJ` | `GET /rest/api/3/project/{key}/version` |
| `user_get` | get | `user abc123def` | `GET /rest/api/3/user?accountId=...` |
| `user_myself` | query | `user query` | `GET /rest/api/3/myself` |
| `user_search` | search | `user search --query "Jane Smith"` | `GET /rest/api/3/user/search` |
| `comment_create` | create | `comment create --issueIdOrKey PROJ-1 --text "…"` | `POST /rest/api/3/issue/{key}/comment` |
| `comment_query` | query (scoped) | `comment query --issueIdOrKey PROJ-123` | `GET /rest/api/3/issue/{key}/comment` |
| `worklog_query` | query (scoped) | `worklog query --issueIdOrKey PROJ-123` | `GET /rest/api/3/issue/{key}/worklog` |
| `changelog_query` | query (scoped) | `changelog query --issueIdOrKey PROJ-123` | `GET /rest/api/3/issue/{key}/changelog` |
| `remotelink_query` | query (scoped) | `remotelink query --issueIdOrKey PROJ-123` | `GET /rest/api/3/issue/{key}/remotelink` |
| `transition_query` | query (scoped) | `transition query --issueIdOrKey PROJ-123` | `GET /rest/api/3/issue/{key}/transitions` |
| `component_query` | query (scoped) | `component query --projectIdOrKey PROJ` | `GET /rest/api/3/project/{key}/component` |
| `component_get` | get | `component 10001` | `GET /rest/api/3/component/{id}` |
| `priority_query` | query | `priority query` | `GET /rest/api/3/priority/search` |
| `priority_get` | get | `priority 1` | `GET /rest/api/3/priority/{id}` |
| `status_query` | query | `status query` | `GET /rest/api/3/status` |
| `status_get` | get | `status 10001` | `GET /rest/api/3/status/{idOrName}` |
| `issuetype_query` | query | `issuetype query` | `GET /rest/api/3/issuetype` |
| `issuetype_get` | get | `issuetype 10001` | `GET /rest/api/3/issuetype/{id}` |

**Collaboration & files** — `attachment_get` / `attachment_delete` (file metadata by id; enumerate via `Issue(…).attachments` when the issue payload includes attachment metadata). `issue_watcher_query`, `issue_watcher_add`, `issue_watcher_remove`. `issue_vote_get`, `issue_vote_add`, `issue_vote_remove`.

**Field metadata & bulk read** — `issue_editmeta_get`, `issue_createmeta_get`, `issue_bulk_fetch`.

**Directory & admin** — `group_query`, `group_get`, `group_delete`. `mypermissions_get` requests a **default bundle** of permission keys in the HTTP layer (Jira Cloud requires `permissions=`); use the REST API directly if you need a custom key list or strict project/issue scope. `filter_query`, `filter_get`, `dashboard_query`, `dashboard_get`.

**Integrations** — `webhook_query`, `webhook_register`, `webhook_delete` (delete sends id list in the JSON body per Jira).

**Jira Software (Agile)** — `board_query`, `board_get`, `sprint_query`, `sprint_get` use `/rest/agile/1.0/…` on the same `--backend` host as Platform REST.

**Not in Plasm (transport or shape limits)** — Multipart **file upload** to attach binaries (CML is JSON bodies only). The **global label** suggest API returns a page of plain strings; issue-level labels remain on `Issue` via `issue_get` / `issue_jql`. JQL search is **`issue_jql`** (`/search/jql`), not legacy `/search`.

Mechanical path/method detail lives in `mappings.yaml` comments; capability names and entity text above stay **domain-first** per the authoring reference.

### Writes (issues and comments)

Mutations use the same `JIRA_AUTH` and `--backend` as reads. Bodies follow Jira’s **IssueUpdateDetails** shape where applicable (`fields`, `transition`, …); see the [REST reference](https://developer.atlassian.com/cloud/jira/platform/rest/v3/intro/).

- **Create issue** — `issue create` sends `project`, `issuetype`, and `summary` (optional `description` as plain text; Cloud sites that require [ADF](https://developer.atlassian.com/cloud/jira/platform/apis/document/structure/) for description may reject plain strings).
- **Update issue** — `issue <KEY> update` merges optional `--summary`, `--description`, `--assignee_id`, `--priority_name`, repeatable `--labels` into `fields` (replaces labels when provided).
- **Delete issue** — `issue <KEY> delete`.
- **Transition** — list ids with `issue <KEY> transitions` (query), then `issue <KEY> transition --transition_id <id>` (often HTTP 204 with no JSON body).
- **Add comment** — `comment create` wraps `--text` in minimal **Atlassian Document Format** for the `body` field.

### REPL expressions

```
# Get an issue
Issue("PROJ-123")

# JQL search
Issue~"issuetype=Bug AND status=Open ORDER BY priority"
Issue~"project=PROJ AND updated >= -7d"[key,summary,status,priority]

# Get a project
Project("PROJ")

# Navigate issue → sub-resources (all via_param traversals)
Issue("PROJ-123").comments
Issue("PROJ-123").worklogs
Issue("PROJ-123").changelog
Issue("PROJ-123").remotelinks
Issue("PROJ-123").transitions

# Navigate project → components
Project("PROJ").components

# List project versions
Version{projectIdOrKey=PROJ}

# Get a user
User("accountId123")

# Lookup status by name
Status("In Progress")
```

### CLI examples

```bash
# JQL search with pagination
plasm --schema apis/jira --backend https://domain.atlassian.net \
  issue search --jql "project=MYPROJ AND issuetype=Bug AND status!=Done" --limit 50

# Same search, only fetch a few issue fields (smaller payload)
plasm --schema apis/jira --backend https://domain.atlassian.net \
  issue search --jql "project=MYPROJ" --fields summary --fields status --fields priority --limit 20

# Create / update / transition / comment (requires write scope on the token)
plasm --schema apis/jira --backend https://domain.atlassian.net \
  issue create --project_key MYPROJ --issue_type Task --summary "New task from Plasm"
plasm --schema apis/jira --backend https://domain.atlassian.net \
  issue MYPROJ-42 update --summary "Updated title" --priority_name High
plasm --schema apis/jira --backend https://domain.atlassian.net \
  issue MYPROJ-42 transition --transition_id "31"
plasm --schema apis/jira --backend https://domain.atlassian.net \
  comment create --issueIdOrKey MYPROJ-42 --text "Ship it"

# Get a specific issue (extracts nested fields via path: annotations)
plasm --schema apis/jira --backend https://domain.atlassian.net \
  issue MYPROJ-42

# List all projects
plasm --schema apis/jira --backend https://domain.atlassian.net \
  project query --all

# Search projects by name
plasm --schema apis/jira --backend https://domain.atlassian.net \
  project query --query "Platform" --typeKey software

# List versions for a project
plasm --schema apis/jira --backend https://domain.atlassian.net \
  version project-version-query --projectIdOrKey MYPROJ

# Get comments on an issue
plasm --schema apis/jira --backend https://domain.atlassian.net \
  comment query --issueIdOrKey MYPROJ-42

# Navigate issue → comments in REPL
# (via_params: issueIdOrKey ← Issue.key fires comment_query automatically)
plasm --schema apis/jira --backend https://domain.atlassian.net --repl
# plasm> Issue("MYPROJ-42").comments
```

---

## Authentication

### Jira Cloud (API Token)

Generate an API token at https://id.atlassian.com/manage-profile/security/api-tokens.

```bash
export JIRA_AUTH="Basic $(echo -n 'your@email.com:your_api_token' | base64)"
```

### Jira Data Center / Server (Personal Access Token)

Generate a PAT in your Jira profile settings.

```bash
export JIRA_AUTH="Bearer your_personal_access_token"
```

Both are injected as the `Authorization` header by the auth scheme declared in `domain.yaml`.

---

## Testing status

### CLI validation

Schema loads and CLI generates correctly. All 20 capabilities verified:

```bash
cargo run --bin plasm -- --schema apis/jira --help
cargo run --bin plasm -- --schema apis/jira issue --help
cargo run --bin plasm -- --schema apis/jira issue search --help
cargo run --bin plasm -- --schema apis/jira project query --help
cargo run --bin plasm -- --schema apis/jira worklog query --help
cargo run --bin plasm -- --schema apis/jira changelog query --help
cargo run --bin plasm -- --schema apis/jira component query --help
```

Pagination flags (`--limit`, `--all`, `--offset`) confirmed present on paginated capabilities.
All `via_param` relation traversals wired: `issue <id> comments/worklogs/changelog/remotelinks/transitions` and `project <id> components`.

### Against real server

Not yet tested live. The `path:` annotation mechanism that extracts `fields.summary` etc. from Jira's nested response envelope has not been validated against real issue responses.

---

## What remains to be implemented

### Write operations (create/update/transition)

```
POST  /rest/api/3/issue                    → issue_create
PUT   /rest/api/3/issue/{key}              → issue_update (summary, description, priority, assignee)
POST  /rest/api/3/issue/{key}/transitions  → issue_transition (move to new status)
POST  /rest/api/3/issue/{key}/comment      → comment_create
PUT   /rest/api/3/issue/{key}/assignee     → issue_assign
POST  /rest/api/3/issue/{key}/worklog      → worklog_create
```

### Sprints and boards (Jira Software — separate API)

Sprint and board data lives under the Agile REST API (`/rest/agile/1.0/`), not the core REST API v3. These require a separate schema base URL and auth, but the same Plasm mechanisms apply:

```
GET /rest/agile/1.0/board                           → board_query
GET /rest/agile/1.0/board/{boardId}/sprint          → sprint_query (scoped to board)
GET /rest/agile/1.0/sprint/{sprintId}/issue         → sprint_issue_query (scoped)
```

These are the highest-priority missing capabilities for OpenClaw agent workflows.

### Rich text / ADF

Issue `description` and comment `body` fields return Atlassian Document Format (ADF) — a JSON document structure, not a plain string. Currently decoded as a raw string. A dedicated `adf_to_markdown` transformer would be useful.

### CGS structural gaps (cannot express today)

**FK scalar traversal**: `assignee_id` and `reporter_id` are embedded User references that CGS cannot auto-traverse — there is no `field_type: entity_ref` with a resolver backed by `user_get` for scalar fields yet.

**Self-referential relations**: `parent_key` on Issue refers to another Issue. CGS has no notion of a same-entity relation.

**Inline link graphs** (`fields.issuelinks`): Issues contain a many-to-many graph with typed edges (`blocks`, `duplicates`, `relates to`) encoded as an inline array in the issue GET response. This cannot be expressed as a CGS relation at all — it is not a separate sub-resource endpoint, and CGS has no `field_type: links` with edge-type metadata.

---

## Known limitations

**ADF body fields**: `description` and `body` (comment) are ADF JSON objects in the real response. The `path:` annotation extracts the nested structure but the decoder returns it as a raw Value. For agents that need readable text, an ADF-to-markdown post-processor would be needed.

**`issue_jql` cursor pagination**: Uses `nextPageToken` (from_response style). The `--cursor` flag is not exposed (nextPageToken does not contain "cursor" in the name). Use `--all` to fetch all pages; the runtime handles the cursor internally.

**Agile API not covered**: Boards, sprints, backlogs, and velocity data live at `/rest/agile/1.0/` which requires a separate base URL configuration. Point `--backend` at `https://domain.atlassian.net` and the core API works; Agile endpoints would need a second schema or a base-path override mechanism.

**Comment items key**: Jira returns issue comments as `{"comments": [...]}` — the `comment_query` mapping uses `items: comments` which correctly decodes them.
