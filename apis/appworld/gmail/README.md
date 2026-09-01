# AppWorld gmail — mail simulation.

Task-critical surface: inbox/outbox/archived thread search, thread mutators (read/star/archive), send/reply, drafts, profile. Auth taught via `login`.

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/gmail
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/appworld/gmail/openapi.json apis/appworld/gmail
```

OpenAPI: `openapi.json`. Backend: `http://127.0.0.1:9000` after `appworld serve apis`.

## Scope notes

- Category searches are separate caps (`email_thread_query` = inbox, `email_thread_outbox_query`, `email_thread_archived_query`) — vendor paths differ; not a single `views:` mailbox.
- Inbox/outbox archive workflows use search filters + thread actions; no composed snapshot view required.
- Spam category search (`email_thread_spam_query`), label/spam mutators, forward (`email_forward`, `email_thread_forward`), and attachment download (`attachment_download` on `Attachment`).
