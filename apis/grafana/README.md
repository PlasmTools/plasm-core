# Grafana API catalog

Curated CGS for Grafana 9+ HTTP API — maps the core surface of
[grafana/mcp-grafana](https://github.com/grafana/mcp-grafana) onto Plasm entities
instead of ~70 MCP tools.

## Scope (v5)

| Area | Coverage |
|------|----------|
| **Core (v1–2)** | Dashboard, Folder, Datasource, Annotation, AlertRule, alerting routing, teams, org users, library elements |
| **RBAC** | Roles CRUD, role assignments, user/team role assign/set/revoke, resource permissions, resource type metadata |
| **Datasource explorers** | Prometheus labels/metrics, Loki labels/patterns/stats, ClickHouse SQL/tables/columns — actions project **`discovery_data`** (`path: data`) |
| **Navigation** | Frontend settings (`appUrl`), deeplink compose view, panel PNG render (`/render/…`) |
| **Panel queries** | `panel_query_run` on Datasource when `expr` is known from `dashboard_get` panel targets |
| **Sift** | Investigations list/get/create, analyses list (`grafana-ml-app` plugin) |
| **Incident** | Query/get/create + activity notes (`grafana-irm-app` RPC under `/plugins/…/resources/api/v1/`) |
| **OnCall** | Schedules, shifts, users, teams, alert groups (`grafana-irm-app` plugin proxy; `state` → `status` filter) |

PromQL / LogQL for LGTM datasources use **`datasource_query_run`** or **`panel_query_run`**
on **Datasource** (`POST /ds/query`).

### Plugin / cloud requirements

| Feature | Requires |
|---------|----------|
| Sift | `grafana-ml-app` plugin (Grafana Cloud or self-hosted with ML app) |
| Incident | `grafana-irm-app` plugin + Incident API |
| OnCall | `grafana-irm-app` plugin + OnCall backend URL in plugin settings |
| Panel render | Grafana Image Renderer service |
| RBAC writes | Enterprise / Cloud RBAC + `permissions:type:delegate` scopes |

Local **`grafana/otel-lgtm`** supports core API, Prometheus/Loki explorers, and unified query;
plugin routes return 404 unless those apps are installed.

## Auth

Service account token (recommended):

```bash
export GRAFANA_URL=http://localhost:3000
export GRAFANA_SERVICE_ACCOUNT_TOKEN=glsa_...
```

Local LGTM stack (`grafana/otel-lgtm`): default login is `admin` / `admin` — create
a service account token in **Administration → Service accounts**. Use **Editor** (or
granular scopes) when testing alerting routing, teams, RBAC, or org user admin.

## Local dev (LGTM)

```bash
docker run -d --name plasm-lgtm -p 3000:3000 grafana/otel-lgtm:latest
export GRAFANA_SERVICE_ACCOUNT_TOKEN=...
```

## Validate

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/grafana
cargo run -p plasm-eval -- coverage --schema apis/grafana --cases apis/grafana/eval/cases.yaml
```

## Eval

NL goals live in [`eval/cases.yaml`](eval/cases.yaml). Run deterministic form coverage (no LLM) with the command above.

## REPL (live)

```bash
cargo run -p plasm-repl -- --schema apis/grafana --backend http://localhost:3000/api
```

Examples (`e#` / `m#` from `:help`, **wire names** for params — slot numbers shift with catalog version):

```text
e7("prometheus").m27(expr="up", from="now-5m", to="now")     # PromQL via /ds/query
e7("prometheus").m??()[discovery_data]                              # explorer label/metric names
e6{dashboard_uid=e5("…")}                                          # dashboard summary view
e??{resource_type="dashboard", dashboard_uid="…"}           # deeplink compose (appUrl + scope)
e7("prometheus").m??(expr="up", panel_id=2, dashboard_uid="…")  # panel_query_run
```

### `run_panel_query` workflow

MCP `run_panel_query` parses panel targets from dashboard JSON client-side. In Plasm:

1. `dashboard_get` — read panel `targets[].expr` and datasource uid/type
2. `panel_query_run` (or `datasource_query_run`) with the extracted `expr`

### Deeplinks

`deeplink_generate_query` (view `deeplink_generate`) loads `app_url` from `GET /frontend/settings` and
returns an assembled **`url`** (MCP [`generate_deeplink`](https://github.com/grafana/mcp-grafana/blob/main/tools/navigation.go) parity) plus echo scope fields.

| `resource_type` | Required scope | Result |
|-----------------|----------------|--------|
| `dashboard` | `dashboard_uid` | `{app_url}/d/{uid}?from=…&to=…` |
| `panel` | `dashboard_uid`, `panel_id` | `{app_url}/d/{uid}?viewPanel={id}&from=…&to=…` |
| `explore` | `datasource_uid` | `{app_url}/explore?left={urlencoded JSON}` |

Time params use core `wire_time('unix_ms')` via view templates: relative tokens (`now`, `now-1h`) and
all-digit strings pass through; NL/ISO/RFC3339 inputs encode to epoch-ms strings.

**REPL example** (symbolic slots from `:help`):

```text
NavigationLink{resource_type="dashboard",dashboard_uid="otel-demo",from="now-1h",to="now"}[url]
```

Preflight on **`datasource_query_run`** and explorer actions runs `datasource_get` to hydrate `type` and `uid`
before posting to `/ds/query` or datasource proxy paths.

## OpenAPI / Hermit

```bash
curl -fsSL http://localhost:3000/public/api-merged.json -o apis/grafana/openapi.json
hermit --specs apis/grafana/openapi.json --port 9090 --use-examples
```

Note: Alertmanager silences, plugin routes, Incident RPC, and `/render` may be absent from `api-merged.json`;
this catalog maps them from Grafana HTTP docs and mcp-grafana source.

## Version

Requires Grafana **9.0+** (tested on 13.x via `grafana/otel-lgtm`). Catalog **`version: 5`** — 30 entities, 102 capabilities.
