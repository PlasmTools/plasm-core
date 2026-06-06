# Changelog

All notable changes to this OSS workspace are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.86] - 2026-06-06

### Added

- **Row contract:** search/query teaching table separates capability filter `inputs` from `provides` row fields; projection, `group_by`, sort, dedupe, and row filters reject filter-only params with capability-scoped errors.
- **Fibery hosted MCP:** resolve `YOUR_ACCOUNT.fibery.io` via `FIBERY_HTTP_BACKEND` env or `http_backend` in outbound KV JSON; catalog `hosted_kv` for tenant connect.
- **Release packaging:** `pokeapi` added to OSS appliance plugin pack list (`scripts/oss-packaged-apis.txt`); SaaS list updated in monorepo `deploy/saas-packaged-apis.txt`.

## [0.1.85] - 2026-06-06

### Added

- **Row transforms:** `.dedupe(keys)`, `.distinct(keys)`, and `.distinct()` postfixes → `ComputeOp::DedupeBy` (in-memory, first-seen per key; full-row hash when no keys).
- **Aggregates:** `first(field)` and `last(field)` on `.group_by(...)` for representative-row analytics.
- **Search teaching:** filter exemplars `e#~"text"{p#=…}` in teaching TSV synthesis.
- **Pagination cap:** optional `max` on CML `PaginationParam::Counter`; GitHub `*_search` mappings cap at page 33 (`per_page=30`, 1000-result API limit).

### Changed

- **Search homographs:** `p#` keys inside `~"{…}"` filter braces expand via Search-cap param map before global wire expansion; Create-only homographs get actionable parse errors.
- **Binding continuations:** known postfix tails (`dedupe`, `distinct`, …) peel before relation nav; unknown transforms list allowed postfix ops.

### Fixed

- **`repos.dedupe(p#)` misroute:** no longer treated as a bogus relation hop (G2).

## [0.1.84] - 2026-05-30

### Added

- **Language matrix:** `lang_relation_opaque_r_symbol` (`items.r#` nav) and `lang_flattened_single_liner_coercion` (space-separated bindings + `coerced_default_return` metadata).

### Changed

- **teaching table docs / prompt_render:** clarify `r#` vs `p#` pools — relation nav exemplars use `.r#` without duplicate standalone gloss rows; homograph `p#` vs expanded parse documented.

## [0.1.83] - 2026-05-30

### Added

- **Relation `r#` namespace:** separate opaque relation symbols in [`SymbolMap`](crates/plasm-core/src/symbol_tuning.rs); teaching TSV / teaching gloss rows; MCP `_meta.plasm.relations` with `target_entity`.
- **Unified relation-segment resolver:** [`resolve_relation_segment`](crates/plasm-core/src/relation_segment.rs) shared by parser and DAG; homograph `p#` in relation nav rejected with `RelationSegmentWrongRole`.
- **Forgiving program coercion:** space-separated single-liner programs split into bindings; default return is the first binding (`FlattenedProgram` + plan `metadata.coerced_default_return`); LHS-gated relation continuation forgives wrong opaque tokens when the binding label matches a relation wire.

### Changed

- MCP initialize / `plasm_context` prompts document `r#`, coerced single-liners, and `_meta.plasm.relations`.
- `exposed_relation_symbol_rows_with_catalogs` fills `target_entity` at symbol-map build time.

## [0.1.82] - 2026-05-30

### Fixed

- **HTTP 403 rate limits:** GitHub-style quota responses (`API rate limit exceeded`, `X-RateLimit-Remaining: 0`, `X-RateLimit-Reset`) are now retryable on safe methods with existing backoff; exhausted retries surface `RuntimeError::RateLimited` instead of a generic request error.
- **`X-RateLimit-Reset` parsing:** sleep hint now uses seconds-until-reset (was incorrectly multiplied by 1000).

## [0.1.81] - 2026-05-30

### Fixed

- **Plural relation row-hole:** resolve opaque `p#` to wire names before row-hole IR in `parse_relation_continuation_expr` (`issues.p2` ≡ `issues.labels` on plural bindings).
- **`expr_parser`:** resolve relation pipeline segments through `sym_map` for `p#` parity with DAG lowering.
- **Plan `prefer_from_parent_get` live:** wire-embed fast path from parent JSON; per-row embed before scoped fallback; avoid holding session graph lock across scoped fanout (fixes Hermit matrix hang on `LangItem(…).tags`).

### Added

- **Language matrix:** `LangItem.tags` `prefer_from_parent_get` + `langtag_get`; Hermit embed hit/miss rows; opaque plural relation unit coverage in `plasm-agent-core`.

### Changed

- **Language spec:** plural binding continuations accept wire names and opaque `p#`; row-hole continuation subsection.

## [0.1.80] - 2026-05-30

### Fixed

- Drop unused `binding_proofs` from `ValidatedPlanRelationTraversal` (plan JSON still carries proofs; fixes `-D warnings` dead_code in CI).
- `plasm-core` / `plasm-runtime` clippy clean under `-D warnings` (Rust 1.93).

## [0.1.79] - 2026-06-05

### Added

- **`prefer_from_parent_get` relation materialization:** CGS composite (wire path + declared scoped fallback); shared `resolve_relation_row_resolution` in `plasm-core` for plan and runtime parity.
- **Frozen plan materialize:** `ValidatedPlanRelationTraversal.materialize` copied from CGS at lower time; plan executor matches on enum (no cache-shape heuristics on pure scoped fanout).

### Changed

- **Pure `query_scoped_bindings`:** always one scoped HTTP query per parent row; ignores decoded `relations[rel]` on the parent.
- **GitHub `Issue.labels`:** `prefer_from_parent_get` with `issue_label_query` scoped fallback (`on_embed_miss: fallback_scoped`).
- **Parallel scoped fanout:** `fanout_scoped_query_parallel` reuses projection `branch_seed` + `hydrate_concurrency` for fallback jobs.
- Removed runtime `chain_relation_refs_present` / hybrid `relations[key]` shortcuts on `Unavailable` and pure scoped paths.

