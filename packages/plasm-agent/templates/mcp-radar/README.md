# MCP Radar Agent

Event-driven research example for [`@plasm_lang/vercel-agent`](../../plasm-oss/packages/plasm-agent/): watch Hacker News for **MCP** innovation signals, corroborate with **Tavily**, and append structured entries to a maintained proof log.

## Purpose

Demonstrates Plasm’s **dual surface**:

- **Code surface** — channel/schedule handlers preflight HN via typed stubs and own proof persistence
- **Model surface** — federated `plasm_context` (hackernews + tavily) → `plasm` → `plasm_run` for synthesis

## Bootstrap from CLI

Scaffold a fresh copy (verified by `npm run smoke:bootstrap` in the framework package):

```bash
mkdir /tmp/my-radar && cd /tmp/my-radar
plasm-agent init --template mcp-radar .
npm install
plasm-agent build
npm run smoke:channel
plasm-agent dev
```

The canonical template source is this directory (`examples/mcp-radar-agent/`).

## Deploy to Vercel

Canonical stack: **AI SDK v7** (`ai@7.0.14`, `@ai-sdk/otel@1.0.14`) + **Workflow 4.5** + pinned `zod@4.3.6`. Use **pnpm** (lockfile committed).

```bash
cd examples/mcp-radar-agent
plasm-agent link
node scripts/provision-vercel.mjs   # sync monorepo .env secrets + Blob store
pnpm install
pnpm run build                      # local Nitro output
# Remote build on Vercel requires @plasm_lang/vercel-agent@0.3.114+ on npm
VERCEL=1 pnpm run build && vercel deploy --prebuilt --prod   # monorepo prebuilt path
curl -s "$DEPLOY_URL/channel/mcp-radar/status" | jq .
```

| Concern | On Vercel |
|---------|-----------|
| AI Gateway | OIDC via linked project — no API key |
| Tavily | `TAVILY_API_TOKEN` synced from monorepo `.env` via provision script |
| Tenant scope | `PLASM_TENANT_SCOPE` (default `mcp-radar`) synced from `.env` |
| Proof log + dedupe state | `@vercel/blob` only (markdown + JSON objects) |
| Cron | Platform `x-vercel-cron` — no `CRON_SECRET` required |
| Local dev | monorepo `.env` loaded automatically; optional `AI_GATEWAY_API_KEY` off-Vercel |

`vercel blob create-store` wires Blob to the project; runtime auth is OIDC (same model as [eve content agent](https://github.com/vercel-labs/eve-content-agent-template)).

On Vercel, `POST /channel/mcp-radar/run` returns `202` and continues in the background via `waitUntil`. Locally it runs synchronously for smoke tests.

Monorepo deploy: set Vercel **Root Directory** to `examples/mcp-radar-agent`.

## Setup (monorepo checkout)

```bash
cd examples/mcp-radar-agent
npm install
cp .env.example .env.local
```

| Variable | Required | Role |
|----------|----------|------|
| `AI_GATEWAY_API_KEY` | Local only | Agent model turns off-Vercel |
| `TAVILY_API_TOKEN` | Recommended | Tavily corroboration; set in monorepo `.env`, synced to Vercel via `provision-vercel.mjs` |
| `PLASM_TENANT_SCOPE` | Optional | Session/operator partition (default `mcp-radar`) |

Build CGS stubs (symlinked catalogs):

```bash
npm run build
```

## Dev

```bash
npm run build
npm run dev              # Nitro dev server — Vercel routing parity (channels, cron, /plasm/v1/*)
npm run dev:interactive  # optional: in-process server + TUI + sessions + hot reload
```

`plasm-agent dev` starts Nitro by default so channel routes like `/channel/mcp-radar/status` match production.

Slash commands in the interactive TUI: `/info`, `/catalogs`, `/new`, `/quit`.

## Channel API

Start headless dev server (`npm run dev`), then:

```bash
BASE=http://127.0.0.1:3000

# Status
curl -s "$BASE/channel/mcp-radar/status" | jq .

# Read proof log (markdown)
curl -s "$BASE/channel/mcp-radar/proof"

# Trigger one scan (dedupes seen HN ids)
curl -s -X POST "$BASE/channel/mcp-radar/run" -H 'content-type: application/json' -d '{}'

# Force scan even when no new ids
curl -s -X POST "$BASE/channel/mcp-radar/run" -H 'content-type: application/json' -d '{"force":true}'
```

Proof artifact (local fs): [`agent/research/mcp-innovations-proof.md`](./agent/research/mcp-innovations-proof.md)

Dedupe state (local fs): `agent/.plasm/research/seen-hn-items.json` (gitignored). On Vercel, proof + dedupe + last-run use Blob paths under `research/`.

## Schedule

`agent/schedules/mcp-radar-scan.ts` runs every **6 hours** (`0 */6 * * *`) as a **Nitro scheduled task** (Vercel Cron auto-wired at deploy). Durable execution uses **Vercel Workflows** (`workflow/api` `start()`).

Manual dev trigger: `POST /internal/schedule/mcp-radar-scan`

## Evals

```bash
npm run eval    # requires AI_GATEWAY_API_KEY locally
```

Live eval: `evals/mcp-radar-discover.eval.ts`

## Smoke

```bash
npm run build
npm run smoke:channel
```

## Catalogs

Symlinked from monorepo `apis/`:

- `agent/catalogs/hackernews` → `plasm-oss/apis/hackernews`
- `agent/catalogs/tavily` → `plasm-oss/apis/tavily`

## Stable session intent

```
track MCP innovations from Hacker News and corroborate with Tavily web search
```

Federated seeds: `hackernews:Item`, `tavily:SearchResult`
