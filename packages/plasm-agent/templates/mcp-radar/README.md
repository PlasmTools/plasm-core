# MCP Radar Agent

Event-driven research example for `@plasm_lang/vercel-agent`: watch Hacker News for **MCP** innovation signals, corroborate with **Tavily**, and publish structured entries to a live **Proof** document.

## Purpose

Demonstrates a **Plasm-first agent loop**:

- **Host surface** — channel/schedule triggers the agent; last-run metadata only (`radar-state.ts`)
- **Model surface** — federated `plasm_context` (hackernews + tavily + proof) → `plasm` → `plasm_run`

Proof has **no** host env vars. The agent **`share_link_create`**s a document (slug + share URL in the response), **`document_share_bind`**s session auth, then reads/appends via Proof catalog capabilities.

## Bootstrap from CLI

```bash
mkdir /tmp/my-radar && cd /tmp/my-radar
plasm-agent init --template mcp-radar .
npm install
plasm-agent build
npm run smoke:channel
plasm-agent dev
```

## Deploy to Vercel

```bash
plasm-agent link
node scripts/provision-vercel.mjs   # sync Tavily + tenant scope from monorepo .env
pnpm install
pnpm run build
VERCEL=1 pnpm run build && vercel deploy --prebuilt --prod
```

| Concern | On Vercel |
|---------|-----------|
| AI Gateway | OIDC via linked project — no API key |
| Tavily | `TAVILY_API_TOKEN` synced from monorepo `.env` via provision script |
| Tenant scope | `PLASM_TENANT_SCOPE` (default `mcp-radar`) synced from `.env` |
| Proof document | Created by agent via `share_link_create`; slug/URL from Plasm response — **no** `PROOF_*` env |
| Cron | Platform `x-vercel-cron` — no `CRON_SECRET` required |

## Setup

| Variable | Required | Role |
|----------|----------|------|
| `AI_GATEWAY_API_KEY` | Local only | Agent model turns off-Vercel |
| `TAVILY_API_TOKEN` | Recommended | Tavily corroboration |
| `PLASM_TENANT_SCOPE` | Optional | Session/operator partition (default `mcp-radar`) |

## Stable session intent

```
track MCP innovations from Hacker News and corroborate with Tavily web search
```

Federated seeds: `hackernews:Item`, `tavily:SearchResult`, `proof:ShareLink`
