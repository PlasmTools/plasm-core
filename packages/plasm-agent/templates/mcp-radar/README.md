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

Low-friction production path (eve-aligned): `vercel.json` + catch-all `api/[[...path]].ts` mounts the same routes as local dev (channels, cron, `/plasm/v1/info`).

```bash
cd examples/mcp-radar-agent   # or your bootstrapped project
plasm-agent link
plasm-agent build
vercel env pull .env.local    # AI_GATEWAY_API_KEY, CRON_SECRET, KV, Blob
vercel deploy
curl -s "$DEPLOY_URL/channel/mcp-radar/status" | jq .
```

| Variable | Deploy |
|----------|--------|
| `AI_GATEWAY_API_KEY` | Required for agent synthesis |
| `CRON_SECRET` | Required — Vercel Cron hits `/internal/cron/mcp-radar-scan` |
| `KV_REST_API_URL` + `KV_REST_API_TOKEN` | Durable seen-items + last-run state |
| `BLOB_READ_WRITE_TOKEN` | Durable proof markdown log |
| `TAVILY_API_TOKEN` | Optional corroboration |

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
| `AI_GATEWAY_API_KEY` | Yes (live runs) | Agent model turns |
| `TAVILY_API_TOKEN` | Optional | Tavily corroboration; HN-only when unset |

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

Start headless dev server (`npm run dev:headless`), then:

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

Dedupe state (local fs): `agent/.plasm/research/seen-hn-items.json` (gitignored). On Vercel deploy, proof + dedupe use Blob + KV when env vars are set.

## Schedule

`agent/schedules/mcp-radar-scan.ts` runs every **6 hours** (`0 */6 * * *`). Vercel Cron calls `/internal/cron/mcp-radar-scan` with `Authorization: Bearer $CRON_SECRET`.

## Evals

```bash
npm run eval    # requires AI_GATEWAY_API_KEY
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