### Fixed

- Relation decoders emit embed path for `prefer_from_parent_get` (same as `from_parent_get`).

## [0.1.78] - 2026-06-04

### Added

- **Runtime session cache cutover:** `SessionMaterialization` facade (entity graph + live `ResponseStore` by `RequestFingerprint` + `QueryIndex` for exact scoped queries); consult-before-HTTP in query/GET/replay paths; fanout branches fork session snapshots and merge via `absorb_branch()` instead of empty local caches.
- **`CacheTelemetry`:** honest consult counters (`entity_graph_*`, `response_store_*`, `query_satisfied_from_graph`, `rows_materialized`) alongside legacy `cache_hits` / `cache_misses` on traces.

### Changed

- **Plan relation fanout:** `materialize_relation_scoped_fanout` holds one session graph lock and reuses materialization across rows (fewer redundant pipeline re-entries).

### Fixed

- **Trace cache KPIs:** stop treating output row counts as cache misses on query/chain merge paths.

## [0.1.77] - 2026-05-30

### Changed

- **Paginated program reads:** plan and HTTP execute default to consuming **all API pages** for paginated capabilities unless the surface node sets `.page_size(n)` (first-page cap). Row `.limit(n)` still truncates after materialization. MCP `page(pgN)` continuations unchanged.

### Fixed

- **Relation `uses_result`:** merge scope `node_input` aliases (e.g. `repo`) into relation plan nodes; rewrite IR holes that name the row input `source` to the upstream binding id (fixes live `InputAlias("repo")` on scoped fanout).

## [0.1.76] - 2026-05-30

### Fixed

- **Relation binding assignability:** parent `id` / `key_vars` scalars may supply `entity_ref` scope params on the same entity (e.g. Cloudflare `zone_id: id`); denormalized compound slugs such as GitHub `repository: full_name` pass when `normalize_entity_ref_value_for_target` accepts the parent field row.

## [0.1.75] - 2026-05-30

### Fixed

- **Relation scoped bindings (proof-carrying):** catalog validates parent field ↔ capability param type assignability; plan stores `RelationBindingProof`; shared `wire_coercion` types row JSON, IR holes, and runtime `execute_chain_via_bindings` predicates; post-instantiate preflight on plan fanout (`execute_plasm_parsed_expr`) so dry plan approval matches live compile (fixes `issue_number: Integer` vs string on GitHub-style `query_scoped_bindings`).
- **Preflight `$` gate:** reject teaching placeholder via IR (`reject_domain_placeholder_in_executable`) only — not raw `source.contains('$')` — so search surface `e#~$` and plan display labels still execute.

### Added

- **`plasm_language_matrix`:** `lang_relation_integer_scoped_bindings` (Integer param through relation fanout).
- **`plasm-core`:** `wire_coercion` module (`RelationBindingProof`, `field_type_assignable_for_relation_binding`, `coerce_value_for_field_type`).

## [0.1.74] - 2026-05-30

### Fixed

- **Plural relation fanout:** `RelationTraversal` with `source_cardinality: many` executes scoped child queries per upstream row (fixes live “expected exactly one row” on `issues.labels`-style programs).
- **Postfix after `group_by`:** `.sort` / `.filter` on compute chains validate against immediate `PlanGroup` schema (e.g. `.sort(n, desc)` after `group_by(..., n=count)`).
- **Scoped relation holes:** coerce numeric strings for `number` / `issue_number` (and `*_number`) when filling `node_input` IR holes.

### Changed

- **`=>` clarity:** MCP frontmatter pitfalls — binding `=>` only for derive maps and `for_each`; child reads use `binding.p#`. Teaching TSV relation Meaning uses `→` (not executable `=>`). Dry-run derive lines use `derive map … →` (not `map … =>`).
- **Derive compile guard:** reject `source => Entity(…)` / `eN(…)` surface literals on derive RHS (use `binding.p#` for relation hops).
- **Dry-run review:** warn when plan includes `source_cardinality: many` relation fanout (`has_relation_many_source_fanout`).
- **teaching contract:** federation pitfall only when `distinct_catalog_count > 1`; dedupe `$` / `.content` guidance (fill-in + string slots in Common pitfalls only).
- **teaching contract (slice-conditioned):** gate search, search-only-entity, and federation Common pitfalls by exposure slice (`ContractSliceHints`).
- **Teaching TSV:** relation nav cap 16→4; skip redundant `.p#` nav when a teaching row already shows relation navigation; multi-arity method exemplars capped at 16 (was 48); Meaning uses `→` / `opt:`; capability gloss `MAX_DESC` 80.
- **MCP initialize:** trim `workflow_head`; tighten head budget cap to 950; remove unused `plasm_tool_tail.txt`.

### Added

- **`plasm_language_matrix`:** `lang_relation_many_from_plural_query`, `lang_group_by_then_sort_agg_column`; derive-map compile reject tests.
- **`pokeapi_type_only_slice_prompt_snapshot`** search-pitfall omissions; **`federated_slice_contract_includes_federation_pitfall`**; github full-prompt **5%** byte regression baseline (25_850).

## [0.1.73] - 2026-05-30

### Changed

- **teaching:** search exemplars use `e#~"text"` (not `e#~$`); Common pitfalls call out search-only entities and fill-in `$`.
- **MCP initialize:** compressed `workflow_head`; fill-in rule folded into `program_contract` grammar line; tool heads head-only again; per-segment char budgets in `mcp_prompt_char_budget`.
- **teaching contract:** scoped GitHub worked example only when slice includes `Repository` + `Issue`; trimmed Common pitfalls; generic MCP frontmatter omits worked example.
- **Teaching TSV:** lazy field gloss (retain `p#`/`v#` referenced by exemplars); cap query exemplars to 2 per entity (primary query first).

### Added

- **`domain_search_teaching_rows_use_quoted_text_not_dollar`** regression test.
- **Docs:** teaching table `$` / `~"text"` placeholder subsection in incremental-teaching-prompts.

## [0.1.72] - 2026-05-30

### Fixed

