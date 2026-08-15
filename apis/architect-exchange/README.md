# Architect Exchange (AX)

Task-oriented CGS/CML catalog for Architect Exchange perpetual futures. One catalog covers both HTTP gateways.

## Backends

| Environment | Origin (no path suffix) |
|-------------|-------------------------|
| Production | `https://gateway.architect.exchange` |
| Sandbox | `https://gateway.sandbox.architect.exchange` |

CML paths start with `api` or `orders`:

- User, portfolio, market data, ledger → `/api/...`
- Order lifecycle → `/orders/...`

Do **not** set `--backend` to `…/api` or `…/orders`. Use the origin only.

Vendor OpenAPI (v15.24.0), downloaded into this directory:

- [`openapi-api-gateway.json`](openapi-api-gateway.json) — server `https://gateway.architect.exchange/api`
- [`openapi-order-gateway.json`](openapi-order-gateway.json) — server `https://gateway.architect.exchange/orders`

Docs index: <https://docs.architect.exchange/llms.txt>

## Auth

Architect mints a JWT with `POST /api/authenticate` and JSON `{api_key, api_secret, expiration_seconds}`. This is **not** OAuth client-credentials. The catalog uses:

```yaml
auth:
  scheme: bearer_token
  env: ARCHITECT_EXCHANGE_TOKEN
```

Mint a token (secrets stay out of Plasm programs):

```bash
curl -sS -X POST https://gateway.architect.exchange/api/authenticate \
  -H 'content-type: application/json' \
  -d "{\"api_key\":\"$ARCHITECT_API_KEY\",\"api_secret\":\"$ARCHITECT_API_SECRET\",\"expiration_seconds\":3600}"
```

For sandbox, use `https://gateway.sandbox.architect.exchange/api/authenticate`.

Export the returned token as `ARCHITECT_EXCHANGE_TOKEN`. `/authenticate`, Clerk login/logout, and `/health` are intentionally not capabilities.

## Out of scope

- WebSockets (`/md/ws`, `/orders/ws`)
- Clerk login / logout / health
- Admin token schemes
- Deprecated `GET /index-prices` (use underlying prices)
- Leaderboard

## Write risks

- `order_create` / `order_update` / `order_cancel` / `order_cancel_all` hit the live matching engine.
- `api_key_create` returns `api_secret` once; `api_key_delete` revokes credentials.
- `sandbox_deposit` / `sandbox_withdraw` are sandbox-only. Do not run them against production.

Prefer sandbox for writes. Preview capabilities (`order_preview`, `aggressive_limit_preview`, `initial_margin_quote`) do not place orders.

## Validate

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/architect-exchange
```

Hermit serves **one** OpenAPI file per process. Vendor specs put `/api` and `/orders` on `servers.url`, while this catalog's CML paths already include those prefixes (backend is the origin only). Pointing Hermit at a raw vendor file therefore 404s (`/api/whoami` vs `/whoami`).

For local transport checks, prefix the spec paths (or merge both files) so Hermit routes match CML. Set a dummy `ARCHITECT_EXCHANGE_TOKEN` even against Hermit — the client still injects Bearer.

```bash
# After prefixing paths to /api/... and /orders/...
hermit --specs /tmp/ax-hermit-dual.json --port 19090 --use-examples
ARCHITECT_EXCHANGE_TOKEN=dummy cargo run -p plasm-repl --features baml -- \
  --schema apis/architect-exchange --backend http://127.0.0.1:19090
```

Live/sandbox reads require a real `ARCHITECT_EXCHANGE_TOKEN`. This run skipped live and sandbox because that env var was unset.
