# Gmail API v1 — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [Gmail REST API v1](https://developers.google.com/gmail/api/reference/rest). Covers the core mailbox surface: messages, threads, labels, drafts, attachments, and profile.

```bash
# Run against the live API (requires GMAIL_ACCESS_TOKEN in env)
export GMAIL_ACCESS_TOKEN=ya29.a0...
cargo run -p plasm-repl -- \
  --schema apis/gmail \
  --backend https://gmail.googleapis.com \
  --repl
```

---

## What the CGS design is

A CGS (Capability Graph Schema) is a semantic domain model for an API. It is explicitly **not** a mirror of the OpenAPI spec — it describes business objects and operations, not RPC endpoints.

The two files:

`**domain.yaml`** — the semantic model. Declares entities, fields, relations, and capability signatures. No HTTP details.

`**mappings.yaml`** — the HTTP wiring. Declares how each capability compiles to an HTTP request using CML (Capability Mapping Language).

### Auth

Gmail uses OAuth 2.0. The access token is injected as `Authorization: Bearer <token>` on every request. Set `GMAIL_ACCESS_TOKEN` to a valid token with the required scopes.

```yaml
auth:
  scheme: bearer_token
  env: GMAIL_ACCESS_TOKEN
```

`**domain.yaml` `oauth:` block** — Documents Google OAuth URLs with `label`, `notes`, and `docs_url` per scope, `**oauth.requirements`** for which grants satisfy each capability (so control planes can filter tools), and `**oauth.default_scope_sets`** — curated **profiles** (same idea as [apis/linear/domain.yaml](../linear/domain.yaml)). Prefer these Plasm-first bundles when choosing grants:


| Profile (`default_scope_sets` key) | Scopes                     | Use when                                                                                                                                                                             |
| ---------------------------------- | -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `plasm_gmail_readonly_mailbox`     | `gmail.readonly`           | Default read automation: list/search, get bodies, attachments, drafts read, labels read, profile — includes **default query hydration** (`messages.get` / `threads.get` after list). |
| `plasm_gmail_modify_mailbox`       | `gmail.modify`             | Triage: readonly-class reads plus trash, label changes on messages/threads, etc.                                                                                                     |
| `plasm_gmail_compose_and_drafts`   | `gmail.compose`            | Drafts and `message_send`; combine with a read scope if the agent must read mail.                                                                                                    |
| `plasm_gmail_send_only`            | `gmail.send`               | Send-only; **not** enough alone for inbox search/list/get in this CGS.                                                                                                               |
| `plasm_gmail_full_mailbox`         | `https://mail.google.com/` | Permanent `message_delete` / `thread_delete` and other full-mailbox cases.                                                                                                           |
| `plasm_gmail_integrator_bundle`    | (toolkit bundle)           | Broad identity + People/Contacts + `mail.google.com/` — not a minimal readonly profile; see notes in CGS.                                                                            |


`**gmail.metadata` is not in this CGS OAuth catalog** — Google’s `https://www.googleapis.com/auth/gmail.metadata` scope is omitted here because combining it with broader Gmail scopes can effectively cap access (clients may end up with the narrowest grant). Use `gmail.readonly`, `gmail.modify`, `mail.google.com/`, etc. from the table above instead.

Rough capability groupings by Google scope (see CGS `oauth.requirements` for the exact matrix):


| Scope                                            | Typical capabilities in this CGS                                                                                                                      |
| ------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `https://www.googleapis.com/auth/gmail.readonly` | `message_list`, `message_search`, `message_get`, `thread_list`, `thread_search`, `thread_get`, drafts/labels read, `attachment_get`, `profile_get`, … |
| `https://www.googleapis.com/auth/gmail.modify`   | Above plus trash, modify labels, non-permanent deletes                                                                                                |
| `https://www.googleapis.com/auth/gmail.compose`  | Drafts, `message_send`                                                                                                                                |
| `https://mail.google.com/`                       | Permanent `message_delete` / `thread_delete`                                                                                                          |


To get a token with the `gmail.readonly` scope using the OAuth 2.0 playground:

