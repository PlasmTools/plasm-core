# @plasm_lang/vercel-agent

Catalog-native TypeScript agent framework for plasm-oss. Capability is authored once as **CGS/CML catalogs**; the framework projects two surfaces from that single source:

- **Model surface** — Plasm language tools (`discover` → `plasm_context` → `plasm` → `plasm_run`) with teaching TSV in tool results
- **Code surface** — generated TypeScript stubs under `agent/.plasm/stubs/` (CGS-typed params + return types; `npm run build:stubs`)

There is **no `tools/` directory**. See the architecture plan (`.cursor/plans/eve_plasm_language_layer_*.plan.md` in the parent monorepo) for full thesis.

## Layout

```
packages/plasm-agent/
  src/                    # framework library
  agent/                  # example / template agent project
    agent.ts              # model + runtime config (AI Gateway slugs)
    instructions.md       # always-on system prompt
    catalogs/             # authored domain.yaml + mappings.yaml
    .plasm/stubs/         # generated TS clients (build artifact)
    skills/ channels/ schedules/ subagents/ hooks/
  evals/                  # native eval harness (*.eval.ts)
```

## Core modules

| Module | Role |
|--------|------|
| `runtime/plasm-agent.ts` | `PlasmAgent` — AI SDK `generateText` tool loop, AI Gateway model slug, OTEL |
| `tools/plasm-tools.ts` | Four MCP-shaped language tools (Zod + `tool()`) |
| `runtime/agent-runtime.ts` | Engine + session orchestration for tool handlers |
| `session-state.ts` | Durable session mirror (`agent/.plasm/sessions/`, teaching.tsv) |
| `symbol-registry.ts` | Agent-global monotonic `e#/m#/p#/r#` mirror |
| `catalog/loader.ts` | Filesystem catalog discovery |
| `engine/napi-binding.ts` | `NapiPlasmEngine` / `StubPlasmEngine` |
| `instrumentation.ts` | `@ai-sdk/otel` on every model call |
| `operator/routes.ts` | Nitro operator stubs |

## Tool loop (production-shaped)

MCP parity, Vercel AI SDK v6:

```
discover_capabilities → plasm_context → plasm → plasm_run
```

- Teaching TSV returned as **tool result markdown** (history-resident)
- Stable `intent` per goal → deterministic `logical_session_ref` (`l_<token>`)
- `plasm` dry-run registers `pcN` in the NAPI engine; `plasm_run` validates it
- **Live HTTP execute** still requires `HostTransportFn` (Vercel Connect) — next phase

```ts
import { PlasmAgent } from "@plasm_lang/vercel-agent";
import { createPlasmAgent } from "@plasm_lang/vercel-agent/agent";

const agent = await createPlasmAgent();
const result = await agent.generate("List products and summarize");
console.log(result.text);
```

```bash
PLASM_AGENT_MODEL=anthropic/claude-sonnet-4.6 npm run agent -- "list products"
```

## Engine

Rust **plasm-core / plasm-agent-core / plasm-runtime** via NAPI (`@plasm_lang/engine`).

```bash
# Build native addon (required for NAPI mode)
cd packages/plasm-engine && npm install && npm run build

cd ../plasm-agent && npm install
```

`createEngine()` loads `@plasm_lang/engine` when the `.node` binary is present; otherwise falls back to `StubPlasmEngine`.

| Class | Role |
|-------|------|
| `NapiPlasmEngine` | `loadCatalog`, `discover`, `exposeSeeds`, `dryRun` → `pcN`, `runPlan` (validates `pcN`) |
| `StubPlasmEngine` | Placeholder when native binding unavailable |

**Host transport:** env bearer → Vercel Connect `getToken()` → `fetch`. Fixture mock when `PLASM_AGENT_MOCK_HTTP=1` or no bearer and no connector configured.

## Phased next steps

