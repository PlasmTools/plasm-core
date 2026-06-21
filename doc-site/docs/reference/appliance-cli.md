# Appliance CLI reference

Non-interactive **`plasm-server`** subcommands for MCP policy and OAuth. The Ratatui control station calls the same admin services in-process — see [TUI guide](../appliance/tui.md).

Apply migrations before first use:

```bash
plasm-server mcp migrate-db
# or legacy: plasm-server --migrate-mcp-config-db
```

---

## `plasm-server mcp`

| Command | Role |
|---------|------|
| `mcp status [--json]` | Synthetic tenant, enabled APIs, key count |
| `mcp init` | Bootstrap appliance MCP row when empty |
| `mcp apis list` | Registry entries vs enabled set |
| `mcp apis enable <entry_id>…` | Add to allowlist |
| `mcp apis disable <entry_id>…` | Remove from allowlist |
| `mcp apis set <entry_id>…` | Replace allowlist |
| `mcp keys list [--json]` | Transport API keys (hashes only) |
| `mcp keys add [--name NAME]` | Provision new Bearer key |
| `mcp keys reveal <id>` | Show plaintext once |
| `mcp keys rotate <id>` | New secret, invalidate old |
| `mcp keys revoke <id>` | Disable key |
| `mcp migrate-db` | Apply `project_mcp_*` sqlx migrations |

---

## `plasm-server oauth`

| Command | Role |
|---------|------|
| `oauth provider list [--json]` | Rows from `oauth_provider_apps` |
| `oauth provider upsert …` | Create/update provider (flags; `--client-secret-stdin` for secrets) |
| `oauth provider disable <id>` | Mark provider inactive |
| `oauth device start …` | RFC 8628 device authorization |
| `oauth device poll …` | Poll device code until bound |

TUI parity: [Control station (TUI)](../appliance/tui.md) — OAuth tab (`n`, `d`, `x`/`y`).

---

## Serve flags (common)

| Flag | Role |
|------|------|
| `--data-dir PATH` | Override appliance state root (default: `~/.plasm/appliance`) |
| `--catalog-dir PATH` | Override compiled catalog artifacts (default: `{data-dir}/catalogs` when present) |
| `--schema PATH` | Single CGS instead of catalog dir (mutually exclusive with `--catalog-dir`) |
| `--listen-host HOST` | Bind address (default: `127.0.0.1`, or `0.0.0.0` when `KUBERNETES_SERVICE_HOST` is set; env `PLASM_LISTEN_HOST`) |
| `--port N` | HTTP + MCP on one TCP port (default: 3000; MCP path `/mcp`) |
| `--no-tui` / `--tui` | Headless vs control station |
| `--migrate-mcp-config-db` | Migrate on boot |

Full operator matrix: [Surface inventory](appliance-surface-inventory.md).
