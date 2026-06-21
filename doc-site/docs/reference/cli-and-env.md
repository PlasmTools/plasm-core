# CLI flags and environment (index)

!!! tip "Read this when…"

    You need an env var, migration flag, trace hub knob, or execute/MCP switch and want the repo-truth pointer.

**What to know first:** [Appliance quick start](../appliance/quickstart.md) for install and first boot.

**Practical takeaway:** This page is an **index**—single source of truth remains **`AGENTS.md`** on the branch you ship.

---

## Release binaries

| Binary | Role | Doc |
|--------|------|-----|
| **`plasm-server`** | Appliance — HTTP/MCP host, TUI, embedded Postgres | [Quick start](../appliance/quickstart.md), [CLI reference](appliance-cli.md) |
| **`plasm`** | Remote HTTP terminal client | [Remote terminal](plasm-cgs-remote-terminal.md) |
| **`plasm-cgs`** | One-shot compile + HTTP from dev checkout | [Start here](../getting-started.md) |
| **`plasm-repl`** | Interactive REPL against a backend | [Start here](../getting-started.md) |

---

## Common appliance environment

| Variable | Role |
|----------|------|
| `DATABASE_URL` / `PLASM_MCP_CONFIG_DATABASE_URL` | Postgres for `project_mcp_*` (embedded autostart when unset) |
| `PLASM_EMBEDDED_POSTGRES` | `0` disables embedded Postgres autostart |
| `PLASM_EMBEDDED_POSTGRES_DATA_DIR` | Postgres cluster directory |
| `PLASM_LOCAL_STATE_DIR` | Trace archive, run artifacts, bootstrap secrets |
| `PLASM_TRACE_ARCHIVE_DIR` | Durable completed traces |
| `PLASM_RUN_ARTIFACTS_DIR` | Filesystem run snapshots |
| `PLASM_APPLIANCE_DIAG_LOG` | Optional TUI diag log file path |
| `AUTH_STORAGE_ENCRYPTION_KEY` | Encrypt OAuth secrets / MCP key material at rest |

Full matrix: [Surface inventory](appliance-surface-inventory.md).

---

## Authoritative lists (repository)

[`AGENTS.md` (main branch)](https://github.com/PlasmTools/plasm-core/blob/main/AGENTS.md) covers:

- HTTP / MCP ports, `--catalog-dir`, `--schema`, execute `Accept` negotiation, MCP tools
- **Trace hub** caps: `PLASM_TRACE_HUB_*`
- **Run artifacts:** `PLASM_RUN_ARTIFACTS_URL`, retention and GC intervals
- **Incoming auth:** `PLASM_INCOMING_AUTH_MODE`, `PLASM_AUTH_JWT_SECRET`

**Links**

- [Incoming authentication](plasm-mcp-incoming-auth.md)
- [OSS appliance MCP persistence](oss-appliance-mcp-persistence.md)
- [Trace artifacts](oss-core-trace-artifacts.md)
