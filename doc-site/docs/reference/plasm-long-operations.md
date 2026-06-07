# Long-running Plasm operations

Plasm uses the same **continuation model as paging**: opaque handles and program expressions — no extra MCP tools.

See also [plasm-language-definition.md](plasm-language-definition.md#host-continuations-page-wait-cancel) for surface syntax, [incremental-teaching-prompts.md](incremental-teaching-prompts.md) for how the teaching TSV preamble teaches continuations, and [tool-model-http.md](tool-model-http.md) for Phoenix Tool Explorer `execute` notes.

## Handles

| Handle | Expression | Resumes |
|--------|------------|---------|
| `s0_pgN` | `page(s0_pgN)` | Paginated query cursor (more rows) |
| `s0_oN` | `wait(s0_oN)` | In-flight async plan execution |
| `s0_oN` | `cancel(s0_oN)` | Cooperative cancel of that operation |
| `pcN` | (tool/query arg) | Dry-run plan acceptance token |

MCP names handles with the logical session prefix (`s0_o1`, `s0_pg2`). HTTP execute uses the same **`s0_oN`** slot when no MCP `plasm_context` is present (synthetic logical session `s0`).

## Plan commit tokens (`pcN`)

Dry-run mints a **`plan_commit_ref`** (`pc0`, `pc1`, …) tied to a **content-addressed commit id** over the **semantic plan DAG** only:

- Hashed fields: `version`, `nodes`, `edges`, `topological_order`, `returns`
- **Excluded** (session-local / presentation): plan `name` (e.g. `plasm_dag_call_{n}`), dry-run `summary`

The same program therefore yields the same `pcN` acceptance on MCP and HTTP even when call counters or summary metadata differ.

Tokens expire after **10 minutes** (`PLAN_COMMIT_TTL`). Re-run plan dry-run after expiry or program change.

## Agent workflow (MCP)

1. **`plasm`** — dry-run; returns `plan_commit_ref` (`pc0`, …) and `dry_review` / `dry_verdict` in `_meta.plasm` when the plan needs review.
2. **`plasm_run`** — live execute (blocking by default).
   - On **`review`** verdict: blocked unless `plan_commit_ref` matches the current program or `force: true`.
   - With **`wait: false`**: returns immediately with `wait(s0_oN)`; poll via `plasm_run` + `wait(s0_oN)`; cancel via `cancel(s0_oN)`.
3. **`resources/read`** — full run snapshots when Markdown summarizes away fields.

### Examples

```text
plasm_run  program=Pokemon.filter{base_experience >= 300}  wait=false  force=true
→ wait(s0_o1) · verdict review · plan `pc0`

plasm_run  program=wait(s0_o1)
→ step 3/8 · …  (while running) or final results (when done)

plasm_run  program=cancel(s0_o1)
→ cancelled · partial snapshots via resources/read
```

## HTTP execute

`POST /execute/:prompt_hash/:session` accepts the same program strings (`wait(…)`, `cancel(…)`).

Query parameters (also available on MCP `plasm` / `plasm_run` tool args):

| Param | Default | Role |
|-------|---------|------|
| `mode=plan` | — | Plan dry-run only (no live HTTP). Mints `plan_commit_ref` in `_meta.plasm` like MCP `plasm`. |
| `wait=false` | `true` | Start live execute in background; response is `wait(s0_oN)` accept Markdown. |
| `force=true` | `false` | Bypass **review** soft gate without `plan_commit_ref`. |
| `plan_commit_ref=pcN` | — | Accept a matching dry-run plan after **review** verdict. |

JSON body alternative: `{"program": "…", "wait": false, "force": true, "plan_commit_ref": "pc0"}`.

HTTP binds operation handles under logical session **`s0`** (`s0_o1`, …) via trace context — no MCP `plasm_context` required for wait/cancel continuations on the same execute session.

## CLI (`plasm run`)

```bash
plasm run --mode plan -e 'Pokemon.filter{base_experience >= 300}'
plasm run --wait=false --force -e 'Pokemon.filter{base_experience >= 300}'
plasm run -e 'wait(s0_o1)'
plasm run -e 'cancel(s0_o1)'
```

## Internal observability

Trace hub SSE and internal HTTP operation APIs are for Phoenix/SRE only — not exposed in agent-facing Markdown or `_meta.plasm`.

## Tests

- Dual-surface E2E: `cargo test -p plasm-e2e --test long_operation_e2e`
- HTTP oneshot smokes: `cargo test -p plasm-agent-core --test long_operation_http`
- Commit-id + hash perf guard: `cargo test -p plasm-agent-core plan_commit_semantic_dag_hash_benchmark`