1. ~~**NAPI binding crate**~~ — `plasm-node` + `@plasm_lang/engine`
2. ~~**AI SDK tool loop**~~ — four language tools + `PlasmAgent` + session persistence
3. ~~**Live execute**~~ — Promise host transport + Connect broker
4. ~~**Prod durability**~~ — `defineState` (fs/kv/postgres) + Blob/KV archive adapters + Workflow bootstrap
5. ~~**TS stub generator**~~ — CGS introspection + deterministic `e#` programs
6. ~~**eve devx scaffold**~~ — `defineChannel` / `defineSchedule` / `defineHook` / `defineSkill` loaders, hot-reload dev server, native `evals/`, **`plasm-agent` CLI** (`init` / `info` / `link` / `build` / `dev`), session SSE
7. ~~**Terminal client**~~ — TypeScript TUI over `/plasm/v1/session` + stream (`plasm-agent dev` TTY, `plasm-agent chat`)
8. **Operator UI** — React panes (plan-ui / run-explorer embed)
9. **Channels (Connect)** — `authorization.required` stream events

## Storage (`defineState` + archives)

| Concern | Dev (`fs`) | Prod |
|---------|------------|------|
| Sessions / symbols | `agent/.plasm/` | `PLASM_STATE_BACKEND=kv` or `postgres` |
| Run/trace archives | local dirs | `BLOB_READ_WRITE_TOKEN` + `KV_REST_API_*` |

See **Connect / state / archive env** below.

Parent monorepo references:

- [docs/oss-core-trace-artifacts.md](../../../docs/oss-core-trace-artifacts.md) — local trace/run archive env vars
- [docs/oss-core-ui-surface.md](../../../docs/oss-core-ui-surface.md) — OSS HTTP surfaces the operator UI can proxy

## Operator UI

Route stubs under `src/operator/routes.ts` (`GET /operator/catalogs`, `/sessions`, `/plans`, `/runs`, `/traces`, `/archives`). Mount via Nitro:

```ts
import { nitroOperatorHandler } from "@plasm_lang/vercel-agent/operator";

export default nitroOperatorHandler({ agentRoot: "./agent" });
```

Full UI is out of scope for this scaffold; reuse patterns from `apps/plan-ui/` and `apps/run-explorer-ui/` in the parent monorepo where appropriate.

## CLI (`plasm-agent`)

TypeScript bootstrap for this framework — **not** the Rust `plasm-server` Ratatui TUI.

```bash
npm run plasm-agent -- init ./my-agent   # scaffold agent/, catalogs/, evals/
npm run plasm-agent -- info [--json]     # project + stub diagnostics
npm run plasm-agent -- link              # vercel link + env pull (AI Gateway key)
npm run plasm-agent -- build             # stubs + agent/.plasm/discovery/manifest.json
npm run plasm-agent -- dev [--no-tui]   # server + TUI when TTY
npm run plasm-agent -- chat --url URL   # attach to running server
```

Installed globally or via `npx`: `plasm-agent <command>` (bin entry in package.json).

## Scripts

```bash
cd packages/plasm-agent
npm install
npm run build:stubs
npm run typecheck
npm run dev                      # hot-reload dev server + operator + channel routes
npm run eval                     # live LLM evals (AI Gateway required)
npm run eval:catalog             # plasm-eval bridge (OPENROUTER_API_KEY)
npm run smoke:hot-reload
npm run smoke:sessions
npm run smoke:tui-async
npm run smoke:stubs
npm run smoke:stubs:matrix
npm run agent -- "your prompt"   # requires AI_GATEWAY_API_KEY (Vercel AI Gateway)
```

### Dev server (`plasm-agent dev`)

**Default:** `plasm-agent dev` runs programmatic **Nitro v3** (same host as Vercel). `plasm-agent build` emits **`.vercel/output`** on Vercel (Build Output API) or **`.plasm/nitro-output`** locally, plus **`.plasm/agent-summary.json`** for Agent Observability.

```bash
plasm-agent dev                    # Nitro (default)
plasm-agent dev --interactive      # in-process server + TUI when TTY
plasm-agent dev --interactive --no-tui
plasm-agent chat --url http://127.0.0.1:3000
```

Slash commands in the TUI: `/help`, `/info`, `/model`, `/channels`, `/catalogs`, `/new`, `/quit`.

