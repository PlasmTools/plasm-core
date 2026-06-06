# Slack Web API — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [Slack Web API](https://api.slack.com/methods). Covers the core messaging and collaboration surface: channels (conversations), messages, users, files, user groups, reminders, and pinned items.

```bash
# Run against the live API (requires SLACK_BOT_TOKEN in env)
export SLACK_BOT_TOKEN=xoxb-...
cargo run -p plasm --bin plasm-cgs -- \
  --schema apis/slack \
  --backend https://slack.com/api \
  --repl
```

---

## What the CGS design is

A CGS (Capability Graph Schema) is a semantic domain model for an API. It is explicitly **not** a mirror of the OpenAPI spec. Where OpenAPI describes endpoints ("POST /conversations.history"), a CGS describes business objects ("here is a Channel, here is its message history, here are the operations available on it").

The two files:

**`domain.yaml`** — the semantic model. Declares entities, fields, relations, and capability signatures. No HTTP details.

**`mappings.yaml`** — the HTTP wiring. Declares how each capability compiles to an HTTP request using CML (Capability Mapping Language). Path segments, query params, pagination config, response envelope shape.

### Auth

Slack uses Bearer tokens (Bot Token `xoxb-...` or User Token `xoxp-...`) for all Web API calls. Set `SLACK_BOT_TOKEN` in the environment.

```yaml
auth:
  scheme: bearer_token
  env: SLACK_BOT_TOKEN
```

The required OAuth scopes depend on which capabilities you use. For a bot with read access: `channels:read`, `channels:history`, `users:read`, `files:read`, `usergroups:read`. For write access add: `chat:write`, `channels:manage`.

### Cursor-based pagination

All Slack list endpoints use cursor-based pagination via a nested response field:

```json
{
  "channels": [...],
  "response_metadata": {
    "next_cursor": "dGVhbTpDMDYxRkE1UEI="
  }
}
```

The cursor token from `response_metadata.next_cursor` is passed as the `cursor` query parameter on subsequent requests. Pagination stops when the field is absent or empty.

Plasm's pagination config uses dot-path notation for nested response fields (a runtime feature — `.` separates JSON object levels):

```yaml
pagination:
  params:
    cursor: { from_response: response_metadata.next_cursor }
    limit: { fixed: 200 }
  location: query
```

Pass `--all` to walk all pages. Without it, only the first page is returned. Pass `--cursor <token>` to resume from a known position.

### Response envelopes

Every Slack API response wraps its payload under a named key and includes an `ok` boolean:

```json
{ "ok": true, "channels": [...] }
{ "ok": true, "user": {...} }
{ "ok": true, "channel": {...} }
```

All list capabilities declare `response: {items: <key>}` to direct the decoder. For single-entity responses (get/singleton), the same config enables the runtime to unwrap the object under its key before decoding — so `channel_info` with `response: {items: channel}` correctly decodes the nested `channel` object.

### Method URL pattern

All Slack API methods are at flat paths (no path parameters):

```
GET  /conversations.list
GET  /conversations.history?channel=C123&cursor=...
POST /chat.postMessage   (body: channel, text, ...)
```

CML path segments for Slack contain exactly one literal with the full method name, e.g.:

```yaml
path:
  - type: literal
    value: conversations.list
```

The `--backend` URL is `https://slack.com/api`, making the full URL `https://slack.com/api/conversations.list`.

### Message identity

Slack messages are identified by their `ts` (timestamp) field — a decimal string like `"1512085950.000216"`. **`ts` is only unique within a channel**; the CGS still uses `id_field: ts` because that is Slack’s stable per-conversation key. **Scoped capabilities** (`channel_history`, `channel_replies`, `message_post`, `message_update`, `message_delete`, etc.) always take `channel` (and thread operations take `thread_ts` / `ts`) so the runtime knows which conversation the message belongs to.

When the API returns them on message objects, the domain also exposes optional **`channel`** and **`user`** as `entity_ref` fields (`Channel`, `User`) for navigation and predicates — they do not replace the need for `channel` on capabilities when addressing a message by `ts` alone.

