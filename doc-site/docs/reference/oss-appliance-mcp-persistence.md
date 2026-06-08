# OSS appliance MCP persistence — synthetic tenant + sqlx tables

This document fixes **where MCP policy lives** for the single-user **OSS appliance** (desktop Phoenix + local `plasm-mcp`): **no parallel policy store** in desktop KV or bespoke schemas; **one synthetic tenant row** in the **same `project_mcp_*` tables** the agent already reads via sqlx.

**Related:** `oss-saas-boundary.md`, `private-control-plane-api.md`, `plasm-mcp-tenant-configuration.md`, [oss-outgoing-oauth-promotion.md](oss-outgoing-oauth-promotion.md), `ProjectMcp (Phoenix)`, [`plasm-agent-core` migrations](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/migrations/).

---

## Canonical store

| Concern | Location |
|--------|----------|
| MCP allowlists, capability/auth bindings, API key hashes | **`project_mcp_*`** tables applied by **`plasm-agent-core` migrations** (e.g. `project_mcp_configs`, `project_mcp_allowed_graphs`, related auth rows). |
| Runtime reads | **`plasm-mcp`** process via sqlx (`McpRuntimeConfig` → [`mcp_policy.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/mcp_policy.rs)). |

The appliance **does not introduce** a second logical model (no duplicated allowlist columns in `desktop_settings`, no separate “appliance policy” table).

---

## Single synthetic tenant

SaaS binds MCP config to `(tenant_id, workspace_slug, project_slug)`. The appliance uses **exactly one** intended configuration:

- **Stable identifiers** — Rust defaults live in [`appliance_mcp_defaults`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/appliance_mcp_defaults.rs): `PLASM_APPLIANCE_MCP_TENANT_ID` (`appliance-local`), `PLASM_APPLIANCE_MCP_WORKSPACE_SLUG` / `PLASM_APPLIANCE_MCP_PROJECT_SLUG` (`default`). Every upsert must reuse the same triple and config UUID.
- **Desktop chrome** — Phoenix desktop persists `mcp_appliance_config_id` and `mcp_appliance_endpoint_hash_hex` in `desktop_settings` (policy pointers only). Optional env overrides: `PLASM_APPLIANCE_MCP_CONFIG_ID`, `PLASM_APPLIANCE_MCP_ENDPOINT_HASH_HEX` (must match the row already on the agent if set).
- **Invariant** — at most **one** active appliance MCP policy row for that triple (plus normal versioning fields); UI edits **that** row’s graphs and secrets, not a catalog of workspaces.

This keeps Elixir and Rust aligned with existing uniqueness indexes (e.g. tenant/workspace/project) without inventing a new key space.

---

## Physical database topology

| Component | Rule |
|-----------|------|
| **`plasm-mcp`** | Uses `DATABASE_URL` → **same Postgres** that holds `project_mcp_*`. |
| **Desktop Phoenix** | May use its **own Ecto schema prefix** (`plasm_desktop`, etc.) for app tables while **sharing the database server** with agent migrations. |
| **Policy writes** | Target **`project_mcp_*` only** (via Ecto schemas mirroring agent DDL, or via HTTP upserts below). Desktop tables **must not** become a second source of truth for allowlists or MCP API key policy. |

Optional desktop KV (`desktop_settings`) remains **chrome only** (e.g. agent base URL, UI prefs), not MCP catalog parity.

---

## Secure upsert path (without SaaS `/internal/*`)

SaaS pushes JSON via `ProjectMcp (Phoenix)` to **`/internal/*`** on the hosted binary.

**OSS `plasm-mcp` (implemented):** When `PLASM_MCP_CONFIG_DATABASE_URL` / `PLASM_AUTH_STORAGE_URL` / `DATABASE_URL` resolves, the OSS binary connects **`project_mcp_*`**, mounts the same routes as `private-control-plane-api.md` **`McpConfig_v1`** / **`McpApiKey_v1`** under `/internal/…` (handlers in [`http_mcp_config.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/http_mcp_config.rs)), and wires MCP transport API keys (Postgres auth KV when available, otherwise **in-memory** keys only until restart). Use **`cargo run -p plasm --bin plasm-mcp -- --migrate-mcp-config-db`** against that URL to apply migrations.

**Guards:**

- **Shared secret** header (same spirit as `PLASM_MCP_CONTROL_PLANE_SECRET`) **or**
- **Loopback-only** listener for upsert routes **or**
- Both — defense in depth for a machine-local appliance.

Desktop (or a local operator script) calls **that** endpoint after edits; the agent reloads policy from sqlx like today.

**Alternative implementation detail:** Phoenix desktop may write **`project_mcp_*` via Ecto** in-process **only when** it shares the DB with `plasm-mcp` and uses the **same migrations** — still **one** logical store; HTTP upsert stays the portable choice when processes split hosts.

---

## Duplication rule (explicit)

| Allowed | Rejected |
|---------|----------|
| One row in `project_mcp_configs` (+ children) for the synthetic triple | Mirroring `allowed_entry_ids` into `desktop_settings` |
| Shared Postgres + agent migrations as DDL authority | A second “appliance allowlist” table maintained by hand |
| Payload parity with `ProjectMcp.payload_for_agent` / control-plane JSON | Divergent JSON that Rust never applies |

---

## Summary

**Canonical MCP policy for the appliance = existing sqlx `project_mcp_*`, keyed by one synthetic tenant triple, updated through a secure promoted upsert or co-located Ecto writes against that DDL — never a parallel desktop DB model.**

---

## Operator notes (appliance schema)

The OSS appliance ships **one** idempotent sqlx migration ([`20260601000000_plasm_agent_schema.sql`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/migrations/20260601000000_plasm_agent_schema.sql)) covering `project_mcp_*`, discovery embeddings, and `oauth_provider_apps`. There is no in-field upgrade path for unreleased appliances — after a schema squash, wipe embedded data once:

```bash
rm -rf ~/.plasm/appliance/postgres
plasm-server --plugin-dir ~/.plasm/appliance/plugins
```

`plasm-server mcp migrate-db` is safe to re-run on a healthy database (idempotent `IF NOT EXISTS` DDL).

**Shared SaaS Postgres** (hosted `plasm-mcp` + Phoenix): sqlx migrations must stay idempotent; **never** delete rows from `_sqlx_migrations` in application code. Phoenix must not `DROP project_mcp_*` after MCP creates them — see `plasm-mcp-tenant-configuration.md`.
