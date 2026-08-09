# Microsoft Teams (Microsoft Graph) — Plasm CGS

A **wave‑3** [Plasm](../../README.md) domain for **Microsoft Teams** via **Microsoft Graph** `v1.0`: joined teams, channels, channel messages, chats, and chat messages (read and send).

```bash
export MICROSOFT_GRAPH_ACCESS_TOKEN="…"   # OAuth 2.0 access token for Graph (delegated user)
cargo run -p plasm-repl -- \
  --schema apis/microsoft-teams \
  --backend https://graph.microsoft.com
```

## Auth and permissions

- **Scheme:** `bearer_token` → `Authorization: Bearer …` using `MICROSOFT_GRAPH_ACCESS_TOKEN`.
- **Delegated:** `team_query` calls `GET /me/joinedTeams` — register an Azure AD app, add **Microsoft Graph delegated** permissions such as `Team.ReadBasic.All` or broader `Team.ReadBasic.All` / `Group.Read.All` as your tenant policy allows, complete admin consent if required, then obtain a user access token (authorization code or device code flow).
- **Application-only tokens** do not have a `/me` surface; a different CGS slice would use `/teams` filters or roster APIs instead.

## Phased roadmap (relational design)

| Wave | Scope | Notes |
|------|--------|--------|
| **1 (this tree)** | `Team`: `team_query`, `team_get` | Joined teams + detail; `@odata.nextLink` pagination via `response_next_url`. |
| **2 (this tree)** | `Channel`, `Chat`, `ChatMessage` | Channels scoped by `teamId`; chats and chat messages for delegated `/me` reads. |
| **3 (this tree)** | `ChannelMessage`, chat/channel sends | Team channel message read/post; chat message send. |
| **4** | Apps, tabs, membership writes | Side-effect capabilities beyond messaging. |

## Entity graph

- `Team` — joined teams; `channels` relation lists channels when `teamId` scope is known
- `Channel` — team channels (`channel_query` / `channel_get` require `teamId` scope); `messages` relation lists channel posts when `teamId` scope is also supplied at invoke time
- `ChannelMessage` — posts in a channel (`channel_message_query` / `channel_message_get` / `channel_message_send` require `teamId` + `channelId` scope)
- `Chat` — `/me/chats` conversations; `messages` relation lists chat messages
- `ChatMessage` — messages in a chat (`chat_message_query` / `chat_message_get` / `chat_message_send` require `chatId` scope)

**Channel scope note:** Graph channel and channel-message JSON does not include parent `teamId` on each row. Channel and channel-message capabilities always require explicit `teamId` scope (and `channelId` for messages). The `Channel.messages` relation binds `channelId` only — you must still pass `teamId` when listing or posting.

## Known limitations

Microsoft Graph returns **`@odata.nextLink`** (absolute URL) for collection continuations. **`team_query`** declares `pagination.location: response_next_url` so agents can follow additional pages with `page(pg1)` when the first response includes a next link. The first page pins `$top=100`.

## Validation

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/microsoft-teams
cargo run -p plasm-repl -- --schema apis/microsoft-teams --backend https://graph.microsoft.com --help
cargo run -p plasm-eval -- coverage --schema apis/microsoft-teams --cases apis/microsoft-teams/eval/cases.yaml
```
