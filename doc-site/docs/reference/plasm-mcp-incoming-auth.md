# plasm-mcp incoming (inbound) authentication

Incoming auth scopes **execute sessions** by tenant principal. It is separate from:

1. **MCP transport auth** — Bearer API keys on Streamable HTTP when `project_mcp_*` rows exist ([oss-appliance-mcp-persistence.md](oss-appliance-mcp-persistence.md))
2. **Outbound catalog auth** — `AuthResolver` / `AuthScheme` in YAML for vendor APIs

`plasm-mcp` / `plasm-server` can require **JWT** (`Authorization: Bearer`) and/or **API keys** (`X-API-Key`) for HTTP discovery/execute routes and for MCP tools (see below).

## Environment variables

| Variable | Values | Meaning |
|----------|--------|---------|
| `PLASM_INCOMING_AUTH_MODE` | `off` (default), `optional`, `required` | Whether requests must present credentials. |
| `PLASM_AUTH_JWT_SECRET` | string | HMAC key for **HS256** JWTs. |
| `PLASM_AUTH_JWT_ISSUER` | optional string | If set, JWT `iss` must match. |
| `PLASM_AUTH_JWT_AUDIENCE` | optional string | If set, JWT `aud` must match. |
| `PLASM_AUTH_API_KEYS_FILE` | path to JSON file | API keys for inbound auth (see format). |

Startup fails fast if `PLASM_INCOMING_AUTH_MODE=required` but neither `PLASM_AUTH_JWT_SECRET` nor `PLASM_AUTH_API_KEYS_FILE` is set.

!!! note "OSS appliance"

    Default OSS `plasm-mcp` / appliance HTTP+MCP startup does **not** require incoming JWT. Appliance MCP transport keys are the usual local gate. Hosted product binaries may enable `auth-framework` + incoming auth by default.

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

**Tenant MCP transport** is separate: use a provisioned **API key** as `Authorization: Bearer <api_key>` on Streamable HTTP. See [oss-appliance-mcp-persistence.md](oss-appliance-mcp-persistence.md).

**Incoming (inbound) auth** for execute sessions: Streamable HTTP does not pass `Authorization` to tool handlers, so clients must call the tool **`plasm_incoming_auth`** once per MCP transport session with **exactly one** of:

- `bearer_token` — raw JWT string
- `api_key` — raw API key string

When `PLASM_INCOMING_AUTH_MODE=required`, other tools fail until `plasm_incoming_auth` succeeds. On hosts where incoming auth is disabled, the tool is omitted.

## Dev JWT helper

From a monorepo checkout that ships the helper (requires `PLASM_AUTH_JWT_SECRET`):

```bash
./scripts/plasm-dev-auth.sh mint-jwt --tenant my-tenant --sub my-user
```

## Hosted-only notes (optional)

Private control-plane / browser shells may mint JWTs for local UX and call `GET /v1/incoming-auth/context` for tenant resolution. Device-login (`POST /v1/incoming-auth/device/*`) and public web origins are **hosted** concerns — not required for the OSS appliance. When those routes are enabled, they need `PLASM_AUTH_STORAGE_URL` (auth-framework KV) and a signed `PLASM_AUTH_JWT_SECRET`.