### Ergonomics (CGS / [plasm-authoring](../../skills/plasm-authoring/SKILL.md))

| Topic | Model in this schema |
|--------|------------------------|
| **Channel context** | Optional `Message.channel` when wire payloads include `channel` / channel id; identity remains `ts` + scope via capabilities. |
| **Author context** | Optional `Message.user` when wire payloads include `user` (user id); omitted for some bot/system messages. |
| **Block Kit / post body** | `message_post` / `message_update` declare `blocks`, `attachments` (`json_text`), `unfurl_links`, `unfurl_media`, `mrkdwn` (post). CML uses `body: { type: var, name: input }` for `chat.postMessage` so the merged create/update **input** object is sent as JSON; optional keys are listed in the domain, not “hidden” passthrough. |
| **List vs get** | List/query capabilities use explicit **`provides:`** where list rows are strict subsets of the corresponding **get** (or full) shape — see `domain.yaml` (e.g. `user_list` vs `user_info`, `channel_list` vs `channel_info`, `channel_history` / `channel_replies`, `file_list`, `pin_list`, `bookmark_list`, …). |
| **teaching** | `channel_history` vs `channel_replies` are disambiguated in core (`query_resolve`: required filter-like params such as `ts` participate in capability matching). `provides:` on those caps aligns prompt projection with responses. |

---

## What is implemented

### Entities

| Entity | Key | Notable fields | Relations |
|--------|-----|----------------|-----------|
| `Channel` | `id` (C/G/D/W-prefixed) | name, is_private, is_archived, is_general, is_member, topic, purpose, num_members | → `Message` (messages), → `User` (members), → `Pin` (pins), → `Bookmark` (bookmarks), → `ScheduledMessage` (scheduled_messages) — scoped params |
| `Message` | `ts` (timestamp string) | type, text, subtype, thread_ts, reply_count, permalink; optional `channel` / `user` refs when the API returns them | — |
| `Bookmark` | `id` (Bk… ) | title, link, emoji, type, channel_id, rank, date_created | — |
| `User` | `id` (U/W-prefixed) | name, real_name, display_name, email, is_bot, is_admin, deleted, tz | — |
| `File` | `id` (F-prefixed) | name, title, mimetype, filetype, size, created, url_private, permalink, is_public | — |
| `UserGroup` | `id` (S-prefixed) | name, handle, description, is_usergroup, date_create, user_count | — |
| `Team` | `id` (T-prefixed) | name, domain, email_domain | — |
| `Reminder` | `id` | text, recurring, time, completed | — |
| `Bot` | `id` (B-prefixed) | name, deleted | — |
| `Pin` | `id` | type, created | — |
| `ScheduledMessage` | `id` (opaque; Slack may return a numeric id in list) | channel_id, post_at, date_created, text | — |

### Capabilities

#### Channel (conversations)

| Capability | Kind | CLI | Endpoint |
|------------|------|-----|----------|
| `channel_list` | query | `channel query` | `GET /conversations.list` |
| `channel_info` | get | `channel <id>` | `GET /conversations.info` |
| `channel_history` | query (scoped) | `message channel-history --channel <id>` | `GET /conversations.history` |
| `channel_replies` | query (scoped) | `message channel-replies --channel <id> --ts <ts>` | `GET /conversations.replies` |
| `channel_members` | query (scoped) | `user channel-members --channel <id>` | `GET /conversations.members` |
| `channel_create` | create | `channel create --name <name>` | `POST /conversations.create` |
| `channel_archive` | action | `channel <id> archive` | `POST /conversations.archive` |
| `channel_unarchive` | action | `channel <id> unarchive` | `POST /conversations.unarchive` |
| `channel_join` | action | `channel <id> join` | `POST /conversations.join` |
| `channel_leave` | action | `channel <id> leave` | `POST /conversations.leave` |
| `channel_rename` | update | `channel <id> rename --name <name>` | `POST /conversations.rename` |
| `channel_set_topic` | action | `channel <id> set-topic --topic "..."` | `POST /conversations.setTopic` |
| `channel_set_purpose` | action | `channel <id> set-purpose --purpose "..."` | `POST /conversations.setPurpose` |
| `channel_invite` | action | `channel <id> invite --users U123,U456` | `POST /conversations.invite` |
| `channel_kick` | action | `channel <id> kick --user <user_id>` | `POST /conversations.kick` |
| `channel_mark` | action | `channel <id> mark --ts <ts>` | `POST /conversations.mark` |