- **Federated teaching table symbols:** teaching TSV and gloss emission use qualified `(entry_id, entity)` lookups (`entity_sym_for`, `ident_sym_*_for`) so colliding wire names (e.g. `github/Issue` + `linear/Issue`) render distinct `e1` / `e2` instead of both `e1`.
- **Unqualified symbol lookup:** `SymbolMap::entity_sym` / `try_entity_domain_term` return wire names when the same entity label is exposed from multiple catalogs (fail closed for agents inferring from bare `Issue`).

### Added

- **`plasm_language_matrix`:** `lang_federated_duplicate_entity_e1_query` and `lang_federated_duplicate_entity_e2_search` Hermit rows assert `catalog_entry_id` stamps `github` vs `linear` for session `e1` / `e2`.

## [0.1.71] - 2026-05-30

### Performance

- **teaching synthesis (full-catalog / `include_domain_execution_model`):** precomputed `SymbolMap` expand tables (no per-call `HashMap` on symbol replace); lazily cached per-entity `CachedManifestNames`; inverted `CgsIncomingNavIndex` for incoming relation nav; `creates_by_anchor` on `CgsCapabilityIndex` (no full-capability scan per entity).
- **Render path:** `TeachingSynthesisSession` shares line-valid cache and gloss state; federated exposure uses `render_teaching_table_resolved` (no duplicate entity loop); `ident_meta_by_entity` index for exposure ident metadata lookup.
- **Regression:** Criterion benches for validation TSV on `plasm_prompt_matrix` and `apis/github` (`domain_prompt_render`).

## [0.1.70] - 2026-05-30

### Performance

- **teaching synthesis:** unified line-valid cache stores `ParsedExpr` for both TSV-only and `include_domain_execution_model` paths (no `collect_meta` bypass); nav/projection probes share the same cache.
- **CGS indexing:** `named_query_capabilities` uses `CgsCapabilityIndex` instead of scanning all capabilities.

## [0.1.69] - 2026-05-30

### Changed

- **MCP prompts:** `plasm` / `plasm_run` tool descriptions are head-only; v0.1.68 grammar pitfalls live in the first-wave teaching TSV contract preamble (passes `mcp_prompt_char_budget` again).
- **Discovery:** MCP `discover_capabilities` defaults to global score-ranked top-N rows (round-robin fair-share opt-in via `DiscoveryTableMode::PerEntryFairShare`).
- **teaching exposure:** MCP `plasm_context` read-first seeded waves defer weak-scored mutators unless `ranked_capabilities` lists the wire name.

### Added

- **Plan dry-run:** hint when the graph is `query` → `.limit` → row `.filter` (fetch vs row filter).

### Performance

- **teaching synthesis:** `Arc<SymbolMap>` on line validation (no per-row map clone); `u64` line-valid cache keys; exposure `entity_catalog_ids` map; `capability_manifest` reuse per entity.
- **Regression:** Criterion bench `domain_prompt_render`; CI wall-time guard `prompt_matrix_full_tsv_synthesis_benchmark` (`PLASM_PROMPT_MATRIX_SYNTH_MAX_MS`).

## [0.1.68] - 2026-05-30

### Added

- **Row compute:** `.filter{…}` / `.filter(…)` on materialized bindings; `group_by(key)` sugar (`count=count`) and multi-key `group_by`; plan lowering to `ComputeOp::Filter` / expanded `GroupBy` keys; matrix + MCP prompt coverage.
- **Federation P0:** `catalog_entry_id` on surface IR through typecheck/plan; ambiguous bare entity names fail closed across federated catalogs.

### Fixed

- **Plan relations:** chained `from_parent_get` hops (e.g. `summary.detail`) materialize nested JSON from parent rows instead of HTTP GET on non-existent summary/detail routes.
- **Language matrix (live):** `run_markdown` expectations match MCP fenced TSV output; row-filter fixtures use stable Hermit `owner` values.

## [0.1.67] - 2026-06-02

### Fixed

- **OSS release (macOS):** Circle `oss_binaries_macos` no longer deletes downloaded release tarballs before computing `SHA256SUMS` (fixes post-upload checksum failure on retagged releases).

## [0.1.66] - 2026-06-01

### Fixed

- **Referential transparency:** unified session `e#` / wire entity constructor parsing across brace predicates, method args, and nested compound slots (`entity_ref_parse`); symbolic `p#` compound keys normalize without wire pre-expand.
- **Program binding refs:** `issue.p27` / `body.content` lower to `PlasmInputRef` with opaque `p#` path segments resolved to wire field names in program context.
- **Teaching QA:** teaching rows validate on the opaque agent parse path (`domain_line_validate_symbolic`) in addition to wire-expanded validation.

### Changed

- **MCP prompts:** entity-ref RT guidance in tool tail, scoped-search contract examples, and github symbolic teaching table snapshot.

## [0.1.65] - 2026-06-01

### Fixed

- **Templates:** `${}` row-cursor roots skipped in template dependency collection; for_each materialization wires upstream singleton inputs; effect-template interpolation aliases validated; `${}` scanning consolidated in `template_ref`.
- **Symbol expansion:** method tokens (`.m#`) resolve when entity symbols stay opaque in compact teaching table — wire entity name used for anchor-scoped method lookup.

## [0.1.64] - 2026-06-01

### Changed

- **Release packaging:** split hosted SaaS (`deploy/saas-packaged-apis.txt`, 17 catalogs — no Google Workspace or zero-auth demos; adds `grafana`) from OSS appliance tarballs (`scripts/oss-packaged-apis.txt`, 22 catalogs — SaaS set plus Google five). `oss-release-pack-native.sh` no longer reads deprecated `deploy/packaged-apis.txt`.

## [0.1.63] - 2026-05-31

### Fixed

- **OSS install checksums (Linux):** Circle `oss_binaries_linux` no longer skips `SHA256SUMS`; `oss_binaries_macos` rebuilds checksums from all release tarballs so `oss-release.json` includes Linux `sha256` and `install.sh` can verify downloads.

## [0.1.62] - 2026-05-31

