# plasm-mcp incoming (inbound) authentication

**Architecture context:** `saas-architecture.md` (§0.4 — inbound identity vs MCP transport vs outbound provider auth).

`plasm-mcp` can require **JWT** (`Authorization: Bearer`) and/or **API keys** (`X-API-Key`) for HTTP discovery/execute routes and for MCP tools (see below).

Outbound CGS/API credentials are unchanged (`AuthResolver`, `AuthScheme` in YAML).

## Environment variables

| Variable | Values | Meaning |
|----------|--------|---------|
| `PLASM_INCOMING_AUTH_MODE` | `off` (default), `optional`, `required` | Whether requests must present credentials. |
| `PLASM_AUTH_JWT_SECRET` | string | HMAC key for **HS256** JWTs. |
| `PLASM_AUTH_JWT_ISSUER` | optional string | If set, JWT `iss` must match. |
| `PLASM_AUTH_JWT_AUDIENCE` | optional string | If set, JWT `aud` must match. |
| `PLASM_AUTH_API_KEYS_FILE` | path to JSON file | API keys for inbound auth (see format). |

Startup fails fast if `PLASM_INCOMING_AUTH_MODE=required` but neither `PLASM_AUTH_JWT_SECRET` nor `PLASM_AUTH_API_KEYS_FILE` is set.

## JWT claims (HS256)

Required:

- `sub` — subject
- `tenant_id` (alias `tid`) — tenant scope for execute sessions

Standard `exp` is validated.

## API key file format

JSON array of objects:

```json
[
  { "key": "pk_example", "tenant_id": "tenant-a", "subject": "key-a" }
]
```

Keys are compared in **constant time** against the raw `X-API-Key` header value.

## HTTP

Protected routes: `/v1/registry`, `/v1/registry/:id`, `/v1/discover`, `/v1/incoming-auth/context`, `/execute`, `/execute/...`

Public: `GET /v1/health`

Execute sessions are keyed by **tenant scope** from the principal; cross-tenant access to an existing session returns **403**.

## MCP

**Tenant MCP transport** (graph allowlists pushed from the control plane) is separate: use a provisioned **API key** as `Authorization: Bearer <api_key>` on Streamable HTTP. See ``docs/plasm-mcp-tenant-configuration.md``.

**Incoming (inbound) auth** for execute sessions: Streamable HTTP does not pass `Authorization` to tool handlers, so clients must call the tool **`plasm_incoming_auth`** once per MCP transport session with **exactly one** of:

- `bearer_token` — raw JWT string
- `api_key` — raw API key string

When `PLASM_INCOMING_AUTH_MODE=required`, other tools fail until `plasm_incoming_auth` succeeds.

## Dev JWT helper

From the repo root (requires `PLASM_AUTH_JWT_SECRET`):

```bash
./scripts/plasm-dev-auth.sh mint-jwt --tenant my-tenant --sub my-user
```

## Phoenix `web/` UI

For local development, **`just local-web`** exports **`PLASM_WEB_DEV_AUTO_BEARER_TOKEN`** so Phoenix seeds the **browser session** (see `PlasmWebWeb.DevAutoIncomingAuth`) — no manual paste page. With **`PLASM_WEB_INCOMING_LOGIN_MODE=oauth_github`**, sign in via **`/login`** (GitHub OAuth).

The SaaS shell resolves tenant/workspace/project from **`GET /v1/incoming-auth/context`** (Rust-owned principal + workspace/project list). Phoenix does not maintain parallel user or membership tables for that flow.

Configure `PLASM_MCP_HTTP_BASE_URL` (see `web/config/runtime.exs`, default `http://127.0.0.1:3000` with `just local-web`; see `env-profiles.md`).

## CLI device login (SaaS / hosted platform)

Headless **`plasm login`** and platform **`plasm init`** use RFC 8628-style device authorization against the **incoming-auth** HTTP API (execute JWT), not MCP Streamable HTTP OAuth (`/mcp/oauth/*`).

| Step | Endpoint / URL |
|------|----------------|
| CLI starts session | `POST {server}/v1/incoming-auth/device/start` |
| User opens browser | `{PLASM_PUBLIC_WEB_ORIGIN}/device?user_code=XXXX-XXXX` (paste-friendly single field, then GitHub) |
| Phoenix completes | `POST /internal/incoming-auth/v1/device/complete` (control-plane secret) |
| CLI polls token | `POST {server}/v1/incoming-auth/device/poll` |

**Hosted `server` URL:** use the agent HTTP prefix on the public host, e.g. **`https://platform.plasm.tools/plasm/http`** (not bare `https://platform.plasm.tools` — ingress serves `/plasm/http/…` → agent `/v1/…`).

| Variable | Role |
|----------|------|
| `PLASM_PUBLIC_WEB_ORIGIN` | Phoenix origin for `verification_uri` (e.g. `https://platform.plasm.tools`). Set on **plasm-mcp** in Helm via `plasmWebPublicBaseUrl`. |
| `PLASM_AUTH_STORAGE_URL` | **Required** for device sessions (auth-framework KV; multi-replica safe). |
| `PLASM_AUTH_JWT_SECRET` | Signs polled `access_token` JWTs. |
| `PLASM_MCP_CONTROL_PLANE_SECRET` | Phoenix → agent `device/complete`. |

Device sessions are stored only in **auth-framework KV** (no in-process fallback). If `auth_storage` is unset, device routes return **503** `device_auth_storage_unavailable`.