#### Message (chat/search)

| Capability | Kind | CLI | Endpoint |
|------------|------|-----|----------|
| `message_search` | search | `message search --query "..."` | `GET /search.messages` |
| `message_post` | create | `message post --channel <id> --text "..."` | `POST /chat.postMessage` |
| `message_update` | update | `message <ts> update --channel <id> --text "..."` | `POST /chat.update` |
| `message_delete` | delete | `message <ts> delete --channel <id>` | `POST /chat.delete` |
| `message_permalink` | action | `message <ts> permalink --channel <id>` | `GET /chat.getPermalink` |
| `message_react_add` | action | `message <ts> react-add --channel <id> --name thumbsup` | `POST /reactions.add` |
| `message_react_remove` | action | `message <ts> react-remove --channel <id> --name thumbsup` | `POST /reactions.remove` |

#### User

| Capability | Kind | CLI | Endpoint |
|------------|------|-----|----------|
| `user_list` | query | `user query` | `GET /users.list` |
| `user_info` | get | `user <id>` | `GET /users.info` |
| `user_lookup_by_email` | action | `user lookup-by-email --email a@b.com` | `GET /users.lookupByEmail` |
| `user_get_presence` | action | `user <id> get-presence` | `GET /users.getPresence` |
| `user_identity` | singleton | `user identity` | `GET /users.identity` |
| `auth_test` | singleton | `user auth-test` | `POST /auth.test` |

#### File / UserGroup / Team / Reminder

| Capability | Kind | CLI | Endpoint |
|------------|------|-----|----------|
| `file_list` | query | `file query` | `GET /files.list` |
| `file_info` | get | `file <id>` | `GET /files.info` |
| `file_upload` | create | `file create --channels C123 --filename x.txt` | `POST /files.upload` |
| `file_delete` | delete | `file <id> delete` | `POST /files.delete` |
| `usergroup_list` | query | `usergroup query` | `GET /usergroups.list` |
| `usergroup_create` | create | `usergroup create --name "..." --handle "..."` | `POST /usergroups.create` |
| `usergroup_update` | update | `usergroup <id> update` | `POST /usergroups.update` |
| `usergroup_enable` | action | `usergroup <id> enable` | `POST /usergroups.enable` |
| `usergroup_disable` | action | `usergroup <id> disable` | `POST /usergroups.disable` |
| `usergroup_member_list` | query (scoped) | `user usergroup-member-list --usergroup <id>` | `GET /usergroups.users.list` |
| `team_info` | singleton | `team info` | `GET /team.info` |
| `reminder_list` | query | `reminder query` | `GET /reminders.list` |
| `reminder_info` | get | `reminder <id>` | `GET /reminders.info` |
| `reminder_add` | create | `reminder create --text "..." --time "in 10 minutes"` | `POST /reminders.add` |
| `reminder_complete` | action | `reminder <id> complete` | `POST /reminders.complete` |
| `reminder_delete` | delete | `reminder <id> delete` | `POST /reminders.delete` |
| `pin_list` | query (scoped) | `pin query --channel <id>` | `GET /pins.list` |
| `bookmark_list` | query (scoped) | `bookmark query --channel <id>` | `POST /bookmarks.list` |
| `bookmark_add` | create | `bookmark create --channel <id> --title … --type link --link …` | `POST /bookmarks.add` |
| `bookmark_edit` | update | `bookmark <id> update --channel <id> …` | `POST /bookmarks.edit` |
| `bookmark_remove` | action | `bookmark <id> bookmark-remove --channel <id>` | `POST /bookmarks.remove` |
| `scheduledmessage_list` | query (scoped) | `scheduledmessage query --channel <id>` | `POST /chat.scheduledMessages.list` |
| `scheduledmessage_create` | create | `scheduledmessage create --channel <id> --post-at <unix_sec> --text "..."` | `POST /chat.scheduleMessage` |
| `scheduledmessage_delete` | delete | `scheduledmessage <id> delete --channel <id>` | `POST /chat.deleteScheduledMessage` |
| `bot_info` | get | `bot <id>` | `GET /bots.info` |