### Changed

- **Listen binding:** `--listen-host` and `PLASM_LISTEN_HOST` control the TCP bind address on `plasm-server`, OSS `plasm-mcp`, and hosted `plasm-mcp-app` (default `127.0.0.1`; `0.0.0.0` when `KUBERNETES_SERVICE_HOST` is set). HTTP and Streamable MCP share one port on `/mcp`.
- **Appliance TUI / boot:** Status and client copy show `host:port`; wildcard binds add a loopback hint for `plasm` / MCP JSON snippets.

### Fixed

- **Docs:** removed stale HTTP port N / MCP port N+1 guidance; document unified listener and `--listen-host`.

## [0.1.61] - 2026-05-31

### Changed

- **Auth-framework bootstrap:** OSS appliance, `plasm-mcp` with `project_mcp_*`, and hosted stacks share `ensure_auth_framework_on_host()` — encrypted auth KV, JWT signing, and MCP API keys on one `AuthStorage` `Arc`.
- **`GET /v1/auth/status`:** single probe — `200` when `AuthFramework` is initialized (`storage`: `postgres` or `memory`), `503` only when bootstrap failed.

### Fixed

- **Appliance TUI:** `plasm-server` persists local `PLASM_AUTH_JWT_SECRET` and initializes auth-framework in boot phase 4 so `/v1/auth/status` no longer returns `auth_framework_disabled` while execute and secret encryption work.

## [0.1.60] - 2026-05-30

### Changed

- **Federated teaching table:** `(entry_id, entity)` qualified keys through exposure, symbol assignment, intent-surface filters, federated render, and parse (`e#` keeps catalog ownership via opaque symbols).
- **MCP steady state:** identical-seed `plasm_context` returns one-line noop; syntax hints removed from reuse paths.

### Fixed

- **Teaching TSV bleed:** github vs linear `Issue` (and other name collisions) render from the owning catalog, not first-match entity names.
- **Linear `team_key` search:** federated parse resolves CGS from opaque `e#` / `active_entity_entry_id`.
- **PokéAPI evolution chain:** card-one URL `id_field` relations decode via nested `RelationDecoder`.
- **Legacy exposure surface:** session registry `entry_id` aligns with intent-filtered capabilities when YAML omits `entry_id:`.

## [0.1.59] - 2026-05-30

### Changed

- **Registry aliases:** optional `registry_aliases:` in `domain.yaml` (e.g. `pokemon` → `pokeapi`); `plasm_context` resolves seeds before tenant ACL.
- **Discover:** entity summaries keyed by `(entry_id, entity)`; MCP table uses round-robin fair-share across catalogs; federated truncation hint improved.
- **teaching waves:** omit full TSV on reused execute open; intent-gated mutations on seeded entities (reads still always exposed).
- **Dry-run:** projection highlight line; `_meta.plasm.unused_seeds` / `projection_warning`; resolved `cap=` in DAG for federated queries; GitHub auth-scoped repo list boundedness note.

### Fixed

- **Discover descriptions:** cross-catalog entity name collisions (e.g. `linear/Issue` no longer inherits GitHub Issue text).

## [0.1.58] - 2026-05-29

### Changed

- **Single DAG execute pipeline:** MCP `plasm` / `plasm_run`, HTTP `POST /execute/:prompt_hash/:session`, and resolved-plan CLI runs all go through `compile_plasm_expression_to_plan` → `ExecutePipeline::run_program` (no comma-root split, staged lines, or `execute_session_run_markdown`).
- **HTTP execute body:** one program only — `text/plain`, JSON string, or `{"program":"..."}`; JSON `lines` arrays and top-level string arrays return **400**.
- **Slim run Markdown:** return-label headers (`## sorted (12 rows)` / `# Results` + `### …`) plus table/TSV only — no REPL `output:`, `owner:`, `→`, projection, or raw source in MCP/CLI `run_markdown`.
- **Parallel return labels:** plan publication uses binding node ids (not `parallel[i]`).
- **`_meta.plasm.steps`:** truncated snapshot steps include `return_label`, `display`, and `row_count`.

## [0.1.57] - 2026-05-29

### Changed

- **MCP prompt deduplication:** initialize owns orchestration + program contract (newlines/heredoc); tool descriptions and `program` JSON-schema param are slim pointers (~40% fewer static prompt bytes). Grammar source remains dynamic **`plasm_context`** teaching TSV.
- **`discover_capabilities` guidance:** one **`intent`** string per user goal — no per-integration discover splits or keyword arrays; retry only when the first table is incomplete.

## [0.1.56] - 2026-05-29

### Added

- **GET shadow sugar:** `Entity(id_field=value)` on simple-id entities (e.g. `Pokemon(name="pikachu")`, `EvolutionChain(url="https://…")`) parses as unary GET — not not taught in the teaching table; canonical surface remains `Entity(value)`.

### Fixed

- **`p#` projection:** teaching-table field symbols resolve to wire names in DAG compile, preflight, and live projection hydration (`pikachu[p35,…]`).
- **Relation chains:** relation-sourced bindings continue via row-hole IR instead of anchor re-parse (fixes federated multi-hop type-check and evolution-chain URL GET).
- **Relation target IDs:** `resolve_relation_target_id` no longer falls back to the source row’s primary ref; decoded relation URL fields populate row-identity ambient for plan holes and live `extract_ref_id`.

## [0.1.55] - 2026-05-29

### Added

- **Row composition redesign (Waves A–E):** `RowIdentity`, unified `SuffixPipeline` lowering, `CatalogResolver` / `FederationDispatch` hint-aware resolution, and `ExecutePipeline` as the single HTTP/MCP execute ingress.
- **`PreflightToken`:** agent-core preflight gates (type-check, `$` rejection, projection field validation) run once; live runtime skips duplicate TC when the token is present.
- **`PlasmPreflight` / `ExecutePipeline::run_expression`:** dry `plan_only` and live lines share the same preflight chain (dry ≡ live).

### Changed

