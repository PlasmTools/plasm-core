# Outlook (Microsoft Graph) — Plasm CGS

Wave 3 adds **`Calendar`** and **`Event`** entities (list/get/create/delete), plus Teams **`ChannelMessage`** read/post and **`chat_message_send`**.

Wave 2 added **`message_create`**, **`views.mailbox_triage`**, and **`query_scoped_bindings`** on folder/message relations.

- <https://learn.microsoft.com/en-us/graph/api/user-list-calendars?view=graph-rest-1.0>
- <https://learn.microsoft.com/en-us/graph/api/user-list-events?view=graph-rest-1.0>
- <https://learn.microsoft.com/en-us/graph/api/event-create?view=graph-rest-1.0>
- <https://learn.microsoft.com/en-us/graph/api/event-delete?view=graph-rest-1.0>

Docs consulted (mail):
- <https://learn.microsoft.com/en-us/graph/api/user-list-mailfolders?view=graph-rest-1.0&tabs=http>
- <https://learn.microsoft.com/en-us/graph/api/mailfolder-list-childfolders?view=graph-rest-1.0&tabs=http>
- <https://learn.microsoft.com/en-us/graph/api/mailfolder-get?view=graph-rest-1.0&tabs=http>
- <https://learn.microsoft.com/en-us/graph/api/user-list-messages?view=graph-rest-1.0&tabs=http>
- <https://learn.microsoft.com/en-us/graph/api/mailfolder-list-messages?view=graph-rest-1.0&tabs=http>
- <https://learn.microsoft.com/en-us/graph/api/message-get?view=graph-rest-1.0&tabs=http>
- <https://learn.microsoft.com/en-us/graph/api/message-update?view=graph-rest-1.0&tabs=http>
- <https://learn.microsoft.com/en-us/graph/api/message-move?view=graph-rest-1.0>
- <https://learn.microsoft.com/en-us/graph/api/message-send?view=graph-rest-1.0>
- <https://learn.microsoft.com/en-us/graph/api/message-delete?view=graph-rest-1.0&tabs=http>
- <https://learn.microsoft.com/en-us/graph/api/message-list-attachments?view=graph-rest-1.0&tabs=http>
- <https://learn.microsoft.com/en-us/graph/api/attachment-get?view=graph-rest-1.0&tabs=http>
- <https://learn.microsoft.com/en-us/graph/permissions-reference>

## Auth and permissions

- Scheme: `bearer_token` via `MICROSOFT_GRAPH_ACCESS_TOKEN`.
- Flow assumption: delegated Microsoft Graph user token for the signed-in mailbox. This catalog intentionally uses `/me/...` paths, so application-only client-credentials tokens are not the default wave-1 path.
- Practical least privilege:
  - Read-only navigation: `Mail.ReadBasic` or `Mail.Read`
  - Full message bodies and attachment content: `Mail.Read`
  - Message state changes, moves, draft send, delete: `Mail.ReadWrite`
  - Calendar read: `Calendars.Read` or `Calendars.ReadWrite`
  - Calendar create/delete: `Calendars.ReadWrite`
- If you later want an application-only mailbox catalog, author a separate `/users/{id}`-scoped CGS slice rather than overloading this one.

```bash
export MICROSOFT_GRAPH_ACCESS_TOKEN="..."
cargo run -p plasm-repl -- \
  --schema apis/outlook \
  --backend https://graph.microsoft.com
```

## Entity graph

- `MailFolder`
  - root folders via `mail_folder_query`
  - child folders via `mail_folder_child_query`
  - messages via `message_folder_query`
- `MailboxTriage`
  - composed triage via `mailbox_triage_query` / `mailbox_triage_get` (view over root folders)
- `Message`
  - mailbox-wide messages via `message_query`
  - draft creation via `message_create`
  - detail via `message_get`
  - attachments via `attachment_query`
  - triage writes via `message_update`, `message_move`, `message_send`, `message_delete`
- `Attachment`
  - attachment metadata via `attachment_query`
  - attachment content via `attachment_get`
- `Calendar`
  - list via `calendar_query`
  - detail via `calendar_get`
  - events via `event_calendar_query`
- `Event`
  - mailbox-wide list via `event_query`
  - detail via `event_get`
  - create via `event_create`
  - delete via `event_delete`

## Known limitations

- Mail **delta sync** (`/messages/delta`, `@odata.deltaLink` token carry-forward) is not modeled — requires session-held delta tokens and tombstone decode (future core/catalog work).

- List capabilities paginate via Microsoft Graph `@odata.nextLink` using CML `pagination.location: response_next_url`. The default query returns the first page (`$top=100`); use `page(pg1)` (or a postfix limit) to follow additional pages when the service returns a next link.
- `Attachment` detail still requires an explicit `messageId` scope parameter on `attachment_get`. The relation `Message.attachments` lists attachment rows cleanly, but the current runtime does not automatically carry the parent message identity into a later attachment detail call.

## Validation note

This tree was validated with:

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/outlook
cargo run -p plasm-eval -- coverage --schema apis/outlook --cases apis/outlook/eval/cases.yaml
```

`cargo run -p plasm-cli --bin plasm-cgs -- validate --spec ...` apis/outlook was not run in this change because no minimal, maintainable Microsoft Graph OpenAPI slice is checked into the repo yet. A full Graph description would be disproportionately large and noisy for this wave; schema validation plus eval coverage were the clean validation path for today.