### CLI examples

```bash
# List all public channels (first page, 200 per page)
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  channel query --types public_channel

# List all channels including private (bot must be a member)
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  channel query --types public_channel,private_channel --all

# Get a specific channel by ID
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  channel C012AB3CDX

# Read recent messages from a channel
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  message channel-history --channel C012AB3CDX --limit 50

# Read all messages since a timestamp
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  message channel-history --channel C012AB3CDX --oldest 1700000000 --all

# Search messages across the workspace
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  message search --query "deployment failed in:engineering"

# Post a message
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  message post --channel C012AB3CDX --text "Hello from Plasm!"

# Reply in a thread
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  message post --channel C012AB3CDX --text "Got it!" \
  --thread_ts 1512085950.000216

# Get thread replies
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  message channel-replies --channel C012AB3CDX --ts 1512085950.000216

# List all users (paginated)
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  user query --all

# Get a specific user
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  user W012A3CDE

# Look up a user by email
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  user lookup-by-email --email alice@example.com

# Check the bot's own identity
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  user auth-test

# Get workspace info
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  team info

# List files in a channel
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  file query --channel C012AB3CDX --types images

# List user groups
plasm-cgs --schema apis/slack --backend https://slack.com/api \
  usergroup query --include_count true

# Navigate channel → messages (via_param traversal, auto-fills channel param)
# plasm REPL: channel C012AB3CDX messages
```

---

## Testing status

### CLI validation

Schema loads without panics. All subcommand names, typed flags, and pagination controls verified.

```bash
cargo run -p plasm --bin plasm-cgs -- --schema apis/slack --help
cargo run -p plasm --bin plasm-cgs -- --schema apis/slack channel --help
cargo run -p plasm --bin plasm-cgs -- --schema apis/slack channel query --help
cargo run -p plasm --bin plasm-cgs -- --schema apis/slack message --help
cargo run -p plasm --bin plasm-cgs -- --schema apis/slack message search --help
cargo run -p plasm --bin plasm-cgs -- --schema apis/slack message channel-history --help
cargo run -p plasm --bin plasm-cgs -- --schema apis/slack user --help
```

CLI outputs verified:
- `channel query` — `--exclude_archived`, `--types`, `--limit`, `--all`, `--cursor`
- `message search` — `--query` (required), `--sort` (score|timestamp), `--sort_dir` (asc|desc), `--highlight`, `--limit`, `--all`, `--page`
- `message channel-history` — `--channel` (required), `--oldest`, `--latest`, `--inclusive`, `--limit`, `--all`, `--cursor`
- `user query` — `--include_locale`, `--limit`, `--all`, `--cursor`

### Against the live Slack API

Not yet tested with live credentials. To test with a Bot Token:

```bash
export SLACK_BOT_TOKEN=xoxb-your-token-here

# Auth check
plasm-cgs --schema apis/slack --backend https://slack.com/api user auth-test

# List channels (requires channels:read scope)
plasm-cgs --schema apis/slack --backend https://slack.com/api channel query

# List users (requires users:read scope)
plasm-cgs --schema apis/slack --backend https://slack.com/api user query --limit 10
```

---

## Runtime features used

**Dot-path `from_response`** — Slack's cursor is nested under `response_metadata.next_cursor`, not at the top level. Plasm's pagination config now supports dot-separated paths for both `from_response` cursor extraction and `stop_when` field checks. This was added as part of this integration.

**Single-object-under-key decoding** — Slack wraps single entities in a named key (`{"channel": {...}}`, `{"user": {...}}`). The runtime's `execute_get` path now checks the CML `response.items_key()` and unwraps single objects before decoding, exactly as it does for one-element collection responses.