- **Federation:** `catalog_ownership::resolve_cgs_for_entity` and `plan_http_origin` unify type-check, plan, and live HTTP dispatch (engine harness wins over schema placeholder backends).
- **Relation chains:** nested `.relation` navigation resolves tip entity for type-check and materialization (`relation_navigation_entity`).
- **Runtime:** `reject_domain_placeholder_in_executable` lives in `plasm-core`; embedded entity cache + row-hole IR for URL evolution-chain GET.

### Fixed

- **Language matrix:** federated relation targets, one-cardinality relation hops, and `limit(1)` continuations run live without skip list; Hermit harness URL overrides `127.0.0.1:9` catalog placeholders.


### Fixed

- **Composition identity:** align federated type-check, binding continuation, and runtime decode so entity + row identity resolve the same way across compile, dry-run, and live execution.
- **Federation:** `resolve_cgs_with_hint` + source-catalog-first chain targets in `type_check_chain_federated`; `catalog_ownership` / `typecheck_parsed_for_session` use shared federation doctrine.
- **Plasm programs:** `ContinuationAnchor` (`RootSurface`, `RelationExpand`, `BindingLabel`) with unified `lower_relation_continuation`; bare surface relation chains lower to `RelationTraversal` (not `Query`); `limit`/`project` bindings continue via label or surface anchors.
- **Runtime:** GET/invoke decode merges CML env into identity ambient; `extract_ref_id` prefers embedded relation refs (no parent-id fallback); URL `id_field` GET decode uses request identity override.

### Changed

- **Language matrix:** `lang_query_all` expects explicit `langitem_query` capability; relation-chain IR assertions updated for lowered chains.

## [0.1.53] - 2026-05-29

### Changed

- Workspace version sync for monorepo release **v0.1.53** (SaaS MCP config UI in parent `web/` + `elixir/plasm_ui_core`).

## [0.1.52] - 2026-05-29

### Fixed

- **Federation:** `resolve_qualified_entity_key` replaces blind `session.entry_id` fallbacks when labeling plan `qualified_entity` rows — relation targets (e.g. `EvolutionChain` in pokeapi+linear sessions) resolve to the owning catalog, not the lexicographic primary.
- **Plasm programs:** typed relation continuations emit executable IR (`anchor` re-parse or `node_input` holes) instead of `Get($)` placeholders that dry-run accepted but live execution rejected.

### Added

- **`catalog_ownership`:** shared QE resolution for `plasm_dag`, HTTP execute traces, and plan materialization.
- **Language matrix:** `lang_federated_relation_target_entry` row (federated primary `linear` + secondary `pokeapi`).

## [0.1.51] - 2026-05-29

### Fixed

- **Plasm programs:** unified `ProgramBindingContract` for binding continuation — one-cardinality relation hops from singleton parents (e.g. `species = item.<one_rel>; next = species.<one_rel>`) now emit `source_cardinality: single` instead of incorrectly treating all relation bindings as plural. Completes the partial relation-binding work shipped in 0.1.50.

### Added

- **Language matrix:** binding continuation rows for `limit(1)` chains, projection→relation, and two-hop one-cardinality `summary.detail` hops.

## [0.1.50] - 2026-05-29

### Added

- **MCP discovery:** score-ranked row caps (default 12 rows, 8 per API) with `_meta.plasm.discovery` omission hints when truncated.
- **MCP dry-run:** `DryPlanGuidanceMode` — boilerplate `next:` guidance shown once per logical session.

### Fixed

- **MCP prompts:** federation invariant (one goal → one `intent` → one `plasm_context`); `discover_capabilities` documents single `intent` (not `query`); initialize instructions no longer duplicate the full syntax guide.
- **MCP execution:** federated CGS resolution for in-band summaries; strings >256 chars and markdown fields emit `(in artifact)` instead of inline blobs; preview threshold lowered to 4k chars.
- **Plasm programs:** relation continuation through bindings — partial (`RelationTraversal` entity routing only); one-cardinality source proofs completed in 0.1.51.


### Fixed

- **Tavily catalog:** `research_create` uses an explicit JSON object body so POST `/research` sends `{"input":"…","model":"…"}` instead of a bare string (execute_create env splat overwrote `env["input"]` when mapping used `body: { type: var, name: input }` alongside a scalar `input` parameter).

### Added

- **Schema validate:** reject `BodyVarInputParamCollision` when a capability maps `body: { type: var, name: input }` but parameter `input` is scalar (not `type: json` / inline object).
- **E2E:** `tavily_smoke` asserts compiled `research_create` body is a JSON object with an `input` key.
- **Authoring:** reference.md documents the env-splat pitfall and the explicit-object fix.

## [0.1.48] - 2026-05-28

### Fixed

- **teaching exposure:** Explicit `plasm_context` seeds always teach that entity’s `query` / `search` / `get` / `create` (and `primary_read`) even when the stable `intent` lexicon does not score them; ranked-capability gate cannot drop seeded-entity surface (fixes federated Proof `ShareLink` + `share_link_create` without `ranked_capabilities`).

## [0.1.47] - 2026-05-28

### Added

- **Runtime:** `ResolvedIdentity` binds entity `id_field` names (e.g. `Team.key`) into CML env for GET, preflight, and views.
- **Language:** `${binding.path}` interpolation in program strings at plan instantiate; `Issue{identifier=…}` rewrites to Get on search-only entities; view template `split` / `split_part` filters and `.split()[n]` desugar.
- **Planner:** `=>` bare binding resolves to `NodeSymbol` (`.content` in derive context); Minijinja render column inference from `r.field` refs; relation-target mutation closure in intent teaching table exposure.

### Fixed

- **Linear catalog:** `issue_context` comments bind human `issue_identifier` for GraphQL filter.

## [0.1.46] - 2026-05-28

### Added

- **`plasm-server`:** auto headless mode when stdout or stdin is not a TTY; `--tui` forces the Ratatui control station; `--no-tui` unchanged.

## [0.1.45] - 2026-05-28

### Added