- `GET /plasm/v1/info` — discovery + `loadedSlots` summary (`dev` field for routes/model)
- `POST /plasm/v1/session` — agent turn; `wait: false` returns 202 immediately (attach SSE)
- `POST /plasm/v1/session/:id` — continue (requires `continuationToken` + `message`)
- `GET /plasm/v1/session/:id/stream` — SSE (`turn:start`, `turn:step`, `turn:finish` / `turn:error`)
- `GET /operator` — read-only operator shell
- Channel routes from `agent/channels/*.ts` (e.g. `GET /channel/health`, `POST /channel/execute-tiny/products`)

Hot reload watches `catalogs/`, `skills/`, `channels/`, `schedules/`, `hooks/`, and `instructions.md`.

### Authoring slots

| Slot | Helper | Notes |
|------|--------|-------|
| `skills/` | markdown or `defineSkill()` | Progressive index + `read_skill` tool (`experimental.skills: "inline"` for full inject) |
| `channels/` | `defineChannel()` | HTTP ingress; call stubs, not `plasm_context` |
| `schedules/` | `defineSchedule()` | Dev: `*/N * * * *` timers; prod: `GET /internal/cron/:name` + Vercel Cron manifest in `/plasm/v1/info` |
| `hooks/` | `defineHook()` | `agent:start`, `plan:commit`, `run:complete`, … |
| `subagents/` | `defineAgent()` in `subagents/<name>/agent.ts` | `delegate_subagent` harness tool |

### Evals

**`npm run eval`** — live LLM evals only (requires `AI_GATEWAY_API_KEY`). Fixture-only tool-chain evals were removed.

**`npm run eval:catalog`** — bridge to `plasm-eval` + `apis/<api>/eval/cases.yaml` (OpenRouter + BAML expression eval).

### Vercel AI Gateway (local + deploy)

This framework uses the [Vercel AI SDK `gateway()`](https://ai-sdk.dev/providers/ai-sdk-providers/ai-gateway) provider only — no direct OpenRouter/OpenAI keys in the agent layer.

1. **Vercel team billing** — AI Gateway returns `customer_verification_required` until a valid payment method is on the team. Add a card under [Vercel → AI](https://vercel.com/docs/ai-gateway) (unlocks free credits).
2. **API key** — Vercel dashboard → **AI** → **Gateway** → **API Keys** → create key.
3. **Local env** — put in `plasm-oss/.env` (shell `export` lines are supported):

   ```bash
   export AI_GATEWAY_API_KEY=vck_...
   ```

   Aliases `AI_API_GATEWAY_KEY` / `AI_GATEWAY_KEY` are mapped automatically. Or link the project and run `vercel env pull .env.local`.
4. **Run:**

   ```bash
   npm run smoke:tools
   npm run smoke:async-transport
   npm run agent -- "list products from execute_tiny"
   ```

### Vercel Connect (outbound catalog HTTP)

```bash
export PLASM_CONNECTOR_<ENTRY>=provider/connector-uid   # e.g. PLASM_CONNECTOR_GITHUB
export PLASM_CONNECT_SUBJECT_TYPE=app                  # or user + PLASM_CONNECT_USER_ID
# Local: vercel link && vercel env pull → VERCEL_OIDC_TOKEN
```

### Production state + Workflow

```bash
export PLASM_STATE_BACKEND=kv|postgres|fs
export PLASM_ARCHIVE_BACKEND=vercel|postgres|local
export BLOB_READ_WRITE_TOKEN=...
export KV_REST_API_URL=...
export KV_REST_API_TOKEN=...

# Workflow (agent.ts experimental.workflow.world.type)
export PLASM_WORKFLOW_WORLD=postgres
export WORKFLOW_POSTGRES_URL=postgres://...
```

**Prod fallback (future):** sidecar `plasm-server` accepting resolved plans over HTTP `/execute`.

## Open decisions

- ~~Package publish name~~ — **`@plasm_lang/vercel-agent`** (resolved)
- Prod execution default: force-bundled NAPI vs Vercel Rust Function sidecar
- Catalog wire format: raw YAML vs precompiled `catalog.cgs.json` IR at build time