---

## What remains to be implemented

### High priority — missing read operations

**`reactions.get`** — Get reactions on a message, file, or file comment. The response shape (`{"ok": true, "type": "message", "message": {"reactions": [...]}}`) requires model-specific decoding.

**`conversations.open`** — Open or resume a DM channel. Returns a channel object with `response.channel.id`. Useful for bot-initiated DMs.

**Emoji list** — `emoji.list` returns a name→URL mapping with no IDs — not suitable for entity decoding as-is. Would need a custom `Emoji` entity with `id_field: name`.

**`users.profile.get`** — Returns the full user profile, including custom fields set by workspace admins. Could enrich User with extra fields via `provides:` pattern.

### Medium priority — write operations

**Thread messages** — `message_post` with `thread_ts` already works. Dedicated `thread_reply_create` with cleaner ergonomics would improve the agentic UX.

**Stars** — `stars.list`, `stars.add`, `stars.remove`. Would need a `Star` entity. Stars apply to messages, files, or channels.

**File sharing** — `files.sharedPublicURL` to make a file public. Separate from `file_upload`.

### Lower priority — admin surface

**Admin APIs** (`admin.conversations.*`, `admin.users.*`, `admin.teams.*`) — require additional OAuth scopes (`admin.*`) and are only available to workspace admins. Large surface area.

**RTM (Real-Time Messaging)** — `rtm.connect` returns a WebSocket URL for event streaming. Not modelled here (no HTTP polling equivalent).

**Block Kit payloads** — See **Ergonomics** above and `domain.yaml` for the declared `blocks` / `attachments` / unfurl / `mrkdwn` surface. For **post**, `body: { type: var, name: input }` merges the full create `input` into one JSON body; the runtime may still forward extra keys if present on `input`. For **update**, CML maps `channel`, `ts`, `text`, `blocks`, and `attachments` explicitly.

---

## Known limitations

**`message_search` response is doubly nested** — Slack returns search results at `{"messages": {"matches": [...], "pagination": {...}}}`. The items array is at `messages.matches`, but the CML `items:` field only supports top-level keys. The mapping currently declares `items: messages` which gives a partially decoded result (the `matches` sub-object, not the individual messages). Search results appear with a non-standard shape. A `response: {items: messages.matches}` dot-path feature would fully fix this.

**`usergroup_member_list` returns IDs only** — `usergroups.users.list` returns `{"users": ["U012", "U013", ...]}` — a bare array of user ID strings, not full User objects. The decoder produces User entities with only `ts` populated. Auto-hydration (if `user_info` is called per-ID) would enrich these, but the initial query result is sparse.

**`auth_test` identity** — `POST /auth.test` returns workspace-level caller identity fields (`user`, `user_id`, `team_id`) at the response root, not wrapped in a `user` key. The mapping uses `response: single` and decodes from root. The fields `user_id` and `user` map to the User entity's `id` field only approximately — `auth_test` is more useful for token validation than full User decoding.

**No `channel_info` in hydration path** — When `channel query` returns summary rows and hydration fires, `channel_info` (GET) correctly unwraps the `{"channel": {...}}` envelope via the new execute_get envelope-key feature. This is verified at the code level but not yet tested against the live API.

**Post body encoding** — Many Slack POST endpoints accept `application/x-www-form-urlencoded` rather than JSON. The CML `body: {type: var, name: input}` sends JSON. The Slack API generally accepts both, but some legacy endpoints may behave differently. Test each write capability if you encounter unexpected errors.

---

## CGS review and backlog

For a structured completeness and ergonomics pass (toolchain results, CGS-first gap matrix, prioritized follow-ups), see [REVIEW.md](REVIEW.md). Validate the toolkit with `cargo run -p plasm-cli --bin plasm -- schema validate apis/slack` (directory, not `domain.yaml` alone). The schema-driven CLI binary is `plasm-cgs` in the `plasm` crate: `cargo run -p plasm --bin plasm-cgs -- --schema apis/slack --help`.
