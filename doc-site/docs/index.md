# Plasm OSS documentation

**Plasm** turns APIs into a **typed graph** (what exists, how it relates, what you can do), maps that graph to **HTTP or GraphQL** calls, and exposes a **compact language** agents can learn once—then reuse across catalogs. This site teaches how to **run** the open-source stack, **connect** new APIs, or **embed** the engine in your own systems.

Repository: **[plasm-core](https://github.com/PlasmTools/plasm-core)**.

---

## Pick your path

| Path | You want to… | Start here |
|------|----------------|------------|
| **Run the appliance** | Operate **`plasm-server`** locally — TUI, MCP, OAuth, embedded Postgres | [Appliance quick start](appliance/quickstart.md) |
| **Use the remote terminal** | Connect agents/CI with **`plasm init`** → **`search`** → **`context`** → **`run`** | [Remote terminal (`plasm`)](reference/plasm-cgs-remote-terminal.md) |
| **Connect an API** | Author `domain.yaml` + `mappings.yaml`, validate, pack catalogs | [Catalog authoring](authoring/index.md) → [Catalog index](reference/apis-readme.md) |
| **Embed Plasm** | Use crates (`plasm-runtime`, `plasm-agent-core`) from your own binary | `Embed Plasm` |

**New to the ideas?** Read [How Plasm fits together](concepts.md), then [Start here](getting-started.md).

---

## How the pieces stack (one minute)

1. **Graph (CGS)** — Entities, fields, relations, capabilities: *what the domain is* (split catalogs: field/param **`value_ref`** into **`values:`** semantic slots).
2. **Mappings (CML)** — Per-capability templates: *how calls hit the wire*.
3. **Runtime** — Executes capabilities, caches rows, handles paging and effects.
4. **Plasm language** — Path expressions and programs agents emit against a live **teaching table** (`e#` / `m#` / `r#` + **wire names** for fields/params; legacy `p#` rejected).
5. **Host** — **`plasm-server`** serves MCP tools + HTTP discovery/execute; optional **`plasm`** client for transport-only remote access.

Details and edge cases live in the [Reference](reference/cli-and-env.md) section and [AGENTS.md](https://github.com/PlasmTools/plasm-core/blob/main/AGENTS.md).

---

## Quick links

| Need | Page |
|------|------|
| Install + first boot | [Appliance quick start](appliance/quickstart.md) |
| **TUI: enable APIs, add keys, copy client config** | [Control station (TUI)](appliance/tui.md) |
| First commands from source | [Start here](getting-started.md) |
| Mental model + vocabulary | [Concepts](concepts.md) |
| Language + heredocs | [Language definition](reference/plasm-language-definition.md) |
| MCP sessions and `intent` | [MCP session reuse](reference/mcp-session-reuse.md) |
| Full CLI/env index | [CLI & environment](reference/cli-and-env.md) |

---

## Maintainers

Sources under `doc-site/docs/` are curated for the public OSS repo; some pages are synced via [`doc-site/scripts/sync_allowlisted_docs.py`](https://github.com/PlasmTools/plasm-core/blob/main/doc-site/scripts/sync_allowlisted_docs.py) — see [`doc-site/README.md`](https://github.com/PlasmTools/plasm-core/blob/main/doc-site/README.md). Examples focus on **HTTP** and **GraphQL** catalogs.

<div class="plasm-docs-cloud-strip">
  <strong>OAuth apps blocked?</strong> Self-hosted OAuth (especially Google Workspace) is often operationally heavy.
  <a href="https://platform.plasm.tools" target="_blank" rel="noopener">Plasm Cloud</a> hosts OAuth provider registration and outbound connection flows for teams that prefer not to own every client ID.
</div>
