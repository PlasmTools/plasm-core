# Appliance surface inventory

Maps operator-facing capabilities to **appliance** (single-user local binary + TUI), **shared** (OSS agent HTTP/MCP used by multiple shells), or **SaaS-only** (Phoenix product / multi-tenant control plane). See also [oss-appliance-mcp-persistence.md](oss-appliance-mcp-persistence.md), `oss-core-ui-surface.md`, [Plasm Cloud](https://platform.plasm.tools).

| Capability | Classification | Primary implementation | Notes |
|------------|----------------|-------------------------|--------|
| Registry / tool-model / discovery | Shared | `plasm-agent-core` HTTP `/v1/registry`, `/v1/registry/:entry_id/tool-model`, `POST /v1/discover` | TUI uses in-process [`appliance_services`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/appliance_services.rs); external agents use HTTP. |
| Execute sessions / plans / artifacts | Shared | `http_execute`, `execute_session`, MCP tools | Same engine; TUI observes via host state. |
| MCP tenant policy (`project_mcp_*`) | Appliance + shared | `mcp_config_repository`, `http_mcp_config` | Single synthetic tenant for appliance per [oss-appliance-mcp-persistence.md](oss-appliance-mcp-persistence.md). |
| MCP transport API keys | Appliance + shared | `mcp_api_key_registry`, `/internal/mcp-api-key/v1/*` | Appliance configures in-process; protocol remains for remote ops scripts if listeners enabled. |
| Outbound OAuth link / secrets | Appliance + shared | `oauth_link_catalog`, `http_oauth_link`, `http_outbound_secrets`, `oauth_provider_repository`, RFC 8628 device routes | Postgres `oauth_provider_apps` (+ optional `device_authorization_endpoint`) via sqlx migrations; browser code flow + device flow; binding pointer KV `plasm:oauth_binding:v1:{entry_id}`. |
| Incoming JWT / API key (execute identity) | Shared | `incoming_auth` | Optional on OSS appliance. |
| Traces / trace hub / archive | Shared | `trace_hub`, `http_traces`, env sinks | TUI panels read host state + HTTP-shaped helpers as needed. |
| Embedded PostgreSQL | Appliance | `plasm` `embedded_postgres` (default **on** for the **`plasm-server`** package via default feature) | **Autostart** (cache dir, **ephemeral loopback port** by default, DB `plasm_appliance`) unless `PLASM_EMBEDDED_POSTGRES=0` or a non-loopback Postgres URL is already set. See `env-profiles.md`. On **Unix**, concurrent setup takes an OS-level `flock` on `pg-embed-setup.flock` beside the cache so parallel appliances / PTY tests cannot corrupt the shared pg-embed `bin/` tree. |
| `plasm` remote terminal | **Strict client (OSS)** | `plasm-agent-core` `terminal.rs` | Binary **`plasm`** from `plasm`; transport-only; no in-process kernel access. |
| `plasm-server` TUI | Appliance | `plasm-oss/crates/plasm-server` | Unified local operator UX; in-process kernel. **Serve mode:** Ratatui when **both** stdout and stdin are TTYs; otherwise headless (stderr bootstrap milestones). **`--no-tui`** always headless; **`--tui`** forces the control station. Optional **`--data-dir PATH`** (before subcommands when using the flattened serve flags) applies a stable disk layout when `PLASM_EMBEDDED_POSTGRES_DATA_DIR` / `PGDATA` / `PLASM_LOCAL_STATE_DIR` are unset: `{PATH}/postgres` for embedded Postgres (reused across restarts), `{PATH}/local` for default OSS trace archive + run-artifact roots under [`PLASM_LOCAL_STATE_DIR`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/oss_local_state.rs). Override any piece with explicit env vars. |
| Workspace / billing / org shell | SaaS-only | Phoenix `/w/...` | Not lifted into appliance. |
| GitHub login / incoming org provisioning | SaaS-only | Phoenix + `/internal/incoming-tenant/v1/*` | Appliance does not model multi-tenant onboarding. |
| Ops APIs admin UI | SaaS-only | Phoenix `/ops/...` | Operator catalog OAuth promotion is product chrome. |
| Phoenix → agent control-plane sync | SaaS-only | `PlasmMcpClient` `/internal/*` | Appliance uses direct services, not Phoenix. |

**Distribution summary**

- **Appliance:** **`plasm-server`** binary (TUI + optional HTTP/MCP listeners + embedded Postgres path).
- **Strict remote client:** `plasm` (HTTP terminal; security boundary).
- **Headless server:** `plasm-mcp` (OSS) / `plasm-mcp-saas` (hosted) unchanged for CI, containers, and agent-only deployments.

## MCP configuration (Your MCP singleton)

- **Shared domain service:** `plasm-agent-core` [`mcp_config_admin`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/mcp_config_admin.rs) drives `project_mcp_*` via [`McpConfigRepository`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/mcp_config_repository.rs) and transport keys via [`McpTransportAuth`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/mcp_transport_auth.rs) — **no HTTP loopback**.
- **Appliance adapter:** [`appliance_mcp_admin`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-server/src/appliance_mcp_admin.rs) pins the synthetic tenant triple from [`appliance_mcp_defaults`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/appliance_mcp_defaults.rs).
- **TUI:** [`plasm-server` control station](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-server/src/tui.rs) — tabs **Status · Clients · APIs · OAuth · Keys · Runs · Storage · Logs**; **Clients** shows Cursor-style `mcp.json` (`streamableHttp` + Bearer) and **`c`** copies config with the selected key; **OAuth** lists `oauth_provider_apps` rows + binding hints; **`n`** opens provider upsert wizard; **`d`** runs RFC 8628 device bind (blocking poll); **`x`/`y`** disable provider (confirm); catalog rows use **`entry_id — label`** (no CGS hash spam); APIs tab supports filter (`/`), toggle (`Space`), save (`s`), keys add/rotate/revoke/reveal.
- **CLI (non-interactive):** `plasm-server mcp …` ([`mcp_cli`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-server/src/mcp_cli.rs)) — `status`, `init`, `apis list|enable|disable|set`, `keys list|add|reveal|rotate|revoke`, `migrate-db`. **`plasm-server oauth …`** ([`oauth_cli`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-server/src/oauth_cli.rs)) — `provider list|upsert|disable`, `device start|poll` (`--json` where noted). Legacy top-level `--migrate-mcp-config-db` unchanged.

### OAuth — CLI vs control-station TUI parity

Canonical keystrokes and wizard behavior live in [`tui.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-server/src/tui.rs). PTY coverage map: [`tui_feature_inventory.md`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-server/tests/tui_feature_inventory.md).

| Capability | `plasm-server oauth …` CLI | TUI (OAuth tab) |
|------------|--------------------------------|-----------------|
| List providers | `provider list` | Read-only list (same DB rows via admin refresh) |
| Upsert provider | `provider upsert` (flags + optional secret) | **`n`** multi-step wizard → `AdminJob::OauthProviderUpsert` (same [`appliance_oauth_upsert_provider`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-server/src/appliance_oauth_admin.rs)); raw TTY echoes typed secrets — use CLI `--client-secret-stdin` for sensitive values |
| Disable provider | `provider disable` | **`x`** mark disable, **`y`** confirm → `AdminJob::OauthProviderDisable` |
| Device start / poll | `device start` / `device poll` | Single **`d`** flow: `AdminJob::OAuthDeviceBind` (start + poll, 600s). Scopes: if the job carries an empty list, the admin task fills from catalog [`default_scopes`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/oauth_link_catalog.rs) via `resolve_for_oauth_start`; otherwise uses the wizard/device prompt scopes |

### Verification commands

- **Agent-core integration (Postgres):** `cargo test -p plasm-agent-core --test mcp_config_admin` (Docker or optional `PLASM_TEST_POSTGRES_URL`; see `env-profiles.md`).
- **CLI smoke:** with reachable Postgres (`DATABASE_URL` / embedded autostart, or e.g. `postgres:16`), and migrations applied (`plasm-server mcp migrate-db` or `--migrate-mcp-config-db`):  
  `plasm-server mcp status --json`, `plasm-server mcp apis set foo`, `plasm-server mcp keys add --name t`, `plasm-server mcp keys list --json`.
- **Lint:** `cargo clippy -p plasm-agent-core -p plasm-server --all-targets -- -D warnings`.

### TUI appliance CI (`appliance_headless_boot` + PTY quit smoke)

CI runs two gates via `bash scripts/appliance-tui-pty-tests.sh` (CircleCI `appliance_tui_pty`):

1. **Headless smoke** (`plasm-server --no-tui`, or serve with non-TTY stdout/stdin): embedded Postgres + HTTP listener milestones in `PLASM_APPLIANCE_DIAG_LOG` — fast, no PTY.
2. **PTY quit smoke** (`plasm-server-pty-tests`, [`testty`](https://crates.io/crates/testty)): spawn release `plasm-server` in a PTY, `wait_for_stable_frame`, `wait_for_text("q: quit")`, press `q`, assert exit 0. Tab/OAuth/keys strings are covered by **`TestBackend`** unit tests in `plasm-server/src/tui/mod.rs` (see `tests/tui_feature_inventory.md`).

`testty` lives in a **separate crate** so its `vt100` / `unicode-width` stack does not conflict with `ratatui`’s pinned `unicode-width = 0.2.0`.

Plain `cargo test -p plasm-server` runs headless smoke only when invoked directly; the full appliance gate uses the script below.

- **PTY green does not imply your IDE terminal cannot hang.** Treat PTY as a **regression harness**, not proof of identical interactive behavior.

- **Headless provision signal:** `McpConfigAdminService::provision_api_key` is covered in `plasm-agent-core/tests/mcp_config_admin.rs`; CircleCI **`validate`** runs `-p plasm-agent-core` when Postgres is available.

- **Build / run** (repo root):

  `bash scripts/appliance-tui-pty-tests.sh`

  Manual: `cargo build --release -p plasm-server` then `cargo test --release -p plasm-server --test appliance_headless_boot` and `cargo test --release -p plasm-server-pty-tests`.

  The script exports **`RUST_TEST_THREADS=1`** and passes **`--test-threads=1`**. Watchdog: `PLASM_TUI_PTY_WATCHDOG_SECS` (default **600**). On timeout it SIGTERM/SIGKILLs `cargo` and may exit **124**.

- **Environment passed to the PTY child:** `NO_COLOR=1`, per-run `--data-dir`, `PLASM_EMBEDDED_POSTGRES=1`, `PLASM_EMBEDDED_POSTGRES_TIMEOUT_SECS=300`, `AUTH_STORAGE_ENCRYPTION_KEY` (see `tests/appliance_boot_support.rs`), `OTEL_SDK_DISABLED=true`, `PLASM_APPLIANCE_DIAG_LOG={data-dir}/appliance-diag.log`, fixture schema `fixtures/schemas/overshow_tools`. The script unsets external Postgres URLs before tests. **`--data-dir` also clears inherited `DATABASE_URL`** so embedded pg-embed does not reuse a stale loopback port (e.g. 55432 from a prior appliance); autostart picks a free port and rewrites URLs after `embedded postgres: server ready`.

- **Appliance diagnostics (optional, for operators and PTY e2e):**
  - **`PLASM_APPLIANCE_DIAG_LOG`**: UTF-8 filesystem path. When set (TUI mode only), each formatted `tracing` line is appended to this file in addition to the in-process Logs tab sink. If the path cannot be opened, `plasm-server` prints one warning to stderr and continues with the TUI sink only. Default interactive runs omit this; PTY tests set it automatically. **Do not** place this file inside `PLASM_EMBEDDED_POSTGRES_DATA_DIR` / `{--data-dir}/postgres` (`initdb` requires that directory to contain only a cluster); prefer `{--data-dir}/appliance-diag.log` or use `--data-dir` so `postgres/` is isolated.
  - **`PLASM_APPLIANCE_BOOT_TRACE_STDERR`**: when set to `1`, `true`, or `yes`, bootstrap **phase** / **fatal** messages mirror to stderr as well as `tracing`, even when the BOOT TUI is active (normally those lines only go to `tracing` + the Logs tab). Useful with `cargo test -- --nocapture` without opening the diag file.

- **Ports:** each spawn binds `127.0.0.1:0` three times to pick disjoint Postgres / HTTP / MCP ports (avoids collisions with fixed ranges or other listeners).

- **Platform:** Unix PTY only (`#![cfg(unix)]` on the test crate). macOS and Linux are supported; CI should run this target only on runners with a working PTY.

- **Coverage map:** [`plasm-oss/crates/plasm-server/tests/tui_feature_inventory.md`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-server/tests/tui_feature_inventory.md).

- **Troubleshooting (local):** if a PTY run wedges or you abort mid-test, check for orphaned **`plasm-server-pty-tests-*`** or **`plasm-server`** children (each run spawns a real binary + embedded Postgres). Stale children can pin ports or confuse later runs.
