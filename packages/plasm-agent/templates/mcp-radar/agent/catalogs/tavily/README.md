# Tavily REST API — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [Tavily API](https://docs.tavily.com/), covering the full current surface: web search, URL extraction, site crawling, and async deep research.

```bash
export TAVILY_API_TOKEN=tvly-...   # in /Users/ryan/code/plasm/.env
source /Users/ryan/code/plasm/.env

cargo run --bin plasm -- \
  --schema apis/tavily \
  --backend https://api.tavily.com \
  --repl
```

---

## What is implemented

### Entities

| Entity | Key | Fields |
|--------|-----|--------|
| `SearchResult` | `url` | url, title, content, score, raw_content, favicon |
| `ExtractResult` | `url` | url, raw_content, favicon |
| `ResearchTask` | `request_id` | request_id, status, input, model, created_at, content, sources, response_time |

`ExtractResult` is shared between `/extract` and `/crawl` — both endpoints return the same `{url, raw_content, favicon}` shape.

### Capabilities

| Capability | Kind | CLI | Endpoint | Tested |
|------------|------|-----|----------|--------|
| `web_search` | search | `searchresult search --query "..."` | `POST /search` | ✓ Live |
| `url_extract` | query | `extractresult query --urls https://...` | `POST /extract` | ✓ Live |
| `site_crawl` | query | `extractresult site-crawl --url https://...` | `POST /crawl` | ✓ Live |
| `research_create` | create | `researchtask research-create --input "..."` | `POST /research` | ✓ Live |
| `research_get` | get | `researchtask <request_id>` | `GET /research/{id}` | ✓ Live |

`site_map` (`POST /map`) is deferred — see Known Limitations.

---

## CLI examples

```bash
BASE="--schema apis/tavily --backend https://api.tavily.com"

# Web search — basic
plasm $BASE searchresult search \
  --query "Rust async runtime comparison 2025" \
  --max_results 5

# News search with LLM answer
plasm $BASE searchresult search \
  --query "AI agent frameworks latest releases" \
  --topic news \
  --include_answer \
  --max_results 5

# Date-bounded search
plasm $BASE searchresult search \
  --query "Rust 1.94 release notes" \
  --start_date 2026-01-01 \
  --end_date 2026-03-31 \
  --max_results 3

# Domain-filtered search
plasm $BASE searchresult search \
  --query "Rust async tutorial" \
  --include_domains docs.rs \
  --include_domains blog.rust-lang.org \
  --max_results 5

# Fast search with auto parameter selection
plasm $BASE searchresult search \
  --query "latest OpenClaw features" \
  --search_depth fast \
  --auto_parameters \
  --max_results 3

# Extract a single URL
plasm $BASE extractresult query \
  --urls https://docs.tavily.com/documentation/about

# Extract multiple URLs
plasm $BASE extractresult query \
  --urls https://blog.rust-lang.org/ \
  --urls https://doc.rust-lang.org/book/ \
  --extract_depth advanced \
  --format markdown

# Crawl a website
plasm $BASE extractresult site-crawl \
  --url https://docs.tavily.com \
  --max_depth 2 \
  --limit 10

# Crawl with instructions
plasm $BASE extractresult site-crawl \
  --url https://docs.tavily.com \
  --instructions "Find all pages about the Python SDK" \
  --limit 5

# Start async research task
plasm $BASE researchtask research-create \
  --input "What is Plasm, the Rust typed agent CLI for REST APIs?" \
  --model mini \
  --citation_format numbered

# Poll research status (returns content when completed)
plasm $BASE researchtask ec406c91-e2e9-45cd-8f07-0d5725472c1a
```

---

## Authentication

Set `TAVILY_API_TOKEN` to your Tavily API key. The schema reads from this variable:

```bash
export TAVILY_API_TOKEN=tvly-your_key_here
# or source the project .env:
source /Users/ryan/code/plasm/.env
```

---

## search_depth values (updated Dec 2025)

The `search_depth` parameter now has four options:

| Value | Cost | Latency | Content |
|-------|------|---------|---------|
| `basic` | 1 credit | balanced | 1 NLP summary per URL |
| `fast` (BETA) | 1 credit | low | multiple relevant snippets per URL |
| `ultra-fast` (BETA) | 1 credit | lowest | 1 NLP summary per URL |
| `advanced` | 2 credits | high | multiple high-precision snippets |

Earlier snapshots of this schema only exposed `basic` and `advanced` for `search_depth`. The current `apis/tavily` schema includes all four values above.

---

