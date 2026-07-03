# Publish MCP proof entries to Proof

After composing proof markdown (see `mcp-proof-format`), **append to the live Proof document** via Plasm — not chat-only output.

## Sequence (same `logical_session_ref`)

1. `document_share_bind` — once per doc if not already bound (`share_url` or `share_token`).
2. `presence_update` — agent_id e.g. `mcp-radar`.
3. `editor_state_get` — host caches `base_token` for edits.
4. `block_query` — list blocks; pick last block ref for append.
5. `document_edit_v2` — `insert_after` with the proof markdown block(s), `base_revision` from editor state, `by` = agent name.

On stale revision, re-run `editor_state_get` + `block_query` before retrying the edit.

Reset runs: replace body with `# MCP Innovations Proof Log` only, then append fresh entries.
