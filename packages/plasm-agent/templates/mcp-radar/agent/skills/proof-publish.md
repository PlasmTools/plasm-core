# Publish MCP proof entries to Proof

After composing proof markdown (see `mcp-proof-format`), **append to the live Proof document** via Plasm — not chat-only output.

**No host env:** do not expect `PROOF_API_TOKEN`, `PROOF_SHARE_URL`, or `PROOF_DOCUMENT_SLUG`. Auth comes from **`document_share_bind`** after the agent creates or reuses a share link.

## First document (same `logical_session_ref`)

1. `share_link_create` — creates the shared doc; response includes **slug** and **share URL**.
2. `document_share_bind` — bind `share_url` (or token) once; host stores session auth for later `plasm_run` calls.
3. `presence_update` — agent_id e.g. `mcp-radar`.
4. `editor_state_get` — host caches `base_token` for edits.
5. `block_query` — list blocks; pick last block ref for append.
6. `document_edit_v2` — `insert_after` with proof markdown, `base_revision` from editor state, `by` = agent name.

## Later runs

Skip create when the session is already bound: `document_get_markdown` for dedupe, then steps 3–6.

On stale revision, re-run `editor_state_get` + `block_query` before retrying the edit.

Reset runs: replace body with `# MCP Innovations Proof Log` only, then append fresh entries.
