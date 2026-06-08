# Remote Plasm terminal (`plasm`)

The **`plasm`** binary (from the [`plasm`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm/Cargo.toml) crate, `cargo run -p plasm --bin plasm`) is the **remote HTTP terminal** for Plasm: discovery and execution over HTTP (`plasm-mcp` / `plasm-server`). The CLI is **agent-first**: configure once with **`plasm init`**, then **`search` → `context` → `run`**.

*(This document’s filename is historical; the former binary name was `plasm-cgs`.)*

Local schema-driven CLIs are **not** `plasm`; use **`plasm-repl`** (`--schema`), **`plasm-cgs`** from **`plasm-cli`**, or fixture harnesses.

## Client-owned symbol space

The **`plasm` client owns the monotonic `e#` / `m#` / `p#` teaching table**, not the HTTP execute session:

- **`plasm context`** fetches catalog CGS via `GET /v1/registry/{entry_id}?include_cgs=true`, builds a local [`TeachingExposureSession`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/symbol_tuning.rs), and appends teaching rows to **`teaching.tsv`**.
- **`plasm run`** expands programs against that local symbol state, then POSTs the **expanded** surface to the server (lazy server execute binding for auth/HTTP/paging only).
- Catalog digest changes require **`plasm context --new`** (no silent symbol reuse).

There is **no** `primary_api` in the agent-facing model — capabilities are always **`api:Entity`** qualified in summaries and `session_meta.txt`.

See also [incremental-teaching-prompts.md](incremental-teaching-prompts.md) (federated sessions, append-only symbols).

## One-time setup

```bash
plasm init
plasm init --server http://127.0.0.1:3000 --api-key "$PLASM_API_KEY"
plasm init --server https://platform.plasm.tools/plasm/http
plasm login
```

Profile and session state live under **the current working directory**: `.plasm/profiles/default.json` (`server`, optional `api_key` for local/appliance, or `access_token` from device OAuth on `platform.plasm.tools`). Override the workspace root with `PLASM_WORKSPACE` if needed.

## Agent flow

1. **`plasm search "…"`** — MCP-shaped discovery Markdown; **merges** rows into `hosts/<slug>/discovery.tsv` by `(api, entity)`; when a session is active, appends **`out/NNNN-search/`** under that session.
2. **`plasm context -i "…" pokeapi:Pokemon pokeapi:Move`** — client symbol exposure; prints the **symbol wave** (teaching TSV) on stdout; appends **`teaching.tsv`**; records **`out/NNNN-context/`** (`wave.tsv`, `meta.json`); updates **`hosts/<slug>/current`**.
3. **`plasm context --new -i "…" github:Issue`** — new client session id and fresh **`teaching.tsv`** (`entry_id:Entity` seeds required with `--new`).
4. **`plasm run`** — expand with client symbols, execute on server.

```text
No active plasm context for http://127.0.0.1:3000. Run `plasm context -i "…" api:Entity …` first.
```

## Commands

| Command | Role |
| --- | --- |
| `plasm init` | Configure profile (platform host runs device login unless `--no-login`) |
| `plasm login` | GitHub device OAuth for managed platform hosts |
| `plasm doctor` | Profile + `GET /v1/health` |
| `plasm search <INTENT>` | Discover; merge `discovery.tsv` |
| `plasm context [OPTIONS] <CATALOG:ENTITY>…` | Client expose; see `plasm context --help` |
| `plasm run [OPTIONS]` | Expand locally; HTTP execute (`--mode plan\|run`, `--accept plain\|toon\|json\|ndjson`) |

With **`--new`**, every seed must be `entry_id:Entity` (e.g. `pokeapi:Pokemon`). Without **`--new`**, unqualified entity names resolve via discovery cache when unique; ambiguous names error with `api:Entity` options.

## Local mirror (per project directory)

```text
.plasm/
  profiles/default.json
  # Grammar: canonical spec at docs/reference/plasm-language-definition.md (teaching table/teaching.tsv is the live teaching surface)
  hosts/<8hex>/
    discovery.tsv                  # merged search cache
    current                        # one line: active session id
  s/<8hex>/
    meta.txt                       # intent, catalog digests, capabilities
    symbols.json                   # client symbol authority
    teaching.tsv                     # cumulative teaching TSV (agent reads this)
    catalogs/<api>.json
    latest                         # one line: newest out/NNNN-* dir
    out/
      0001-search/                 # body.md + body.json (when session active)
      0002-context/                # wave.tsv + meta.json
      0003-plan/                   # program.plasm, plan.json, body.json, body.txt
      0004-run/                    # same + artifact.json / artifact.txt when available
```

**Breaking cutover:** remove old `.plasm/cgs/` trees (`rm -rf .plasm`) after upgrading; there is no migration.

**Removed:** server-root `active_context.txt`, server `session.txt` as symbol source of truth.

Default **`run`** `--accept` is **`plain`** (`text/plain`). HTTP server default when `Accept` is omitted remains **`text/toon`**.

## Example

```bash
plasm init
plasm search "pokeapi pokemon moves"
plasm context -i "inspect pokemon combat data" pokeapi:Pokemon pokeapi:Move
# Active context: pokeapi:Pokemon, pokeapi:Move
# mirror: …/teaching.tsv (+N rows)

plasm context pokeapi:Type                    # expand same client session (after search)
echo 'e1(p5=pikachu)[p5,p3]' | plasm run --mode plan
echo 'e1(p5=pikachu)[p5,p3]' | plasm run

plasm context --new -i "github issues" github:Issue
```

Paging: `e1[p5]` then `page(pg1)[p5]` in the same client session (server holds pagination handles).

Long-running plan execute: `--mode plan` for dry-run + `plan_commit_ref`; `--wait=false` to accept `wait(s0_oN)` and poll with `plasm run -e 'wait(s0_o1)'` / cancel with `cancel(s0_o1)`. See [plasm-long-operations.md](plasm-long-operations.md).

**`plasm context`** always prints the new symbol wave (TSV) on stdout; **`--verbose`** adds a stderr banner before the wave.

## Testing

- **CLI contract (insta):** `cargo test -p plasm --test plasm_cli_server_insta` — subprocess `plasm` against an in-process router (`fixtures/schemas/overshow_tools`); snapshots under `plasm-oss/crates/plasm/tests/snapshots/`. Refresh with `INSTA_UPDATE=always` during intentional output changes, then review.
- **HTTP-only routes:** `cargo test -p plasm-agent-core --test http_terminal_protocol` — `/symbols`, `/status`, `/runs`, `POST …/context` (discovery/plan/run flows are CLI snapshots).
- **Resolved plan execute:** `plasm run` compiles locally and POSTs `application/vnd.plasm.resolved-plan+json` to `POST /execute/{prompt_hash}/{session}/plan` (symbols stay client-local; server validates catalog digests + typed plan IR).
- **CLI / state invariants:** `cargo test -p plasm-agent-core terminal_cli` and `cargo test -p plasm-agent-core --lib terminal_state`
