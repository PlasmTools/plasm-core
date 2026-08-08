# Plasm

[![GitHub Actions](https://github.com/PlasmTools/plasm-core/actions/workflows/docs.yml/badge.svg?branch=main)](https://github.com/PlasmTools/plasm-core/actions/workflows/docs.yml)

**[Documentation (GitHub Pages)](https://plasmtools.github.io/plasm-core/)** · **[Source](https://github.com/PlasmTools/plasm-core)**

Plasm is a **typed capability graph** (CGS), **wire mappings** (CML), and a **path-expression language** agents use against real APIs: validate before transport, compact session symbols, HTTP and MCP hosts, and **42 curated integration catalogs** under [`apis/`](apis/) (see the [full catalog table](apis/README.md#catalog)). Deep dives—**`plasm-server`**, **`plasm`** remote terminal, execute semantics, authoring, views, schema overlays, and env flags—live in the **[documentation site](https://plasmtools.github.io/plasm-core/)**.

### Why this exists

Most agent stacks still center on **ad hoc JSON tools**: large schemas in context, fragile emitted payloads, and no shared model of entities and relations across vendors. If you want the **motivation and framing** for a typed interaction layer instead—one graph-shaped contract, validation before wire calls, and a path language that stays stable as you federate catalogs—read **[Plasm: a typed interaction layer for agents working across APIs](https://medium.com/@ryansroberts/plasm-a-typed-interaction-layer-for-agents-working-across-apis-38d9d90066a7)** (Medium). This repo and the docs site are where that story meets the implementation.

## Quick start

Prerequisites: **Rust** (`cargo`). Optional: **Just**, **Elixir** (for downstream Phoenix workflows).

**OSS appliance (native binary)** — **`plasm-server`** (Cargo package `plasm-server`): in-process kernel, HTTP + MCP, optional Ratatui UI. No OSS container image is shipped from this repo; run with Cargo:

```bash
cargo build -p plasm-server --release
cargo run -p plasm-server --release -- \
  --schema fixtures/schemas/capability_with_input
```

After **`install.sh`**, run **`plasm-server`** with no flags. From a checkout with packed plugins, pass **`--plugin-dir target/plasm-plugins`** (see **[Plugins & compile pipeline](https://plasmtools.github.io/plasm-core/reference/genco-plugin-pipeline/)**). **`--no-tui`** runs headless. For `project_mcp_*` persistence run **`plasm-server mcp migrate-db`**. Details: [Appliance quick start](https://plasmtools.github.io/plasm-core/appliance/quickstart/).

**Remote client:** install **`plasm`** from [plasm.tools/get](https://plasm.tools/get/) and follow [Remote terminal](https://plasmtools.github.io/plasm-core/reference/plasm-cgs-remote-terminal/).

Full flags, `/execute`, MCP tools, plugins, and catalog workflows are covered in **[the documentation](https://plasmtools.github.io/plasm-core/)**; contributor-oriented commands and boundaries are summarized in [`AGENTS.md`](AGENTS.md). Doc sources: [`doc-site/`](doc-site/README.md).

## API catalogs

Split CGS + CML trees live under [`apis/`](apis/README.md). The links below point at catalogs whose **own README** is the source of truth for **how to run**, **auth env vars**, and **stated scope**—many also spell out **`plasm-eval coverage`** or **`plasm schema validate`** flows. They are the usual “complete enough to trust the README” set, not an exhaustive inventory (see the **[full catalog table](apis/README.md#catalog)** for every directory). **Capability counts** in parentheses are the number of entries under `capabilities:` in that catalog’s `domain.yaml` (what the runtime loads).

**Public (no API key):** [dnd5e](apis/dnd5e/README.md) (60) · [pokeapi](apis/pokeapi/README.md) (97) · [graphqlzero](apis/graphqlzero/README.md) (15) · [hackernews](apis/hackernews/README.md) (8) · [openbrewerydb](apis/openbrewerydb/README.md) (5) · [rickandmorty](apis/rickandmorty/README.md) (6) · [xkcd](apis/xkcd/README.md) (1) · [rawg](apis/rawg/README.md) (2; optional key for rate limits) · [openmeteo](apis/openmeteo/README.md) (1)

**Auth’d integrations (README + eval / validate where noted):** [github](apis/github/README.md) (91) · [clickup](apis/clickup/README.md) (85) · [notion](apis/notion/README.md) (14) · [linear](apis/linear/README.md) (27; [`COVERAGE.md`](apis/linear/COVERAGE.md)) · [gitlab](apis/gitlab/README.md) (42) · [slack](apis/slack/README.md) (57) · [discord](apis/discord/README.md) (135) · [spotify](apis/spotify/README.md) (17) · [reddit](apis/reddit/README.md) (11) · [twitter / X](apis/twitter/README.md) (15) · [tavily](apis/tavily/README.md) (5) · [musixmatch](apis/musixmatch/README.md) (7)

**Google Workspace** (OAuth; each README lists scopes and coverage commands): [gmail](apis/gmail/README.md) (30) · [calendar](apis/google-calendar/README.md) (4) · [docs](apis/google-docs/README.md) (3) · [drive](apis/google-drive/README.md) (45) · [sheets](apis/google-sheets/README.md) (17)

**On-chain (native transport):** [evm-erc20](apis/evm-erc20/README.md) (2) — **intentionally narrow** (ERC-20 balance + `Transfer` logs): it exists to **demonstrate the interface** when the mapping target is **native EVM** (JSON-RPC to a chain URL), not OpenAPI/GraphQL over HTTP. Plasm is **not** HTTP-only—HTTP and GraphQL are the common catalog shapes today; this catalog shows the **same CGS → CML → runtime path** on a **non-HTTP wire**. Broader on-chain surfaces are orthogonal to proving that transport seam. Enable the **`evm`** Cargo feature (see README).

## Observability (two lanes)

**OTEL ≠ stored execution trace.** Do not merge these.

| Lane | Crates | Role |
|------|--------|------|
| **OTLP export** | `plasm-otel` | Spans/metrics/logs via standard `OTEL_*` env |
| **Execution trace** | `plasm-trace-wire` + `plasm-trace` | Wire DTOs + MCP/execute session timeline (TraceHub) |
| **Durable sink** (SaaS ops binary) | `plasm-trace-sink` | Optional Iceberg HTTP ingest when `PLASM_TRACE_SINK_URL` is set — Helm subchart; not a Cargo dep of the appliance |

OSS appliance and agent paths use otel + trace (+ wire). The sink is product/deploy optional and often `--exclude`d from local OSS CI builds (Iceberg/datafusion drift).

## License

Plasm is licensed under the [Business Source License 1.1](LICENSE) (`BUSL-1.1`).
GitHub’s license API often reports `NOASSERTION` for BSL — that is a detector gap, not an
unlicensed tree. The Change License is Apache License 2.0 on the Change Date in `LICENSE`.
