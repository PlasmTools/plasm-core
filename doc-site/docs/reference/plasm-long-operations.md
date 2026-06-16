# Long-running Plasm operations

Plasm uses the same **continuation model as paging**: opaque handles and program expressions — no extra MCP tools.

See also [plasm-language-definition.md](plasm-language-definition.md#host-continuations-page-wait-cancel) for surface syntax, [incremental-teaching-prompts.md](incremental-teaching-prompts.md) for how the teaching TSV preamble teaches continuations, and [tool-model-http.md](tool-model-http.md) for Phoenix Tool Explorer `execute` notes.

## Handles

| Handle | Expression | Resumes |
|--------|------------|---------|
| `l_<token>_pgN` | `page(l_<token>_pgN)` | Paginated query cursor (MCP) |
| `pgN` | `page(pgN)` | Paginated query cursor (HTTP-only execute) |
| `l_<token>_oN` | `wait(l_<token>_oN)` | In-flight async plan execution (MCP) |
| `oN` | `wait(oN)` | In-flight async plan execution (HTTP) |
| `l_<token>_oN` / `oN` | `cancel(…)` | Cooperative cancel of that operation |
| `pcN` | (tool/query arg) | Dry-run plan acceptance token |

**MCP:** handles are namespaced with the stateless wire ref from `plasm_context` — e.g. `l_AAAAAAAAQACAAAAAAAAAAQ_o1`, `l_AAAAAAAAQACAAAAAAAAAAQ_pg2`. Legacy transport slots (`s0`, …) are rejected.

**HTTP execute:** long-op and paging handles are **plain** `oN` / `pgN` on the same `/execute/:prompt_hash/:session` row — no MCP `plasm_context` required for wait/cancel continuations.

## Plan commit tokens (`pcN`)

Dry-run mints a **`plan_commit_ref`** (`pc0`, `pc1`, …) tied to a **content-addressed commit id** over the **semantic plan DAG** only:

- Hashed fields: `version`, `nodes`, `edges`, `topological_order`, `returns`
- **Excluded** (session-local / presentation): plan `name` (e.g. `plasm_dag_call_{n}`), dry-run `summary`

The same program therefore yields the same `pcN` acceptance on MCP and HTTP even when call counters or summary metadata differ.

Tokens expire after **10 minutes** (`PLAN_COMMIT_TTL`). Re-run plan dry-run after expiry or program change.

## Agent workflow (MCP)

1. **`plasm`** — dry-run; returns `plan_commit_ref` (`pc0`, …) and `dry_review` / `dry_verdict` in `_meta.plasm` when the plan needs review.
2. **`plasm_run`** — live execute.
   - On **`review`** verdict: blocked unless `plan_commit_ref` matches the current program or `force: true`.
   - **Review/unbounded plans auto-async** when `wait` is omitted — returns `wait(l_<token>_oN)` immediately; poll via `plasm_run` + `wait(…)`; cancel via `cancel(…)`.
   - **Expensive** plans (unbounded paginated reads, relation fanout, mutating for_each) auto-async on default `wait`. Advisory review alone (e.g. get + project, unused seeds) stays **sync**.
   - With **`wait: false`**: same async accept on any plan.
   - Bounded **ok** verdict plans with default `wait` remain synchronous (fast path).
3. **`resources/read`** — full run snapshots when Markdown summarizes away fields.

### Examples

```text
plasm_run  program=Pokemon.filter{base_experience >= 300}  wait=false  force=true
→ wait(l_AAAAAAAAQACAAAAAAAAAAQ_o1) · verdict review · plan `pc0`

plasm_run  program=wait(l_AAAAAAAAQACAAAAAAAAAAQ_o1)
→ step 3/8 · …  (while running) or final results (when done)

plasm_run  program=cancel(l_AAAAAAAAQACAAAAAAAAAAQ_o1)
→ cancelled · partial snapshots via resources/read
```

## HTTP execute

`POST /execute/:prompt_hash/:session` accepts the same program strings (`wait(…)`, `cancel(…)`).

Query parameters (also available on MCP `plasm` / `plasm_run` tool args):

| Param | Default | Role |
|-------|---------|------|
| `mode=plan` | — | Plan dry-run only (no live HTTP). Mints `plan_commit_ref` in `_meta.plasm` like MCP `plasm`. |
| `wait=false` | `true` | Start live execute in background; response is `wait(oN)` accept Markdown. |
| `force=true` | `false` | Bypass **review** soft gate without `plan_commit_ref`. |
| `plan_commit_ref=pcN` | — | Accept a matching dry-run plan after **review** verdict. |

JSON body alternative: `{"program": "…", "wait": false, "force": true, "plan_commit_ref": "pc0"}`.

HTTP mints plain **`oN`** / **`pgN`** handles on the execute session — no `logical_session_ref` required for wait/cancel continuations.

## CLI (`plasm run`)

```bash
plasm run --mode plan -e 'Pokemon.filter{base_experience >= 300}'
plasm run --wait=false --force -e 'Pokemon.filter{base_experience >= 300}'
plasm run -e 'wait(o1)'
plasm run -e 'cancel(o1)'
```

## Agent-facing progress (poll + push)

Compact **one-line** updates — not repeated poll/cancel instructions:

| Sig | Meaning |
|-----|---------|
| `+` | accept / started |
| `~` | running (coalesced; row updates at most every ~2s per step) |
| `=` | unchanged — poll again later (3–5s recommended); includes step/rows when progress advanced |
| `!` | succeeded |
| `x` | cancelled |
| `?` | failed |

**Poll:** `wait(…)` / HTTP `POST` with the same program — `_meta.plasm.op` uses short keys (`n`, `~`, `s`, `l`, `r`).

**Push (optional):**

- HTTP SSE: `GET /execute/{prompt_hash}/{session}/operations/{handle}/stream` — `data` is the plain wire line (`snapshot` / `progress` / `terminal` events).
- MCP: `notifications/plasm/op` with `{ "line", "n" }` (optional `"c"` on accept).

## Handle discipline

When a response includes **`+`**, **`~`**, or **`=`** on an operation handle, that handle is **open**:

1. Poll with **`plasm_run program=wait(h)`** (MCP) or **`POST`** body `wait(h)` (HTTP) every **3–5s** until **`!`** (done), **`x`** (cancelled), or **`?`** (failed).
2. Or cooperative **`cancel(h)`** when abandoning the run.
3. **Do not** start unrelated live programs or tell the user the task is finished while handles you opened are still open — unless you explicitly say the run is still in progress and keep polling.

**`plasm` (dry-run) does not dispatch `wait(h)`** — continuations are **`plasm_run`** (or HTTP execute) only.

## Concurrent operations

Each async live program mints its own handle (`l_<token>_o1`, `_o2`, …). **Parallel async runs are allowed** on the same execute session — poll **each** handle independently.

**Cap:** `PLASM_MAX_RUNNING_OPS_PER_SESSION` (default **16**). When the cap is reached, the host returns **`too_many_operations`** listing outstanding handles — **wait or cancel** those before starting more. Only **pod-local live executors** count toward the cap; rehydrated Running stubs on a foreign replica do not.

## Cross-pod async operations (Redis-backed)

When `PLASM_MCP_TRANSPORT_REDIS_URL` is configured, the host persists **thin operation descriptors** in the existing execute session descriptor JSON (phase, coalesced progress, terminal `run_artifact_id`). **Tokio tasks, cancel signals, and graph state stay pod-local.**

| Situation | `wait(h)` on another replica | `cancel(h)` on another replica |
|-----------|------------------------------|--------------------------------|
| **Running** (executor on pod A) | Returns compact `~` progress; `_meta.plasm.code` = **`operation_not_on_replica`** (keep polling) | **`operation_not_on_replica`** error (400) |
| **Succeeded** | Hydrates rows from shared **`PLASM_RUN_ARTIFACTS_URL`** / in-memory store via stored `pr…` id | N/A (already terminal) |
| **Unknown handle** | **`unknown_operation_handle`** | same |

Terminal ops store **`run_artifact_id`** only in Redis (not inline `PlasmPlanRunResult`). Progress patches are coalesced (~2s) to bound write volume. At most **16** live Running ops per session and **32** terminal op rows retained in the descriptor.

**Smoke:** `scripts/smoke/mcp-multireplica-execute-live.sh` (async accept + cross-transport `wait(h)`).

## Internal observability

Trace hub SSE remains for Phoenix/SRE timeline detail — separate from the compact agent lines above.

## Tests

- Dual-surface E2E: `cargo test -p plasm-e2e --test long_operation_e2e`
- HTTP oneshot smokes: `cargo test -p plasm-agent-core --test long_operation_http`
- Push E2E (SSE + MCP): `cargo test -p plasm-e2e --test operation_progress_push_e2e`
- Coalesce integration: `cargo test -p plasm-agent-core coalesce`
- Commit-id + hash perf guard: `cargo test -p plasm-agent-core plan_commit_semantic_dag_hash_benchmark`
- Multi-replica smoke: `scripts/smoke/mcp-multireplica-execute-live.sh`