1. Visit [OAuth 2.0 Playground](https://developers.google.com/oauthplayground/)
2. Authorize `https://www.googleapis.com/auth/gmail.readonly`
3. Exchange for an access token and copy it

For production use, implement the full OAuth 2.0 flow with refresh tokens. The `SecretProvider` trait in `plasm-runtime::auth` can be extended to fetch tokens from a token store rather than an env var.

### Debugging OAuth link failures (Phoenix → plasm)

When outbound connect fails or the token response looks wrong, correlate logs across the boundary:

1. **Phoenix** — On connect, look for `outbound_oauth_prepare`: `oauth_scopes_empty` and `oauth_scope_count`. If `oauth_scopes_empty=true`, the agent uses **OAuth link catalog `default_scopes`** for that `entry_id`, not the CGS list from the DB row.
2. **plasm** — On `POST /internal/oauth-link/v1/start`, logs use target `plasm_agent::oauth_link` with `oauth.phase=start`, `scopes.source` (`request_body` vs `catalog_default`), `scope_count`, and `scopes_sha256` (SHA-256 of sorted scopes joined by newlines). Set `PLASM_OAUTH_LINK_LOG_SCOPES=1` to log the full scope list at info.
3. **Token exchange** — On callback, the same target logs `oauth.phase=token_exchange` with a **redacted** `TokenEndpointResponseSummary`: top-level JSON keys, presence/length flags for tokens, `scope` / `token_type` / `expires_in` strings from the IdP, RFC 6749 `error` fields when present, and on parse failure `apply_error_kind` (`oidc_id_token_without_access_token`, `missing_access_token`, etc.).

The CGS `oauth:` block in `**domain.yaml`** is the scope catalog; mismatches are usually **propagation** (empty `oauth_scopes` on the auth config, or catalog defaults) — not missing YAML.

### The list/detail split and auto-hydration

Gmail's `messages.list` returns only `{id, threadId}` per message — the minimum. Full message content (headers, body, labels) requires `messages.get`. This is an intentional API design for efficiency: listing 100 messages costs 1 HTTP call; fetching full content costs 101.

The CGS models this with `provides:` annotations:

```yaml
message_list:
  kind: query
  entity: Message
  provides: [id, threadId]        # declares this endpoint returns only these two fields

message_get:
  kind: get
  entity: Message
  provides: [id, threadId, labelIds, snippet, historyId, internalDate, sizeEstimate, headerFrom, headerTo, headerSubject, headerDate, headerReplyTo, headerCc, headerBcc]

thread_list:
  kind: query
  entity: Thread
  provides: [id, historyId]

thread_get:
  kind: get
  entity: Thread
  provides: [id, snippet, historyId]
```

Since both `query` and `get` exist on `Message`, Plasm automatically hydrates: after `message_list` returns summary rows, it concurrently issues `messages.get` for each row, upgrading them to complete objects. The agent sees full messages without any extra logic.

To skip hydration (get just the id/threadId pairs):

```bash
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  message query --summary
```

The same pattern applies to threads: `thread_list` / `thread_search` declare only `{id, historyId}` so rows are **summary** objects; Plasm hydrates with `threads.get` per row by default (N HTTP GETs for N threads — use `thread query --summary` to skip). `thread_get` fills `snippet` and decodes nested `messages` into the `Thread.messages` relation.

**Sending mail:** `message_send` requires a pre-built base64url **`raw`** (full RFC 2822). Prefer **`message_send_simple`** when the agent should pass **from / to / subject / plain body** only; Plasm builds MIME and `raw` via the CML `gmail_rfc5322_send_body` expression (same `POST …/messages/send`). Optional **`threadId`** / **`inReplyTo`** / **`references`** support replies. Use **`message_reply`** (action on a **`Message`** row) to reply with only **`from`** and **`plainBody`**; runtime **`preflight`** runs **`message_get`** on the target id and merges **`parent_*`** fields, then CML **`gmail_rfc5322_reply_send_body`** builds `raw` (same POST).

### Gmail search query syntax

Full-text search uses Gmail's `**q`** query parameter (same as the Gmail search box). You can:

- **Query + optional `q`:** `message query --q "…"` / `thread query --q "…"` (and brace predicates `Message{q="…"}` / `Thread{q="…"}`).
- **Search capability (Plasm `~` syntax):** `message search --q "…"` / `thread search --q "…"` — same HTTP as list; expressions `**Message~"…"`** and `**Thread~"…"`** resolve to `message_search` / `thread_search` (`kind: search`).

The `**snippet**` field on Message is a **preview string** (list rows are minimal; full rows come from `message_get`). On Thread, snippet is filled by `**thread_get`** — `thread_list` rows in this CGS omit it so default hydration can run. For search, use `**q**` or `**~**`, not `snippet`.

Gmail's own search syntax examples:

```
from:alice@example.com          # from a specific sender
to:bob@example.com              # to a specific recipient
subject:meeting                 # subject contains "meeting"
is:unread                       # only unread messages
is:starred                      # only starred messages
has:attachment                  # only messages with attachments
label:work                      # messages with a specific label
after:2024/01/01 before:2025/01/01  # date range
from:alice subject:budget is:unread  # combine multiple terms
```

`message_list` / `thread_list` expose optional `q` on the **query** capability; `message_search` / `thread_search` require `**q`** on the **search** capability for the `~` expression form.

### userId is always `me`

All Gmail API endpoints include `{userId}` in the path, but for single-user API access the value is always `me` (the authenticated user's mailbox). All CML paths hardcode `me` — there's no `userId` parameter in the domain model.

### Attachment compound key (`key_vars`)

Attachments are not addressable by ID alone — they require both `messageId` and `id`:

```
GET /gmail/v1/users/me/messages/{messageId}/attachments/{id}
```

The CGS models this with `key_vars`:

```yaml
Attachment:
  id_field: id
  key_vars: [messageId, id]
```

The runtime cache key becomes `Attachment:<messageId>/<id>`. When fetching an attachment, both values are injected as path variables from the compound `Ref`.

---

## What is implemented

### Entities


| Entity       | Key                       | Notable fields                                                                                                                   | Relations                                               |
| ------------ | ------------------------- | -------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------- |
| `Message`    | `id` (string)             | threadId, labelIds, snippet, historyId, internalDate, sizeEstimate, headerFrom/To/Subject/Date/ReplyTo/Cc/Bcc (from `payload.headers` on `message_get`) | → `Thread` (threadId, EntityRef), → `Attachment` (many) |
| `Thread`     | `id` (string)             | snippet, historyId                                                                                                               | → `Message` (messages, many; from `threads.get`)        |
| `Label`      | `id` (string)             | name, type (system/user), messageListVisibility, labelListVisibility, messagesTotal, messagesUnread, threadsTotal, threadsUnread | —                                                       |
| `Draft`      | `id` (string)             | —                                                                                                                                | —                                                       |
| `Attachment` | `id/messageId` (compound) | size, data (base64url)                                                                                                           | —                                                       |
| `Profile`    | `emailAddress`            | messagesTotal, threadsTotal, historyId                                                                                           | —                                                       |


### Capabilities

#### Message


| Capability        | Kind   | CLI                           | Endpoint                                                                                |
| ----------------- | ------ | ----------------------------- | --------------------------------------------------------------------------------------- |
| `message_list`    | query  | `message query`               | `GET /gmail/v1/users/me/messages`                                                       |
| `message_search`  | search | `message search`              | `GET /gmail/v1/users/me/messages` (same as list; required `--q`; enables `Message~"…"`) |
| `message_get`     | get    | `message <id>`                | `GET /gmail/v1/users/me/messages/{id}`                                                  |
| `message_send`    | create | `message send --raw <base64>` | `POST /gmail/v1/users/me/messages/send`                                                 |
| `message_send_simple` | create | `message send-simple …` (from, to, subject, plain body; optional threadId / inReplyTo / references) | Same POST as `message_send` — CML builds RFC 5322 + base64url `raw` |
| `message_reply` | action | `message reply …` on a message id (from, plainBody; optional to / subject) | Same POST — preflight GET + `gmail_rfc5322_reply_send_body` |
| `message_trash`   | action | `message <id> trash`          | `POST /gmail/v1/users/me/messages/{id}/trash`                                           |
| `message_untrash` | action | `message <id> untrash`        | `POST /gmail/v1/users/me/messages/{id}/untrash`                                         |
| `message_delete`  | delete | `message <id> delete`         | `DELETE /gmail/v1/users/me/messages/{id}`                                               |
| `message_modify`  | update | `message <id> modify`         | `POST /gmail/v1/users/me/messages/{id}/modify`                                          |


#### Thread


| Capability       | Kind   | CLI                   | Endpoint                                                                              |
| ---------------- | ------ | --------------------- | ------------------------------------------------------------------------------------- |
| `thread_list`    | query  | `thread query`        | `GET /gmail/v1/users/me/threads`                                                      |
| `thread_search`  | search | `thread search`       | `GET /gmail/v1/users/me/threads` (same as list; required `--q`; enables `Thread~"…"`) |
| `thread_get`     | get    | `thread <id>`         | `GET /gmail/v1/users/me/threads/{id}`                                                 |
| `thread_trash`   | action | `thread <id> trash`   | `POST /gmail/v1/users/me/threads/{id}/trash`                                          |
| `thread_untrash` | action | `thread <id> untrash` | `POST /gmail/v1/users/me/threads/{id}/untrash`                                        |
| `thread_delete`  | delete | `thread <id> delete`  | `DELETE /gmail/v1/users/me/threads/{id}`                                              |
| `thread_modify`  | update | `thread <id> modify`  | `POST /gmail/v1/users/me/threads/{id}/modify`                                         |


#### Label / Draft / Attachment / Profile


| Capability       | Kind      | CLI                                                         | Endpoint                                                       |
| ---------------- | --------- | ----------------------------------------------------------- | -------------------------------------------------------------- |
| `label_list`     | query     | `label query`                                               | `GET /gmail/v1/users/me/labels`                                |
| `label_get`      | get       | `label <id>`                                                | `GET /gmail/v1/users/me/labels/{id}`                           |
| `label_create`   | create    | `label create --name "..."`                                 | `POST /gmail/v1/users/me/labels`                               |
| `label_update`   | update    | `label <id> update --name "..."`                            | `PUT /gmail/v1/users/me/labels/{id}`                           |
| `label_delete`   | delete    | `label <id> delete`                                         | `DELETE /gmail/v1/users/me/labels/{id}`                        |
| `draft_list`     | query     | `draft query`                                               | `GET /gmail/v1/users/me/drafts`                                |
| `draft_get`      | get       | `draft <id>`                                                | `GET /gmail/v1/users/me/drafts/{id}`                           |
| `draft_create`   | create    | `draft create --raw <base64>`                               | `POST /gmail/v1/users/me/drafts`                               |
| `draft_update`   | update    | `draft <id> update --raw <base64>`                          | `PUT /gmail/v1/users/me/drafts/{id}`                           |
| `draft_delete`   | delete    | `draft <id> delete`                                         | `DELETE /gmail/v1/users/me/drafts/{id}`                        |
| `draft_send`     | action    | `draft <id> send`                                           | `POST /gmail/v1/users/me/drafts/send`                          |
| `attachment_get` | action    | `attachment <messageId/id> attachment-get --messageId <id>` | `GET /gmail/v1/users/me/messages/{messageId}/attachments/{id}` |
| `profile_get`    | singleton | `profile get`                                               | `GET /gmail/v1/users/me/profile`                               |


### CLI examples

```bash
# List inbox messages (auto-hydrates to full messages by default)
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  message query --labelIds INBOX

# List inbox messages without auto-hydration (id+threadId only, fast)
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  message query --labelIds INBOX --summary

# Search for unread messages from a specific sender
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  message query --q "from:alice@example.com is:unread"

# Search for messages with attachments in last 30 days
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  message query --q "has:attachment newer_than:30d" --limit 20

# Get a specific message by ID (full content)
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  message 19328f4ea78d7abc

# Navigate from a message to its thread (EntityRef auto-resolve)
# In REPL: message 19328f4ea78d7abc thread-id

# List all threads matching a query
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  thread query --q "subject:invoice is:unread" --all

# Same thread list without per-thread threads.get (id+historyId only; fast)
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  thread query --q "subject:invoice is:unread" --summary --limit 20

# List all labels
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  label query

# Get specific label details
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  label INBOX

# Create a new label
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  label create --name "Plasm" --labelListVisibility labelShow

# List drafts
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  draft query

# Get mailbox profile
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  profile get

# Trash a message
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  message 19328f4ea78d7abc trash

# Modify labels on a message (mark as read: remove UNREAD)
# plasm --schema apis/gmail --backend https://gmail.googleapis.com \
#   message 19328f4ea78d7abc modify
# (provide addLabelIds/removeLabelIds in body — write capabilities take --input flags)
```

---

## Testing status

### CLI validation

Schema loads without panics. All subcommand names, typed flags, and pagination controls verified.

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/gmail
cargo run -p plasm-repl -- --schema apis/gmail --help
```

CLI outputs verified:

- `message query` — `--q`, `--labelIds` (repeatable), `--includeSpamTrash`, `--limit`, `--all`, `--pageToken`, `--summary` (hydration opt-out)
- `message search` — required `--q`; same optional filters/pagination as query where applicable
- `thread query` / `thread search` — same pattern as message (including `--summary` to skip default `thread_get` hydration)
- `label query` — no filter flags (no pagination — returns all at once)
- `profile` — `get` singleton subcommand

### Against the live Gmail API

Not yet tested with live credentials. To test with an OAuth 2.0 access token:

```bash
export GMAIL_ACCESS_TOKEN=ya29.a0your_token_here

# Profile check
plasm --schema apis/gmail --backend https://gmail.googleapis.com profile get

# List labels (minimal scope needed)
plasm --schema apis/gmail --backend https://gmail.googleapis.com label query

# List inbox messages (id+threadId only, no hydration)
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  message query --labelIds INBOX --summary --limit 10

# List inbox with full message details (auto-hydration fires)
plasm --schema apis/gmail --backend https://gmail.googleapis.com \
  message query --labelIds INBOX --limit 5
```

---

## What remains to be implemented

### High priority — read operations

`**history_list**` — `GET /gmail/v1/users/me/history?startHistoryId=<id>` — incremental sync endpoint that returns all changes since a given history record. Would require a `History` entity (or reuse existing entities with change type fields). Essential for agentic workflows that need to track inbox changes efficiently.

`**attachment_get` via_param traversal** — The `Message.attachments` relation is declared but attachment_get requires `messageId` as a path variable. Currently attachment_get is an action (not a standard get), so `message <id> attachments` navigation doesn't auto-wire the messageId. A `via_params` mapping from Message.id to the messageId scope param would enable seamless traversal.

### Medium priority — write operations

`**message_modify` ergonomics** — The modify capability takes `addLabelIds`/`removeLabelIds` arrays. The current dispatch sends these as a JSON body via `input`. Dedicated convenience capabilities like `message_mark_read`, `message_star`, `message_label` would be more agent-friendly than raw label ID arrays.

**Import messages** — `POST /gmail/v1/users/me/messages` (not `/send`) — imports an email directly into the mailbox without sending. Useful for migration workflows.

`**messages.batchModify` / `messages.batchDelete`** — Apply label changes or delete across many messages in a single request. Requires array body input.

### Lower priority — settings surface

**SendAs aliases** — `GET /gmail/v1/users/me/settings/sendAs` — the list of email addresses/aliases the user can send as. Would need a `SendAs` entity.

**Vacation responder / filters / forwarding** — Various settings endpoints. Low utility for typical agentic workflows.

**Push notifications** — `POST /gmail/v1/users/me/watch` establishes a Cloud Pub/Sub push channel. Not modelable as a standard REST CRUD capability.

---

## Known limitations

**OAuth 2.0 token lifecycle** — The `bearer_token` auth scheme sends a static token from the environment. Gmail access tokens expire after ~1 hour. For long-running agents, the token must be refreshed externally (e.g., via a cron that writes the refreshed token to the env var) or by implementing a custom `SecretProvider` that handles refresh automatically.

**Message body not in entity fields** — The actual email body (text/html content) lives inside `payload.parts[].body.data` — a deeply nested base64-encoded structure. The CGS entity fields only surface top-level scalar properties (`snippet`, `sizeEstimate`, etc.). Full body access requires working with the raw `message_get` response rather than decoded entity fields. Agents that need email content should use the REPL's `--output json` to inspect the full decoded payload.

`**labelIds` is an array of strings** — The `labelIds` filter parameter on `message_list`/`thread_list` accepts repeated query params (`?labelIds=INBOX&labelIds=UNREAD`). Gmail returns only messages matching ALL specified label IDs. The plasm runtime expands `Value::Array` query params as repeated keys automatically, so `--labelIds INBOX --labelIds UNREAD` works correctly.

**Nested messages on threads** — `GET /gmail/v1/users/me/threads/{id}` returns `messages: [...]` on the thread object. The CGS exposes this as the **`Thread.messages`** relation (`from_parent_get`), materialized when you fetch a thread via `thread_get` (including default hydration after `thread_list` / `thread_search`).

**No multi-account support** — All paths hardcode `userId=me`. Supporting multiple Google accounts would require parameterizing the userId, building per-account auth resolvers, and potentially routing to different base URLs.