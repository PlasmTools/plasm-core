# Appliance CLI reference

Non-interactive **`plasm-server`** subcommands for MCP policy, discovery, and OAuth. The Ratatui control station calls the same admin services in-process — see [TUI guide](../appliance/tui.md).

Apply migrations before first use:

```bash
plasm-server mcp migrate-db
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
| `mcp keys add --name NAME` | Provision new Bearer key (`--name` is **required**) |
| `mcp keys reveal <id>` | Show plaintext once |
| `mcp keys rotate <id>` | New secret, invalidate old |
| `mcp keys revoke <id>` | Disable key |
| `mcp migrate-db` | Apply `project_mcp_*` sqlx migrations |

Example:

```bash
plasm-server mcp keys add --name cursor
```

---

## `plasm-server discovery`

Semantic auto-seed (intent-only `plasm_context`) for the appliance. Persists under `{PLASM_LOCAL_STATE_DIR}/bootstrap-secrets/` (default `{appliance}/local/bootstrap-secrets/`).

| Command | Role |
|---------|------|
| `discovery status [--json]` | Show enabled flag + whether OpenRouter key is configured |
| `discovery enable` | Turn on semantic auto-seed |
| `discovery disable` | Turn off semantic auto-seed |
| `discovery set-openrouter-key [--key KEY]` | Save OpenRouter API key (`--key` or stdin) |
| `discovery clear-openrouter-key` | Remove persisted OpenRouter key |

TUI parity: [Control station (TUI)](../appliance/tui.md) — **Discovery** tab (`d`; `e` toggle, `k` set key).

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
| `--data-dir PATH` | Override appliance state root (default: `~/.plasm/appliance`). Sets `PLASM_LOCAL_STATE_DIR` to `{PATH}/local` when unset. |
| `--catalog-dir PATH` | Override compiled catalog artifacts (default: `{data-dir}/catalogs` when present) |
| `--schema PATH` | Single CGS instead of catalog dir (mutually exclusive with `--catalog-dir`) |
| `--listen-host HOST` | Bind address (default: `127.0.0.1`, or `0.0.0.0` when `KUBERNETES_SERVICE_HOST` is set; env `PLASM_LISTEN_HOST`) |
| `--port N` | HTTP + MCP on one TCP port (default: 3000; MCP path `/mcp`) |
| `--no-tui` / `--tui` | Headless vs control station |
| `--migrate-mcp-config-db` | Migrate on boot |

Full operator matrix: [Surface inventory](appliance-surface-inventory.md).
