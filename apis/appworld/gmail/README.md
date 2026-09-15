# AppWorld gmail — mail simulation.

Task-critical surface: inbox/outbox/archived/spam thread search (unified `email_thread_query` + `mailbox`), thread mutators (read/star/archive/spam/label/snooze), send/reply/forward, drafts (+ attachments), profile/account, attachment download. Auth taught via `login`.

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/gmail
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/appworld/gmail/openapi.json apis/appworld/gmail
```

OpenAPI: `openapi.json`. Backend: `http://127.0.0.1:9000` after `appworld serve apis`.

## Scope notes

- Category list endpoints are one capability: `email_thread_query` with required `mailbox` ∈ {inbox, outbox, archived, spam} (no OpenAPI `category/snoozed` path — use `email_thread_snooze` / `email_thread_unsnooze` mutators).
- Account signup/update/delete and password-reset are mapped; public profile remains `profile_get`.
- Spam/label/forward/attachment download are first-class.
