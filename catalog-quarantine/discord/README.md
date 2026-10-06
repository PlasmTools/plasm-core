# Discord HTTP API (v10)

Curated Plasm CGS for Discord’s **REST** surface (`https://discord.com/api/v10`). This tree is **hand-authored** from Discord’s published OpenAPI ([discord-api-spec](https://github.com/discord/discord-api-spec)); it is not a mechanical export.

## Auth

Set **`DISCORD_BOT_TOKEN`** to your bot token **including** the `Bot ` prefix, for example:

```bash
export DISCORD_BOT_TOKEN='Bot YOUR_TOKEN_HERE'
```

The schema uses `api_key_header` on `Authorization`; Plasm sends the env value verbatim (Discord expects `Bot …`, not `Bearer …`, for bot tokens).

OAuth2 user access tokens are **not** modeled in this schema version; add a separate auth profile later if you need `Bearer` user tokens.

## Run

```bash
cargo run -p plasm-repl -- --schema apis/discord --backend https://discord.com/api/v10
```

Override the origin with `--backend` when using a proxy or future API version.

## Validate

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/discord
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/discord/openapi.json apis/discord
cargo run -p plasm-repl -- --schema apis/discord --help
```

**Replies:** `message_reply` (action on a `Message` row) POSTs to `channels/{channel_id}/messages` with a `message_reference` to the target; runtime **`preflight`** runs **`message_get`** first and merges **`parent_*`** into the CML env for the reference payload.

## Non-goals (v1)

- **Gateway WebSocket** lifecycle (identify, heartbeats, dispatch) — REST only here.
- **Multipart-first** endpoints (e.g. attachment-only application upload, some webhook execute flows) until represented with honest CML.
- **Binary** responses such as guild widget PNG.
- Full **Partner SDK** and **Lobby** trees — deferred or stubbed in README until modeled end-to-end.

## OpenAPI reference

[`openapi.json`](openapi.json) in this directory is a pinned copy of Discord’s spec for `plasm-cli validate` and local tooling; CGS semantics remain authoritative in `domain.yaml`.
