# Long-running Plasm operations

Plasm uses two execution surfaces:

- **MCP:** `plasm` compiles and dry-runs a program. **Provably read-only** plans (plan gate `Proceed`, flow `Clean`, zero remote-mutation nodes) **auto-execute** and return rows in the same tool response. Plans that need review or contain mutations return **`run_ref`** (`pcN`); `plasm_run` executes those via **`run_ref`** only and awaits server-side.
- **HTTP / remote CLI:** live execute can opt into explicit async operation continuations with `wait(oN)` / `cancel(oN)`. After a dry-run, live execute accepts query/body **`plan_commit_ref=pcN`** (HTTP name for the same `pcN` token).

See also [plasm-language-definition.md](plasm-language-definition.md#host-continuations-page-wait-cancel) for surface syntax, [incremental-teaching-prompts.md](incremental-teaching-prompts.md) for how the teaching TSV preamble teaches continuations, and [tool-model-http.md](tool-model-http.md) for tool-model `execute` notes.

## Handles

| Handle | Expression / arg | Resumes |
|--------|------------------|---------|
| `l_<token>_pgN` | MCP: pass handle as **`run_ref`** on `plasm_run` | Paginated query cursor (MCP) |
| `pgN` | `page(pgN)` | Paginated query cursor (HTTP-only execute) |
| `oN` | `wait(oN)` | In-flight async plan execution (HTTP) |
| `oN` | `cancel(oN)` | Cooperative cancel of that operation |
| `pcN` | MCP **`run_ref`**; HTTP query/body **`plan_commit_ref`** | Dry-run plan acceptance token |

**MCP:** `plasm_run` does not accept `program`, `wait`, `cancel`, `force`, `execute`, `plan_commit_ref`, or `page_handle`. It accepts **`run_ref`** — a `pcN` from `plasm` or a page handle from a prior result's "more pages" line — and returns one terminal response. `page(...)` is HTTP-execute program syntax only. Legacy transport slots (`s0`, …) are rejected.

**HTTP execute:** long-op and paging handles are **plain** `oN` / `pgN` on the same `/execute/:prompt_hash/:session` row — no MCP `plasm_context` required for wait/cancel continuations.

## Plan commit tokens (`pcN`)

Dry-run mints a **`pcN`** acceptance token (`pc0`, `pc1`, …) tied to a **content-addressed commit id** over the **semantic plan DAG** only:

- Hashed fields: `version`, `steps`, `bind`, `return`
- **Excluded** (session-local / presentation): plan `name` (e.g. `plasm_dag_call_{n}`), dry-run `summary`

Wire names:

- **MCP:** `_meta.plasm.run_ref` / tool arg **`run_ref`**
- **HTTP:** query or JSON body **`plan_commit_ref`**

The same program therefore yields the same `pcN` acceptance on MCP and HTTP even when call counters or summary metadata differ. MCP stores the reviewed comp under `pcN`; `plasm_run` consumes that token via **`run_ref`** rather than re-accepting a program echo.

Tokens expire after **10 minutes** (`PLAN_COMMIT_TTL`). Re-run plan dry-run after expiry or program change.

## Agent workflow (MCP)

1. **`plasm`** — pass `logical_session_ref` + `program`.
   - **Clean reads:** when the plan gate is `Proceed`, flow is `Clean`, and the DAG has **no** remote mutations, the host fuses dry-run + live execute and returns rows (same shape as a successful `plasm_run`).
   - **Writes / review:** otherwise the response returns **`run_ref`** (`pc0`, …) and `dry_review` / `dry_verdict` in `_meta.plasm` — do **not** call `plasm_run` for clean reads.
2. **`plasm_run`** — live execute for reviewed writes / paging; pass `logical_session_ref` + **`run_ref`** only. Do not echo the program. The server awaits expensive work internally and returns one terminal response (progress via `notifications/plasm/op`).
3. **`resources/read`** — full run snapshots when Markdown summarizes away fields (`plasm://execute/{ph}/{sid}/run/pr{64hex}` or MCP short `plasm://session/{logical_session_ref}/run/pr{64hex}`).

### Examples

```text
# Clean read — one call
plasm      logical_session_ref=l_AAAAAAAAQACAAAAAAAAAAQ  program=Pokemon.filter{base_experience >= 300}
→ terminal rows/table (inline when ≤25)

# Write / review — two calls
plasm      logical_session_ref=l_AAAAAAAAQACAAAAAAAAAAQ  program=Issue.create(…)
→ dry plan · run_ref `pc0`

plasm_run  logical_session_ref=l_AAAAAAAAQACAAAAAAAAAAQ  run_ref=pc0
→ terminal rows/table or resource_link snapshots
```

## HTTP execute

`POST /execute/:prompt_hash/:session` accepts the same program strings (`wait(…)`, `cancel(…)`).

Query parameters:

| Param | Default | Role |
|-------|---------|------|
| `mode=plan` | — | Plan dry-run only (no live HTTP). Mints `pcN` in `_meta.plasm` (HTTP field `plan_commit_ref`). |
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

**Poll:** HTTP `POST` with `wait(…)` — `_meta.plasm.op` uses short keys (`n`, `~`, `s`, `l`, `r`).

**Push (optional):**

- HTTP SSE: `GET /execute/{prompt_hash}/{session}/operations/{handle}/stream` — `data` is the plain wire line (`snapshot` / `progress` / `terminal` events).
- MCP: `notifications/plasm/op` with `{ "line", "n" }` (optional `"c"` on accept).

## Handle discipline

When a response includes **`+`**, **`~`**, or **`=`** on an operation handle, that handle is **open**:

1. Poll with HTTP **`POST`** body `wait(h)` every **3–5s** until **`!`** (done), **`x`** (cancelled), or **`?`** (failed).
2. Or cooperative **`cancel(h)`** when abandoning the run.
3. **Do not** start unrelated live programs or tell the user the task is finished while handles you opened are still open — unless you explicitly say the run is still in progress and keep polling.

**MCP does not dispatch `wait(h)` / `cancel(h)` through `plasm_run`**. Use HTTP execute / remote CLI for explicit operation continuations.

## Concurrent operations

Each HTTP async live program mints its own handle (`o1`, `o2`, …). **Parallel async runs are allowed** on the same execute session — poll **each** handle independently.

**Cap:** `PLASM_MAX_RUNNING_OPS_PER_SESSION` (default **16**). When the cap is reached, the host returns **`too_many_operations`** listing outstanding handles — **wait or cancel** those before starting more. Only **pod-local live executors** count toward the cap; rehydrated Running stubs on a foreign replica do not.

## Cross-pod HTTP async operations (Redis-backed)

When `PLASM_MCP_TRANSPORT_REDIS_URL` is configured, the host persists **thin operation descriptors** in the existing execute session descriptor JSON (phase, coalesced progress, terminal `run_artifact_id`). **Tokio tasks, cancel signals, and graph state stay pod-local.** This is an HTTP / remote CLI continuation surface; MCP `plasm_run` awaits internally.

| Situation | `wait(oN)` on another replica | `cancel(oN)` on another replica |
|-----------|------------------------------|--------------------------------|
| **Running** (executor on pod A) | Returns compact `~` progress; `_meta.plasm.code` = **`operation_not_on_replica`** (keep polling) | **`operation_not_on_replica`** error (400) |
| **Succeeded** | Hydrates rows from shared **`PLASM_RUN_ARTIFACTS_URL`** / in-memory store via stored `pr…` id | N/A (already terminal) |
| **Unknown handle** | **`unknown_operation_handle`** | same |

Terminal ops store **`run_artifact_id`** only in Redis (not inline `PlasmPlanRunResult`). Progress patches are coalesced (~2s) to bound write volume. At most **16** live Running ops per session and **32** terminal op rows retained in the descriptor.

**Smoke:** `scripts/smoke/mcp-multireplica-execute-live.sh` (async accept + cross-transport `wait(h)`).

## Internal observability

Trace hub SSE remains for operator timeline detail — separate from the compact agent lines above.

## Tests

- Dual-surface E2E: `cargo test -p plasm-e2e --test long_operation_e2e`
- HTTP oneshot smokes: `cargo test -p plasm-agent-core --test long_operation_http`
- Push E2E (SSE + MCP): `cargo test -p plasm-e2e --test operation_progress_push_e2e`
- Coalesce integration: `cargo test -p plasm-agent-core coalesce`
- Commit-id + hash perf guard: `cargo test -p plasm-agent-core plan_commit_semantic_dag_hash_benchmark`
- Multi-replica smoke: `scripts/smoke/mcp-multireplica-execute-live.sh`
