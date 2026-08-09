# OSS appliance MCP persistence — synthetic tenant + sqlx tables

This document fixes **where MCP policy lives** for the single-user **OSS appliance** (`plasm-server` / local `plasm-mcp`): **no parallel policy store** in desktop KV or bespoke schemas; **one synthetic tenant row** in the **same `project_mcp_*` tables** the agent already reads via sqlx.

**Related:** [oss-outgoing-oauth-promotion.md](oss-outgoing-oauth-promotion.md), [`plasm-agent-core` migrations](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/migrations/).

---

## Canonical store

| Concern | Location |
|--------|----------|
| MCP allowlists, capability/auth bindings, API key hashes | **`project_mcp_*`** tables applied by **`plasm-agent-core` migrations** (e.g. `project_mcp_configs`, `project_mcp_allowed_graphs`, related auth rows). |
| Runtime reads | **`plasm-mcp`** / **`plasm-server`** via sqlx (`McpRuntimeConfig` → [`mcp_policy.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/mcp_policy.rs)). |

The appliance **does not introduce** a second logical model (no duplicated allowlist columns in local settings, no separate “appliance policy” table).

---

## Single synthetic tenant

Multi-tenant SaaS binds MCP config to `(tenant_id, workspace_slug, project_slug)`. The appliance uses **exactly one** intended configuration:

- **Stable identifiers** — Rust defaults live in [`appliance_mcp_defaults`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/appliance_mcp_defaults.rs): `PLASM_APPLIANCE_MCP_TENANT_ID` (`appliance-local`), `PLASM_APPLIANCE_MCP_WORKSPACE_SLUG` / `PLASM_APPLIANCE_MCP_PROJECT_SLUG` (`default`). Every upsert must reuse the same triple and config UUID.
- **Optional overrides** — `PLASM_APPLIANCE_MCP_CONFIG_ID`, `PLASM_APPLIANCE_MCP_ENDPOINT_HASH_HEX` (must match the row already on the agent if set).
- **Invariant** — at most **one** active appliance MCP policy row for that triple (plus normal versioning fields); operators edit **that** row’s graphs and secrets via TUI/CLI or `/internal/mcp-config/v1/*`, not a catalog of workspaces.

---

## Physical database topology

| Component | Rule |
|-----------|------|
| **`plasm-server` / `plasm-mcp`** | Uses `DATABASE_URL` / `PLASM_MCP_CONFIG_DATABASE_URL` / `PLASM_AUTH_STORAGE_URL` → Postgres that holds `project_mcp_*`. Appliance embedded Postgres is the default local path. |
| **Policy writes** | Target **`project_mcp_*` only** (TUI/CLI admin services, or HTTP upserts below). Local UI prefs **must not** become a second source of truth for allowlists or MCP API key policy. |

---

## Secure upsert path

**OSS `plasm-mcp` / `plasm-server` (implemented):** When a config DB URL resolves, the process connects **`project_mcp_*`**, mounts **`/internal/mcp-config/v1/*`** and **`/internal/mcp-api-key/v1/*`** (handlers in [`http_mcp_config.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/http_mcp_config.rs)), and wires MCP transport API keys (Postgres auth KV when available, otherwise **in-memory** keys only until restart).

```bash
plasm-server mcp migrate-db
# or: cargo run -p plasm --bin plasm-mcp -- --migrate-mcp-config-db
```

**Guards:**

- **Shared secret** header (`X-Plasm-Control-Plane-Secret` / `PLASM_MCP_CONTROL_PLANE_SECRET`) **or**
- **Loopback-only** listener for upsert routes **or**
- Both — defense in depth for a machine-local appliance.

Operator tooling (TUI **Keys** / **APIs**, CLI `plasm-server mcp …`, or a local script) calls **that** surface after edits; the agent reloads policy from sqlx.

!!! note "Hosted control plane"

    Multi-tenant hosted binaries push the same JSON shape over `/internal/*` from a private control plane. That is out of scope for OSS operators — the sqlx tables and upsert contract are shared; the UI is not.

---

## Duplication rule (explicit)

| Allowed | Rejected |
|---------|----------|
| One row in `project_mcp_configs` (+ children) for the synthetic triple | Mirroring `allowed_entry_ids` into a second local settings store |
| Shared Postgres + agent migrations as DDL authority | A second “appliance allowlist” table maintained by hand |
| Payload parity with control-plane MCP JSON | Divergent JSON that Rust never applies |

---

## Summary

**Canonical MCP policy for the appliance = existing sqlx `project_mcp_*`, keyed by one synthetic tenant triple, updated through TUI/CLI or a secure upsert — never a parallel desktop DB model.**

---

## Operator notes (appliance schema)

The OSS appliance ships **one** idempotent sqlx migration ([`20260601000000_plasm_agent_schema.sql`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/migrations/20260601000000_plasm_agent_schema.sql)) covering `project_mcp_*`, discovery embeddings, and `oauth_provider_apps`. There is no in-field upgrade path for unreleased appliances — after a schema squash, wipe embedded data once:

```bash
rm -rf ~/.plasm/appliance/postgres
plasm-server --catalog-dir ~/.plasm/appliance/catalogs
```

`plasm-server mcp migrate-db` is safe to re-run on a healthy database (idempotent `IF NOT EXISTS` DDL).
