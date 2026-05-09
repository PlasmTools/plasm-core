# Canonical API schemas (`apis/`)

**Monorepo layout:** in the private `plasm` repo, `apis/` at the repository root is a **symlink** to this directory (`plasm-oss/apis`). Commits to API definitions belong in the **plasm-oss** / plasm-core submodule, not a duplicate `apis/` tree in the monorepo.

This directory holds **split** Plasm CGS trees: each API is a folder with `domain.yaml` + `mappings.yaml` (and a **README** describing scope, auth, and how to run `**plasm-repl`** / `**plasm-cgs`** / `**plasm-mcp`**). Wire types and shared gloss live under top-level **`values:`**; entity **fields** and capability **parameters** use **`value_ref`** into those **semantic slots** (sharing vs splitting keys is an authoring choice—see **[Value domains](../.cursor/skills/plasm-authoring/reference.md#value-domains-values-and-value_ref)** in the authoring reference). Optional **`views:`** in **`domain.yaml`** models **composed read-only** rows over existing **`query`/`get`** capabilities; matching **`mappings.yaml`** entries use **`transport: view`** (see **[Composed read views](../.cursor/skills/plasm-authoring/reference.md#composed-read-views)**). `**domain.yaml` validation:** `kind: action` requires non-empty `**provides:`** and/or `**output:`** with `**type: side_effect`** and a non-empty `**description:`** (effectful ops with no entity projection must say what they change). Authoring details: `[.cursor/skills/plasm-authoring/reference.md](../.cursor/skills/plasm-authoring/reference.md#action-output-provides-vs-outputside_effect)`.

**Fixtures:** `fixtures/schemas/` holds **test** CGS trees and tiny interchange files (`test_schema.cgs.yaml`, `capability_with_input.cgs.yaml`, plus small slices such as **[PokéAPI mini](../fixtures/schemas/pokeapi_mini/)** for Hermit e2e, integration tests, and eval). **Curated** REST (and EVM) product APIs live only under `apis/`.

**Canon:** Do not overwrite existing `apis/<name>/` trees without an explicit decision; add new APIs as new directories.

**Multi-entry runtime:** Author `**apis/<name>/`**, then pack to cdylibs with `**cargo run -p plasm-agent --bin plasm-pack-plugins -- --apis-root apis --output-dir target/plasm-plugins`** (or `**just build-plugins**`). Start `**plasm-mcp --plugin-dir target/plasm-plugins**`. **Production Docker images** pass `**--package-list deploy/packaged-apis.txt`** so only listed catalogs are built into `**/app/plugins`** (edit that file to change the release set). Omit `**--package-list`** to pack every API under `**apis/`** (local default). Images do not ship raw `**apis/**` for runtime loading.

**Federation:** A multi-entry registry lets HTTP/MCP execute sessions load **more than one** API schema in the **same** session (monotonic `e#` / `m#` / `p#`, per-catalog dispatch — **no** CGS merge). See `[docs/incremental-domain-prompts.md](../docs/incremental-domain-prompts.md#federated-sessions-multi-catalog)`.

---

## Catalog


| Directory                           | Role                                                                                                                            |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| [clickup](clickup/)                 | ClickUp REST v2 (workspaces, tasks, lists, …)                                                                                   |
| [cloudflare](cloudflare/)           | Cloudflare REST v4 (Phase 1: zones, rulesets, phase entrypoints, WAF packages; **CGS `views:`** — `SecurityOverview` / `security_overview_query`; Bearer token; Hermit slice in-tree) |
| [dnd5e](dnd5e/)                     | D&D 5e SRD public API                                                                                                           |
| [evm-erc20](evm-erc20/)             | EVM ERC-20 reads (on-chain, not REST)                                                                                           |
| [flightaware-aeroapi](flightaware-aeroapi/) | FlightAware AeroAPI v4 (`x-apikey`; airborne search + ident flight summaries; OpenAPI from FlightAware)                         |
| [github](github/)                   | GitHub REST (repos, issues, PRs, commits, branches, reviews, files—see README)                                                  |
| [graphqlzero](graphqlzero/)         | GraphQLZero public GraphQL (full JSONPlaceholder slice; `transport: graphql`, pagination, post mutations)                       |
| [hackernews](hackernews/)           | Hacker News Firebase + Algolia search (feeds, maxitem, updates, search, items, users, polls; no auth)                           |
| [gitlab](gitlab/)                   | GitLab REST v4 (projects, issues, merge requests—see README; OpenAPI in-tree)                                                   |
| [gmail](gmail/)                     | Gmail API (Google)                                                                                                              |
| [google-calendar](google-calendar/) | Google Calendar (compound keys / `key_vars`—see README)                                                                         |
| [google-docs](google-docs/)         | Google Docs API v1 (get, create, batch update; OAuth—see README)                                                                |
| [google-drive](google-drive/)       | Google Drive API v3 (files, sharing, comments, drives, changes—see README)                                                      |
| [google-sheets](google-sheets/)     | Google Sheets API v4 (values, batch, metadata; OAuth scope map—see README)                                                      |
| [jira](jira/)                       | Jira Cloud REST                                                                                                                 |
| [linkedin](linkedin/)               | LinkedIn v2 Rest.li (OIDC profile + UGC posting/query with OAuth scope mapping)                                                 |
| [linear](linear/)                   | Linear GraphQL (Relay reads + issue/comment writes; `transport: graphql`; see `COVERAGE.md`)                                    |
| [microsoft-teams](microsoft-teams/) | Microsoft Teams via Microsoft Graph v1.0 (delegated `joinedTeams` + team get; see README)                                       |
| [outlook](outlook/)                 | Outlook mailbox via Microsoft Graph v1.0 (delegated `/me` mail folders, messages, attachments; see README)                      |
| [musixmatch](musixmatch/)           | Musixmatch (lyrics as related entity)                                                                                           |
| [notion](notion/)                   | Notion (bearer auth, Markdown API, DB query → rows as `Page`, search; no block API)                                             |
| [nytimes](nytimes/)                 | NY Times developer APIs                                                                                                         |
| [omdb](omdb/)                       | OMDb (movies)                                                                                                                   |
| [openbrewerydb](openbrewerydb/)     | Open Brewery DB                                                                                                                 |
| [openmeteo](openmeteo/)             | Open-Meteo weather                                                                                                              |
| [pokeapi](pokeapi/)                 | PokéAPI (full surface)                                                                                                          |
| [reddit](reddit/)                   | Reddit OAuth (identity, subreddits, posts, thread comments, search; optional comment submit)                                    |
| [rawg](rawg/)                       | RAWG games                                                                                                                      |
| [rickandmorty](rickandmorty/)       | Rick and Morty API                                                                                                              |
| [slack](slack/)                     | Slack Web API                                                                                                                   |
| [spotify](spotify/)                 | Spotify Web API (multiple projections)                                                                                          |
| [tavily](tavily/)                   | Tavily search / extract / research                                                                                              |
| [themealdb](themealdb/)             | TheMealDB                                                                                                                       |
| [twitter](twitter/)                 | X API v2 (posts, users, lists, OAuth 2 scope map; OpenAPI in-tree)                                                              |
| [vultr](vultr/)                     | Vultr public HTTP v2 (v16: enums/blob/script + Vpc region ref + v15 — see `apis/vultr/README.md`)                               |
| [xkcd](xkcd/)                       | xkcd JSON API                                                                                                                   |


---

## How to run

Use a given API’s README for env vars and backend URL. Typical pattern:

```bash
cargo run --bin plasm-repl -- --schema apis/<name> --backend <origin>
```

Each API’s `domain.yaml` sets `**http_backend**` (default origin for execution); override with `**--backend**` when using the REPL if needed.

Eval harnesses live beside each schema, e.g. `plasm-eval --schema apis/clickup --cases apis/clickup/eval/cases.yaml`.

**Eval coverage (no LLM):** `plasm-eval coverage --schema apis/<name> --cases apis/<name>/eval/cases.yaml` compares CGS-derived required expression-form buckets to the union of per-case `covers` (see the plasm-authoring skill under `.cursor/skills/plasm-authoring/`). Optional `apis/<name>/eval/coverage.yaml` can exclude buckets. See `[eval/README.md](../eval/README.md)`.