- **Remote `plasm` CLI:** compressed workspace layout (`.plasm/hosts/<8hex>/`, `.plasm/s/<8hex>/`) and append-only `out/NNNN-{search,context,plan,run}/` mirror archive with dual `body.json` + `body.txt` (and `artifact.*` on live runs).
- **Remote `plasm` CLI:** device OAuth login (`plasm login`, platform `plasm init`), pwd-local profiles, typed `context`/`run` flags, `incoming_auth_device` HTTP helpers.

### Changed

- **Breaking (local state):** drop `.plasm/cgs/` tree — remove `.plasm` once after upgrade; no migration.

## [0.1.44] - 2026-05-28

### Fixed

- **Release CI (monorepo):** `verify-vultr-release-images` accepts manifest-list/OCI index media types (buildx pushes) and polls until tags appear in Vultr CR.

## [0.1.43] - 2026-05-28

### Fixed

- **Release packaging:** monorepo `deploy/packaged-apis.txt` drops nonexistent `teams` entry and duplicate `outlook` so `plasm-pack-plugins` completes in Docker release builds.

## [0.1.42] - 2026-05-27

### Fixed

- **apis/fibery:** restore complete `domain.yaml` (entities, capabilities, views, `schema_overlay`) so `plasm-pack-plugins` can load the catalog in release Docker builds.

### Added

- **Runtime schema overlay:** unified `schema_overlay:` spec in `domain.yaml` — host fetches workspace schema at execute session open and merges typed entities/columns into the session CGS (`effective_catalog_cgs_hash_hex`).
- **API-driven multi-fetch pipeline:** `source.steps` with `collect` → `for_each` (row-driven `bind`) → `merge` for scoped schema endpoints (ClickUp `team_query` → `custom_field_query`; Jira `project_query` → `issue_createmeta_get`).
- **Projection modes:** `per_scope_entity` (Fibery, Notion, Jira) and `augment_base` (ClickUp custom fields on `Task`); Minijinja filters `join_sanitize`, `sanitize_identifier`.
- **Catalog overlays:** Fibery, Notion, ClickUp, Jira `schema_overlay` blocks; Linear overlay deferred (no public custom-field definition query).
- **Session resolver:** `schema_overlay_session` wired at HTTP execute, MCP `plasm_context`, federated attach, and local `plasm-repl`.

### Changed

- **Overlay configuration is API-only:** removed HTTP `overlay_scope`, MCP seed `scope`, and client/env `source.bind` for overlay — session auth + catalog-declared pipeline only.
- **Authoring skills / catalog READMEs:** document API-driven overlay pattern and multi-fetch for scoped schema APIs.

## [0.1.41] - 2026-05-27

### Fixed

- **Release CI (monorepo):** semver `publish-release vX.Y.Z` is tag-only; `release_ship` verifies Vultr images exist before bumping `deploy/values/dev/images.yaml`, avoiding `ImagePullBackOff` when deploy refs lead the registry.

## [0.1.40] - 2026-05-27

### Fixed

- **apis/linear (v10):** `IssueContext` / `issue_navigation_link` view Gets; `comment_by_issue_query` filters by issue UUID or identifier; `Issue` / `IssueContext.comments` relation materialize; `team_get` GraphQL `key` variable.
- **Runtime:** parameterless view Gets (`user_viewer` / `MyWorkSnapshot`); Get bind `id` aliases entity `id_field`.
- **Parser / planner:** `Issue.search(…)` sugar; brace filters on Search-only entities resolve to `issue_search`; surface parse normalizes `capability_name` so dry-run plans match live execution.
- **MCP:** no-op `plasm_context` expand includes compact expression-syntax hints.

## [0.1.39] - 2026-05-27

### Added

