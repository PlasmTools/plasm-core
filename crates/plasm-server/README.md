# plasm-server

Primary OSS distribution binary: in-process [`plasm-agent-core`](../plasm-agent-core) kernel, **one** concurrent HTTP+MCP listener (path-routed: discovery/execute + Streamable MCP on `/mcp`), and a Ratatui control station.

**Compile-time:** this crate **defaults to** [`plasm`](../plasm)'s **`embedded_postgres`** feature (bundles **pg-embed**). At runtime embedded Postgres **autostarts** (no env required): cache data dir, an **ephemeral loopback port** (override with `PLASM_EMBEDDED_POSTGRES_PORT`), database **`plasm_appliance`**, superuser password **`plasm_embedded_local_dev`** when none is set (pg-embed `initdb --pwfile` cannot be empty). Opt out **`PLASM_EMBEDDED_POSTGRES=0`**, or set a non-loopback Postgres URL (`DATABASE_URL` / `PLASM_MCP_CONFIG_DATABASE_URL` / `PLASM_AUTH_STORAGE_URL`). Slim binary without pg-embed: **`cargo build -p plasm-server --no-default-features`**.

From the **workspace root** (monorepo or `plasm-oss` checkout):

```bash
cargo run -p plasm-server --release -- --catalog-dir target/plasm-catalogs --port 3000
```

Binds **`127.0.0.1:<port>`** by default (HTTP + MCP on `/mcp`). LAN or containers: `--listen-host 0.0.0.0` or `PLASM_LISTEN_HOST`. In Kubernetes, default bind is `0.0.0.0` when `KUBERNETES_SERVICE_HOST` is set.

- **Release / `install.sh` default:** no flags required — state under `~/.plasm/appliance` (`postgres/`, `local/`, `catalogs/` / packed catalog dir from the installer). Same layout via `PLASM_APPLIANCE_DIR` or `--data-dir`.
- Headless: add `--no-tui`
- **Durable layout:** `PLASM_EMBEDDED_POSTGRES_DATA_DIR` → `{appliance}/postgres`, `PLASM_LOCAL_STATE_DIR` → `{appliance}/local` (see `docs/oss-core-trace-artifacts.md`). Put `PLASM_APPLIANCE_DIAG_LOG` and other non-DB files **next to** `postgres/`, not inside it.
- **Local auth KV encryption key:** when durable Postgres-backed auth storage is active and `AUTH_STORAGE_ENCRYPTION_KEY` is unset, `plasm-server` now reuses or creates a local key file at `{PLASM_LOCAL_STATE_DIR}/bootstrap-secrets/AUTH_STORAGE_ENCRYPTION_KEY` (for `--data-dir ~/.plasm/appliance`, that becomes `~/.plasm/appliance/local/bootstrap-secrets/AUTH_STORAGE_ENCRYPTION_KEY`). Keep that file stable across restarts or previously encrypted OAuth secrets and MCP API keys will become unreadable. To manage the key yourself, set `AUTH_STORAGE_ENCRYPTION_KEY` explicitly. Kubernetes / hosted deployments should continue using explicit secret management (`PLASM_SECRETS_DIR` / environment), not this local bootstrap path.
- Migrations: `--migrate-mcp-config-db` (same env as `plasm-mcp` for DB URL resolution)

**Outbound OAuth (providers + device flow):** non-interactive **`plasm-server oauth`** mirrors the HTTP `/internal/oauth-link/v1/*` contract in-process — `oauth provider list|upsert|disable`, `oauth device start|poll` (see [`oauth_cli.rs`](src/oauth_cli.rs)). The Ratatui **OAuth** tab lists `oauth_provider_apps` rows and **`d`** runs RFC 8628 device authorization + polling against the configured IdP.

Strict remote client / security boundary: **`plasm`**, not this crate.

Monorepo inventory: [docs/appliance-surface-inventory.md](../../../docs/appliance-surface-inventory.md).
