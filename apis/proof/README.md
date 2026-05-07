# Proof catalog (Plasm)

This package targets the **public HTTP surface** described in [EveryInc/proof-sdk](https://github.com/EveryInc/proof-sdk) (`AGENT_CONTRACT.md`, `docs/agent-docs.md`): `POST /documents`, `GET /documents/:slug/state`, `GET /documents/:slug/snapshot`, `POST /documents/:slug/edit` and `…/edit/v2`, `POST /documents/:slug/ops`, bridge routes under `…/bridge/*`, and collaboration polling `…/events/pending` / `…/events/ack`.

`mappings.yaml` is generated from `_gen_mappings.py` so paths stay consistent:

```bash
python3 plasm-oss/apis/proof/_gen_mappings.py
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
cargo run -p plasm-agent --bin plasm-mcp -- --schema apis/proof --http --port 3000 --backend http://127.0.0.1:4000
```

Validate the catalog only:

```bash
cargo run -p plasm-cli --bin plasm -- schema validate apis/proof
```

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
TOKEN=…   # optional; maps to Plasm `share_token`
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

- **Optimistic locking:** block-level mutations use Proof SDK **`baseRevision`** (integer) from `GET …/snapshot` — typed in DOMAIN via shared `values.nv_proof_int` on `EditorState.revision`. Structured `POST …/edit` paths that surface **`baseUpdatedAt`** should use the same short-string primitive (`values.nv_proof_str`) when that parameter is modeled on the capability.
- **Share token:** optional capability parameter `share_token` is wired as query **`token`** on requests that support link-style access.
- **Session bind (HTTP/MCP execute):** call **`document_share_bind`** once per document with the share URL (`…/d/{slug}?token=…`) or explicit `share_token`; the host keeps Bearer + mirrored `?token=` in execute session material so later lines stay token-free — see [instance-share-auth.md](../../../docs/instance-share-auth.md).
- **Agent identity:** `agent_id` is sent as **`X-Agent-Id`** on mutating routes.
- **Idempotency:** explicit capability parameter `idempotency_key` is sent as **`Idempotency-Key`** when set. On HTTP/MCP execute, Plasm also injects CML env keys `plasm_execute_prompt_hash` and `plasm_execute_session_id`; the generated **`document_edit_*`** mappings derive a default `Idempotency-Key` from those plus mutation fields (`baseRevision`, refs, text, …) when the caller omits `idempotency_key`. Align with Proof rollout: read `contract.idempotencyRequired` and `contract.mutationStage` from `GET …/state` — during required stages the wire must still carry a key (host-derived or explicit). Same key with a different payload hash yields `IDEMPOTENCY_KEY_REUSED`.
- **`document_edit_find_replace_in_doc`:** CML currently emits a single structured `replace` op; optional sweep fields in the domain are not yet mapped — extend `_gen_mappings.py` when you confirm the live JSON shape.
- **Bug intake:** mappings use **`POST /api/bridge/report_bug`** with **`Accept: application/json`** (hosted `www.proofeditor.ai` and apex `proofeditor.ai`). Older **`POST /report/bug`** returned **404** in production probes — do not revert without re-verifying. **`document_bug_report_submit`** uses the same URL and sends **`slug`** in the JSON body plus optional share **`token`** query when `share_token` is set.
- **`annotation_comment_unresolve`** / **`annotation_comment_batch_apply`** / **`/ops`** shapes remain **best-effort** — verify against your Proof revision if the server returns 4xx.

## Hosted production checks (manual)

Probed **2026-05** with anonymous requests (no doc secrets):

| Check | Result |
| ----- | ------ |
| `GET https://proofeditor.ai/` vs `https://www.proofeditor.ai/` | **200** / **200** |
| `GET …/documents/{slug}/state` vs `…/api/agent/{slug}/state` (unknown slug) | **404** / **404** on both apex and www (same status — aliases behave consistently for missing docs) |
| `POST https://www.proofeditor.ai/report/bug` | **404** |
| `POST https://www.proofeditor.ai/api/bridge/report_bug` | **200** with JSON validation envelope (`needs_more_info` on minimal `{}` body) |

**Presence:** use **`POST /documents/:slug/presence`** (hosted alias **`POST /api/agent/:slug/presence`**) with **`Authorization: Bearer`** + **`X-Agent-Id`** and JSON **`{ "status": "online" }`** (default when `presence_status` is omitted). **`…/bridge/presence`** is for the desktop/SDK bridge — it does **not** substitute for agent join on hosted collab UIs.

## Incremental DOMAIN waves (execute / MCP)

To keep prompts small and monotonic (`e#` / `m#` / `p#`), open sessions with a **tight seed list** and expand in waves ([incremental-domain-prompts.md](../../../docs/incremental-domain-prompts.md)):

1. **Wave 1 — `Document`:** share flows start with **`document_share_bind`**, then reads (`document_get_markdown`, `document_get`), `presence_update`, and lightweight meta (`share_link_create`, bug reports) as needed.
2. **Wave 2 — `EditorState`:** `editor_state_get` for revision / contract / marks before mutating.
3. **Wave 3 — `Block`:** `block_query` + `document_edit_v2` for structural edits.
4. **Wave 4 — `CollaborationEvent`:** `collaboration_event_query` + `collaboration_event_ack` for polling.

Federation and exact seed lists follow host tooling (`plasm_context` seeds, HTTP execute entities).

## Eval cases (form coverage)

```bash
cargo run -p plasm-eval -- coverage --schema apis/proof --cases apis/proof/eval/cases.yaml
```

## Hosted Proof

The default `http_backend` in `domain.yaml` remains `https://www.proofeditor.ai`. Hosted deployments expose **`/api/agent/*`** compatibility aliases in places; this catalog uses **`documents/…`** for reads/edits/collaboration and **`/api/bridge/report_bug`** for product bug intake as verified above.