- **preflight:** typed capability `preflight` steps (full cutover from `invoke_preflight`) — `hydrate_invoke_target`, `hydrate_entity_ref_param`, `query_pick`, `label_ids_delta`; runtime press on create/invoke before CML merge.
- **apis/linear:** task-oriented catalog (v9) aligned with [linear/linear#1035](https://github.com/linear/linear/issues/1035) — `issue_search` with team/state/assignee **names**, `IssueContext` / `MyWorkSnapshot` views, consolidated `issue_create` / `issue_update` with name→ID preflight, `user_search`, unified comment `issue` entity_ref.
- **Catalog:** Gmail, Google Drive, Discord, and Grafana capabilities migrated to `preflight` hydrate steps.

### Changed

- **plasm-server:** typed Logs tab (level colors, compact timestamps); Clients tab MCP JSON display and copy.

## [0.1.38] - 2026-05-27

### Fixed

- **MCP sqlx migrate:** prune squashed `_sqlx_migrations` ledger rows (e.g. `20260216120000`) before embedded migrate so init containers succeed on upgraded clusters.

## [0.1.37] - 2026-05-27

### Fixed

- **Docker bake:** post-push ELF verify works in `debian:*-slim` images (no `file(1)` dependency).

## [0.1.36] - 2026-05-27

### Fixed

- **Docker bake:** export `PLASM_HOST_TARGET_TRIPLE` for `plasm-agent-core` when `cargo chef` skips `build.rs` output.

## [0.1.35] - 2026-05-27

### Fixed

- **Docker cross bake:** restore multiarch OpenSSL sysroot for `auth-framework` reqwest native-tls; `oauth2` / `opentelemetry-otlp` use rustls.

## [0.1.34] - 2026-05-26

### Fixed

- **Docker bake:** canonical cargo-chef order (cook deps before app source); rustls-only `reqwest`; cross arm64→amd64 on M-series Mac CI without OpenSSL cross deps.

## [0.1.33] - 2026-05-26

### Fixed

- **Docker bake:** reject stub `plasm-mcp` / `plasm-trace-sink` ELFs (size + `file(1)` arch) in `rust-builder` and post-bake verify; harden cross-compile artifact paths after `cargo chef cook`.

## [0.1.32] - 2026-05-26

### Fixed

- **Release CI:** forbid k3d `plasm-argocd-sync` job push to `localhost:5000` on CircleCI (force Argo Git sync / kubectl mode).

## [0.1.31] - 2026-05-26

### Fixed

- **Release CI:** `portal-release-finalize.sh` always bakes/pushes/rollouts portal after Argo sync; tag guard rejects broken `v0.1.30` release_ship checkouts.

## [0.1.30] - 2026-05-25

### Fixed

- **plasm-server TUI:** Clear each frame, handle terminal resize, taller tab rail, display-width clipping for catalogue/API rows; bootstrap supervisor messages go to Logs tab (tracing) instead of painting over the footer.

## [0.1.29] - 2026-05-25

### Fixed

- **plasm.tools/get:** release pill reads GitHub `oss-release.json`; portal image cache bust on manifest version.

## [0.1.28] - 2026-05-25

### Fixed

- **CI:** `git-checkout-main.sh` stashes install manifest before checkout (tag release ship phase 3).

## [0.1.27] - 2026-05-25

### Fixed

- **CI:** Source `ensure-kubeconfig-env` (subshell dropped KUBECONFIG under zsh -il); reject EKS/cicd-cluster.

## [0.1.26] - 2026-05-25

### Fixed

- **CI:** Flat Circle config (no custom commands); use workflow **release** / job **release_ship**, not legacy **oss_publish_install_site**.

## [0.1.25] - 2026-05-25

### Fixed

- **CI:** Circle `zsh_run` command parameters (`step_name` / `run_command`; `name`/`command` are reserved).

## [0.1.24] - 2026-05-25

### Changed

- **CI:** Consolidated Circle workflows (`ci` + `release`) and orchestrator scripts (`circle-test`, `circle-dev-deploy`, `circle-release-ship`).

## [0.1.23] - 2026-05-25

### Fixed

- **CI:** `rollout-plasm-portal` bootstraps Argo `plasm-portal` when Deployment is missing; sanity-check VKE cluster.

## [0.1.22] - 2026-05-25

### Fixed

- **CI:** `ensure-kubeconfig-env` prefers `~/.kube/plasm-vke.yaml` over default `~/.kube/config` for portal rollout.

## [0.1.21] - 2026-05-25

### Changed

- **CI:** SaaS deploy on tag (`saas_publish_deploy_ref`); kubeconfig discovery on self-hosted runner; always run tests on `main`.

## [0.1.20] - 2026-05-25

### Changed

- **CI:** CircleCI project re-linked for **PlasmTools/plasm** (`oss_release` on tag).

## [0.1.19] - 2026-05-25

### Changed

- **Docs:** Canonical release process on **PlasmTools/plasm** [`RELEASING.md`](https://github.com/PlasmTools/plasm/blob/main/RELEASING.md); this repo stubs only.

## [0.1.18] - 2026-05-25

### Changed

- **CI:** `publish_portal_site` in `ci` workflow on `main`; Circle project docs for **PlasmTools/plasm**.

## [0.1.17] - 2026-05-24

### Changed

- **CI / docs:** Monorepo canonical GitHub org is **`PlasmTools/plasm`** (install publish, Argo `track.json`, release secrets).

## [0.1.16] - 2026-05-24

### Fixed

- **CI:** Portal image publish uses `docker build` + Vultr push retries instead of buildx bake (504 blob upload timeouts).

## [0.1.15] - 2026-05-24

### Fixed

- **CI:** Install publish requires `PLASM_MONOREPO_GH_TOKEN` (no optional git-push skip on Circle).

## [0.1.14] - 2026-05-24

### Fixed

- **CI:** Circle install publish uses `PLASM_MONOREPO_GH_TOKEN` for monorepo git push (avoids 403 from plasm-core-only `GH_TOKEN`).

## [0.1.13] - 2026-05-24

### Fixed

- **CI:** `circle-oss-release` no longer `cp` SHA256SUMS onto itself when refreshing an existing release (macOS `cp` exit 1).
- **CI:** Linux OSS release job skips checksum upload; macOS job runs after Linux and merges `SHA256SUMS` once (avoids parallel clobber).

### Changed

- **CI:** Coherent monorepo-tag install pipeline (GitHub manifest default, Circle `oss_publish` gate, GHA install recovery only).

## [0.1.12] - 2026-05-24

### Fixed

- **`plasm-runtime`:** `PaginationConfig` unit tests set `response_next_url_field` (OData `nextLink` field).
- **`plasm-trace-sink`:** `http_iceberg_integration` passes segment-projection TTL args to `PersistedTraceSink::connect`.
- **`apis/cloudflare`**, **`apis/grafana`:** declare scope parameters on `zone_get` / `dashboard_get` used by view node binds.
- **`fixtures/plasm_prompt_matrix`:** mirror `zone_get` `zone_id` for `security_overview` CGS validation.

### Changed

- **`plasm-core`:** refresh Linear full-prompt insta snapshot (cycle board view, issue URL view, teaching-table symbols).

## [0.1.11] - 2026-05-24

### Added

- **`plasm-trace-sink`:** Postgres `trace_segments` projection for hot trace detail reads with configurable TTL/GC.
- **`plasm-trace-sink`:** Head-guided `year_month_bucket` Iceberg pruning on cold detail reads (`event_kind` filter + empty-scan retry).
- **API catalogs:** Microsoft Graph–backed Gmail, Jira, and Linear packages with OData `nextLink` pagination.
- **`plasm-runtime` / `plasm-core`:** View origin injection and inner-node template binds; language-matrix conformance for computed view fields.

### Changed

- **`plasm-agent-core`:** Shared `reqwest::Client` for trace-sink HTTP proxy calls.

## [0.1.10] - 2026-05-23

### Fixed

- **CI / quality:** `cargo clippy --workspace --all-targets -- -D warnings` clean (integration Postgres keep-alive holder, TUI `UiMsg::Admin` boxing, assorted clippy nits).

## [0.1.9] - 2026-05-23

### Fixed

- **Release:** declare `rayon`, `criterion`, and `aho-corasick` in the OSS workspace `Cargo.toml` so standalone `plasm-core` CI builds succeed (v0.1.8 tag missed these entries).

## [0.1.8] - 2026-05-23

### Added

- **Performance:** Criterion benches for CGS load and typed discovery index (`plasm-core/benches/schema_load`, `plasm-discovery/benches/index_build`).
- **Performance:** `CatalogIndexCache` on the agent host; OTEL `plasm.discovery.index_cache_total`.
- **Performance:** `PLASM_CGS_FAST_LOAD=1` skips expression-surface teaching table bundle synthesis at load (structural validate only).
- **Performance:** `PLASM_DISCOVERY_EMBED_CONCURRENCY` env for shared ONNX embedder pool sizing.

### Changed

- **Performance:** Cache `catalog_cgs_hash_hex` on `CGS` via `OnceLock`; store hash in registry metadata at insert.
- **Performance:** Aho-Corasick substring scan for typed discovery; parallel entity index build (`rayon`).
- **Performance:** Incremental Postgres embedding reconcile (missing-line upsert + stale-line delete, no full delete/refill).
- **Performance:** Move capability mappings at assemble time (`swap_remove`); single `finalize_cgs_load` in pack-plugins.
- **Performance:** Parallel legacy capability scoring per catalog entry.

## [0.1.7] - 2026-05-23

### Fixed

- **`plasm-server`:** squash `plasm-agent-core` sqlx to one idempotent migration (`20260601000000_plasm_agent_schema`); drop ledger repair so fresh embedded Postgres boots cleanly.
- **`plasm-server`:** typed MCP policy attach (`McpPolicyAttachOutcome`), appliance bootstrap gate, and scrollable Overview with `config_surface_from_host` at RUN handoff (no garbled trace-hub / `enabledts` overlap).

## [0.1.6] - 2026-05-23

### Added

- **`apis/grafana`:** v5 catalog (core API, RBAC, datasource explorers, Sift/Incident/OnCall plugins, assembled deeplink `url`, panel render/query).
- **`plasm-core` / `plasm-runtime`:** view `output` bindings with `kind: computed` (Minijinja); optional `views.scope` `required:`; `wire_temporal_value` and view-template filters (`wire_time`, `urlencode`, `wire_query_suffix`, …).
- **Conformance:** `plasm_language_matrix_views` computed field `echo_slug`.

### Changed

- **`apis/cloudflare`:** derive `security_surface_status` in `views.security_overview` (domain v13); remove `SecurityOverview` hardcoded derivation from `view_execution`.

## [0.1.5] - 2026-05-23

### Fixed

- **`plasm-server`:** reconcile appliance DB env after `.env` load so embedded PostgreSQL autostarts and `project_mcp_*` sqlx migrations run on first launch (no manual `mcp migrate-db` when a cwd `.env` sets `DATABASE_URL`).
- **`plasm-server`:** fatal bootstrap when embedded PG started but MCP policy store did not attach; Status tab shows concrete errors (ASCII markers, no stderr corruption during alternate-screen TUI).
- **Embedded Postgres:** set `PLASM_MCP_CONFIG_DATABASE_URL` alongside `DATABASE_URL` / `PLASM_AUTH_STORAGE_URL` on autostart.

### Changed

- **`plasm-runtime`:** apply request-identity override for entity decoders when a row id is present (not only `implicit_request_identity` entities).

## [0.1.4] - 2026-05-21

### Changed

- **`plasm-server`:** default appliance root to `~/.plasm/appliance` (or `PLASM_APPLIANCE_DIR`); auto `--plugin-dir` when `{appliance}/plugins` exists so `plasm-server` runs without flags after the OSS installer layout.

## [0.1.3] - 2026-05-21

### Changed

- **OSS release binaries:** typed discovery is **lexical-only** (`fastembed` / ONNX behind Cargo feature `local-embeddings`; not linked in CI release builds). `enable_embeddings` defaults **false**; release MCP schema documents the constraint.
- **Release CI:** remove ONNX Runtime `brew install` from GHA and Circle macOS Intel legs (no longer required for packaging).

## [0.1.2] - 2026-05-21

### Changed

- **OSS release platforms:** `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin` (drop Linux arm64). Intel macOS links ONNX Runtime via Homebrew in CI.

## [0.1.1] - 2026-05-21

### Fixed

- **Release CI:** Docker `BUILDPLATFORM`/`TARGETPLATFORM` for Circle Linux builds; native pack uses monorepo `target/` when built from the private repo.
- **GHA:** drop Intel macOS prebuilds (`ort` has no `x86_64-apple-darwin` ONNX); publish aarch64 Apple Silicon only.

### Changed

- **Release asset names:** drop SemVer from tarball filenames (version is the Git tag only), e.g. `plasm-appliance-x86_64-unknown-linux-gnu.tar.gz`.

### Note

- **v0.1.0** shipped only GitHub source archives (no product binaries); use **v0.1.1** or later for downloads.

## [0.1.0] - 2026-05-20

### Added

- **OSS release train:** three tarballs per platform — `plasm-appliance` (server + API plugins), `plasm` (HTTP client), `plasm-cgs` (dev CLI) — on [GitHub Releases](https://github.com/PlasmTools/plasm-core/releases).
- **Install microsite** sources at `get.plasm.tools` (`install.sh`, `oss-release.json`); generator `scripts/ci/generate-oss-release-json.sh`.
- **CI:** GitHub Actions `release.yml` matrix; CircleCI `oss_release_linux` + `oss_release_macos` (monorepo).

### Changed

- **Binary names:** remote HTTP terminal is now **`plasm`** (`plasm`, `--bin plasm`); the local appliance binary is **`plasm-server`** (Cargo package **`plasm-server`**, directory `crates/plasm-server`; formerly **`plasm-appliance`**); the dev/schema CLI is **`plasm-cgs`** (`plasm-cli`, `--bin plasm-cgs`). Former names: `plasm-cgs` (agent), `plasm-appliance`, `plasm` (cli).
- **Workspace versions:** all `plasm-oss` crates use `version.workspace = true` with a single `[workspace.package] version` in the root `Cargo.toml`.
- **Deprecated:** unified `plasm-oss-*.tar.gz` release archives (replaced by product-specific tarballs above).
