# Proof catalog (Plasm)

This package targets the **Proof agent HTTP surface** described in [EveryInc/proof-sdk](https://github.com/EveryInc/proof-sdk) (`AGENT_CONTRACT.md`, `docs/agent-docs.md`). The SDK documents many routes under `/documents/:slug/…`; **hosted `www.proofeditor.ai` serves those agent operations under `/api/agent/:slug/…`** (plain `/documents/:slug/…` often returns **404** HTML). Share flows still use **`GET /d/:slug`** (JSON / markdown); **`POST /share/markdown`** creates a new shared doc. **`POST /api/bridge/report_bug`** is product bug intake.

Author and maintain **`mappings.yaml`** directly (paths, query, headers, body CML). Validate after edits:

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/proof
```

## Local Proof SDK

From a clone of proof-sdk:

```bash
npm install
npm run serve   # API default http://127.0.0.1:4000 (see proof-sdk README)
```

Point Plasm at that origin (overrides `domain.yaml` `http_backend` for the session):

```bash
export PROOF_API_TOKEN=   # if PROOF_SHARE_MARKDOWN_AUTH_MODE=api_key
cargo run -p plasm --bin plasm-mcp -- --schema apis/proof --http --port 3000 --backend http://127.0.0.1:4000
```

If your local SDK does not mirror **`/api/agent/:slug/*`**, adjust **`mappings.yaml`** paths for that backend (hosted **`www.proofeditor.ai`** is the default `http_backend` target).

## curl smoke tests

Create a document (JSON):

```bash
curl -sS -X POST http://127.0.0.1:4000/documents \
  -H 'Content-Type: application/json' \
  -d '{"markdown":"# Hello\n\nfrom curl.","title":"smoke"}' | jq .
```

Share preview `GET /d/:slug` (`document_get` and `document_get_markdown` both use **`Accept: application/json`** in Plasm mappings so responses decode as JSON rows; query `token` when using link access):

```bash
SLUG=…
TOKEN=…   # optional for this direct API example; configure Plasm credentials through host injection
curl -sS -H 'Accept: application/json' "http://127.0.0.1:4000/d/$SLUG?token=$TOKEN" | jq .
```

Optional SDK-only raw body (not used by the Plasm catalog):

```bash
curl -sS -H 'Accept: text/markdown' "http://127.0.0.1:4000/d/$SLUG?token=$TOKEN"
```

Agent state + snapshot (canonical `documents` tree):

```bash
curl -sS -H "Authorization: Bearer $PROOF_API_TOKEN" -H 'Accept: application/json' \
  "http://127.0.0.1:4000/documents/$SLUG/state" | jq .
curl -sS -H "Authorization: Bearer $PROOF_API_TOKEN" -H 'Accept: application/json' \
  "http://127.0.0.1:4000/documents/$SLUG/snapshot" | jq .
```

## Domain ↔ SDK notes

- **Optimistic locking:** block-level mutations use Proof SDK **`baseRevision`** (integer) from `GET …/snapshot` — typed in teaching table via shared `values.nv_proof_int` on `EditorState.revision`. Structured `POST …/edit` paths that surface **`baseUpdatedAt`** should use the same short-string primitive (`values.nv_proof_str`) when that parameter is modeled on the capability.
- **Scoped access:** `document_share_bind` creates a `DocumentAccess` receipt from the explicit document identity and the configured host injection source. Pass its `reference` as the ordinary `access` input. Omit `access` only to use configured host authentication.
- **Local binding:** the catalog declares a reviewed `credential_bind` effect. It makes no HTTP request and returns one receipt row. References are immutable and scoped to session, catalog revision, origin, slot and document; they expire after the declared 24-hour lifetime.
- **`baseToken` / `/ops`:** read `editor_state_get` and explicitly bind its `base_token` into each mutation. The host captures no precondition. After a stale-precondition response, read current state before repairing the write.
- **Agent identity:** `agent_id` is sent as **`X-Agent-Id`** on mutating routes.
- **Idempotency:** explicit capability parameter `idempotency_key` is sent as **`Idempotency-Key`** when set. On HTTP/MCP execute, Plasm also injects CML env keys `plasm_execute_prompt_hash` and `plasm_execute_session_id`; the generated **`document_edit_*`** mappings derive a default `Idempotency-Key` from those plus mutation fields (`baseRevision`, refs, text, …) when the caller omits `idempotency_key`. Align with Proof rollout: read `contract.idempotencyRequired` and `contract.mutationStage` from `GET …/state` — during required stages the wire must still carry a key (host-derived or explicit). Same key with a different payload hash yields `IDEMPOTENCY_KEY_REUSED`.
- **`document_edit_find_replace_in_doc`:** CML currently emits a single structured `replace` op; optional sweep fields in the domain are not yet mapped — extend `mappings.yaml` when you confirm the live JSON shape.
- **Bug intake:** mappings use **`POST /api/bridge/report_bug`** with **`Accept: application/json`** (hosted `www.proofeditor.ai` and apex `proofeditor.ai`). Older **`POST /report/bug`** returned **404** in production probes — do not revert without re-verifying. **`bug_report_submit`** and **`document_bug_report_submit`** require a **`report`** field in the JSON body (markdown-sized bug text); **`document_bug_report_submit`** also sends **`slug`** with explicit scoped **`access`** or configured host authentication — do not create a throwaway document solely to carry the report; pass **`report=`** on the invoke line.
- **Suggestions (hosted `www.proofeditor.ai`):** use **`POST /api/agent/:slug/ops`** with **`type: suggestion.add`** / **`suggestion.accept`** / **`suggestion.reject`** (see proof-sdk `docs/agent-docs.md`). **`POST …/bridge/suggestions`** and **`…/bridge/marks/{accept,reject}`** return **404** on production probes — the Plasm mappings follow **`/ops`**. Capabilities **`annotation_suggestion_insert`**, **`annotation_suggestion_delete`**, and **`annotation_suggestion_replace`** use normal **`agent_id=` / `by=` / `quote=` / …** dotted-call args (no root union payload).
- **Comments (hosted):** **`annotation_comment_add`**, **`annotation_comment_reply`**, and **`annotation_comment_resolve`** use **`POST /api/agent/:slug/ops`** (`comment.add` / `comment.reply` / `comment.resolve`) — not **`…/bridge/comments`**, which hosted rejects for agent slugs. **`collaboration_event_query`** rows decode from **`events/pending`** (`type`, `createdAt`, `data`, `actor`, ack metadata).
- **`annotation_comment_unresolve`** / **`annotation_comment_batch_apply`** / other **`/ops`** shapes remain **best-effort** — verify against your Proof revision if the server returns 4xx.

## Hosted production checks (manual)

Probed **2026-05** with anonymous requests (no doc secrets):

| Check | Result |
| ----- | ------ |
| `GET https://proofeditor.ai/` vs `https://www.proofeditor.ai/` | **200** / **200** |
| `GET …/documents/{slug}/state` vs `…/api/agent/{slug}/state` on **www** (unknown slug, no token) | **404** HTML vs **401/404 JSON** — hosted slug-scoped reads use **`/api/agent/…`** in this catalog |
| `POST https://www.proofeditor.ai/report/bug` | **404** |
| `POST https://www.proofeditor.ai/api/bridge/report_bug` | **200** with JSON validation envelope (`needs_more_info` on minimal `{}` body) |

**Presence:** use **`POST /api/agent/:slug/presence`** with **`Authorization: Bearer`** + **`X-Agent-Id`** and JSON **`{ "status": "online" }`** (default when `presence_status` is omitted). On **`www.proofeditor.ai`**, **`POST /documents/:slug/presence`** returns **404** — Plasm maps **`presence_update`** to **`/api/agent/…`** only. **`…/bridge/presence`** is for the desktop/SDK bridge — it does **not** substitute for agent join on hosted collab UIs.

## Intent selection and incremental teaching

Open an intent-only session for the business operation. Discovery selects business
capabilities and closes declared prerequisites separately. Extend the same session
for later needs; symbols and existing bindings remain append-only.

Mutations that need current document state expose `editor_state_get` through their
declared prerequisites. Bind its `base_token` output explicitly. Configured host
authentication remains independent of business intent; scoped access is an explicit
catalog operation using that injection source.

## Eval cases (form coverage)

```bash
cargo run -p plasm-eval -- coverage --schema apis/proof --cases apis/proof/eval/cases.yaml
```

## Hosted Proof

The default `http_backend` in `domain.yaml` is `https://www.proofeditor.ai`. **`mappings.yaml` uses `/api/agent/:slug/…`** for slug-scoped reads/edits/ops/events/presence (anything that used to hit `/documents/:slug/…` on the local SDK). **`document_get`** and **`document_get_markdown`** use **`GET /d/:slug`**. **`document_share_bind`** is a local credential effect. **`share_link_create`** maps to **`POST /share/markdown`**. Bug intake uses **`/api/bridge/report_bug`** as above.
