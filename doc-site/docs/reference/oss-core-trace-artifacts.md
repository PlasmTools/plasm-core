# OSS / core: traces and run artifacts without object storage

**Enterprise / hosted** deployments may use **trace sink** (`PLASM_TRACE_SINK_URL` → `plasm-trace-sink`) and **object stores** (`PLASM_RUN_ARTIFACTS_URL`). **Core single-user OSS** should rely on **local disk** — no S3 required.

## Environment variables (implemented)

| Purpose | Variable | Behavior |
|---------|----------|----------|
| Local state root | `PLASM_LOCAL_STATE_DIR` | Parent for default trace archive + run-artifact paths and appliance bootstrap secrets. |
| Local trace archive | `PLASM_TRACE_ARCHIVE_DIR` | When set, completed traces are written under `traces/{tenant_id}/{trace_id}/` (summary + NDJSON). See [`local_trace_archive.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/local_trace_archive.rs). |
| Run snapshots / plan archive | `PLASM_RUN_ARTIFACTS_DIR` | Filesystem backend for execute run JSON and plan archive. **Precedence:** if `PLASM_RUN_ARTIFACTS_URL` is set, object store wins and `PLASM_RUN_ARTIFACTS_DIR` is ignored for backend selection. See [`run_artifacts.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/run_artifacts.rs). |
| Trace sink (optional, hosted-class) | `PLASM_TRACE_SINK_URL`, `PLASM_TRACE_SINK_READ_URL` | HTTP ingest + read base for durable tenant history beyond local archive. |

## Defaults: appliance vs standalone `plasm-mcp`

| Host | Default behavior |
|------|------------------|
| **`plasm-server` (appliance)** | When unset, sets **`PLASM_LOCAL_STATE_DIR`** to **`{appliance}/local`** (e.g. `~/.plasm/appliance/local` for the default `--data-dir`). Trace archive and run-artifact roots resolve under that tree. Operators may still override with explicit env vars. |
| **Standalone `plasm-mcp`** | Does **not** invent an appliance data-dir layout. Set **`PLASM_LOCAL_STATE_DIR`** (or the specific `PLASM_TRACE_ARCHIVE_DIR` / `PLASM_RUN_ARTIFACTS_DIR` paths) for durable disk; otherwise traces/artifacts stay in-memory. A common convention is `$HOME/.plasm/local`. |

Use absolute paths in scripts and systemd/desktop entries. Ensure the process user can create those directories.

## Run snapshot identity (`run_id`)

Execute run JSON snapshots use a **single wire form**: ASCII prefix **`pr`** plus **64 hexadecimal digits** (lowercase in server output; parsers accept uppercase hex). This is the SHA256 of a versioned JSON bundle (pinned `catalog_cgs_hash`, `domain_revision`, `entry_id`, trimmed source line, serialized parsed plan, and **sorted** `request_fingerprints`) so paging and distinct HTTP batches produce distinct ids.

**URI shapes** (full cutover — no UUID, no `/r/{n}`):

- Canonical: `plasm://execute/{prompt_hash}/{session_id}/run/pr{64hex}`
- MCP short: `plasm://session/{logical_session_ref}/run/pr{64hex}`

Durable filesystem / object-store blob filenames use the **full 32-byte digest** (see `run_artifacts.rs`). Hyphenated **UUID** `run_id` segments are **not** accepted on GET or in `resources/read`.

## Docs and UX alignment

- Document these vars in any **core** onboarding path (README / desktop installer), separate from Helm/object-store guides.
- Operator UIs that list traces expect agent `/v1/traces*`; durable list/detail requires local archive or sink — see [`http_traces.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/http_traces.rs).
- **OTLP export** (optional): when `OTEL_*` collectors are configured, application spans use stable semantic names (`plasm_agent.*`, `plasm_core.*`, `plasm_runtime.*`) — not Rust module paths. Contract and dashboard guidance: [`plasm-otel` README — Semantic span names](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-otel/README.md#semantic-span-names-stable-contract).