## Integration testing (live, March 2026)

All capabilities tested against the real Tavily API with the dev token.

### web_search

1. **Basic search**: `--query "Plasm Rust REST API typed agent" --max_results 3` → 3 ranked results, correct score and content fields decoded. ✓
2. **News + answer**: `--topic news --include_answer --max_results 3` → news results from past week including OpenClaw surge coverage. ✓
3. **Date range** (`start_date`/`end_date` are new params): `--start_date 2026-01-01 --end_date 2026-03-31` → results limited to Q1 2026, including Rust 1.94.1 release blog. ✓
4. **fast depth + auto_parameters** (both new params): returns results with `auto_parameters` flag set. ✓

### url_extract

- Single URL extraction: `https://docs.tavily.com/documentation/about` → full `raw_content` field decoded (1,900+ char markdown). ✓

### site_crawl

- `--url https://docs.tavily.com --max_depth 1 --limit 3` → 3 crawled pages (`/`, `/changelog`, `/welcome`) decoded as `ExtractResult` entities. ✓

**Note:** A runtime bug was found and fixed during testing. Scope parameters whose names match entity field names (e.g. `url` in `site_crawl` matching `ExtractResult.url`) were incorrectly applied as client-side result filters, returning 0 results. The fix adds scope/search parameter awareness to `entity_field_predicate_scoped` in `execution.rs` — only true field predicates (not scope params) are applied client-side. This fix also benefits GitHub `issue_query` (owner/repo scope params) and Jira `comment_query` (issueIdOrKey scope param).

### research_create + research_get

- `POST /research` with `--input "What is Plasm...?" --model mini` → request_id `ec406c91...`, status `pending`. ✓
- `GET /research/{request_id}` after ~35 seconds → status `completed`, `content` field with 530-char report. ✓

**Note during testing:** The research GET response uses `content` (not `report`) for the generated text. The initial schema used `report` — corrected to `content`.

---

## Design notes

### All capabilities use POST body

Unlike REST APIs that use query params for GET requests, Tavily puts all parameters in the JSON request body (even for non-mutating operations). The CML `body:` block is used for `web_search`, `url_extract`, and `site_crawl`. Only `research_get` uses a GET with no body.

### url_extract and site_crawl as `kind: query`

Both capabilities POST a body and return a list of entities — the same execution path as any paginated query. Using `kind: query` means:
- CLI generates repeatable/typed flags from `parameters:`
- Parameters flow through predicate compilation into the CML env
- Response decoded as a collection via `response: {items: results}`

### research_create as `kind: create`

The async research endpoint creates a task (HTTP 201) and returns `{request_id, status: pending, ...}`. The `kind: create` capability collects flags via `args_to_input` into a Value::Object, then `body: {type: var, name: input}` sends that whole object as the request body. The create response is decoded as a single `ResearchTask` entity.

### Scope params excluded from client-side filtering

Capabilities like `site_crawl` (with `url: role: scope`) and `url_extract` (with `urls: role: scope`) need their scope param predicates excluded from client-side entity filtering. The runtime now calls `capability_non_filter_params()` to get the set of scope/search params and exclude them in `entity_field_predicate_scoped()`.

---

## Known limitations

### site_map deferred

`POST /map` returns `{results: ["url1", "url2", ...]}` — a bare string array. The Plasm decoder expects JSON objects with named fields; it cannot decode bare string items as entities. This is a decoder gap. Once the decoder supports "use the bare string value as the id field", `site_map` can be modelled.

Workaround: use `site_crawl` when you need page content, or call the map endpoint directly with `curl`.

### research polling is manual

The research workflow requires two steps: `research_create` (start task) → wait → `research_get` (poll). There is no automatic polling loop. Agents must poll manually by re-running `researchtask <request_id>`. A future `kind: async_job` capability kind with built-in polling would improve this.

### Large `content` field

Research `content` can be thousands of characters. In table output format, it's truncated. Use `--output json` or `--output compact` to get the full text.

### `include_domains` / `exclude_domains` in body

For web search, `include_domains` and `exclude_domains` are sent as JSON arrays in the body (not repeated query params). Tavily's body parser handles them correctly. They appear in the CLI as repeatable flags: `--include_domains reuters.com --include_domains bbc.com`.

### `answer` field not on SearchResult entity

The search response includes a top-level `answer` string (LLM-generated summary) separate from the individual results. This field is NOT decoded into `SearchResult` entities because it's a response-level field that doesn't belong to any individual result. Agents requesting the answer should use `--include_answer` and read the raw JSON output.
