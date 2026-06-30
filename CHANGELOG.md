# Changelog

All notable changes to this OSS workspace are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.94] - 2026-06-30

### Fixed

- **MCP ranked replay:** exact ranked capability wire names admit seeded mutators at lexicon score zero (`mutating_capability_admitted`); ranked replay diagnostics use qualified `entry:Entity.cap` keys.
- **Capability-only expand waves:** federated sessions render compact mutator/param TSV deltas (not full entity re-render); optional deferred params synthesize `p#` gloss rows from ident metadata when absent from the filtered bundle.
- **Reuse recap:** duplicate `plasm_context` responses include fenced TSV active mutator/param recap; stale binding recovery surfaces `discard_cached_plasm_symbols` in `_meta.plasm.continuity`.

### Changed

- **Exposure wave commit:** `ExposureWaveChanges` + `ExposureWaveSnapshot` replace branching commit tail; `CommittedWaveDelta.surface_unchanged` replaces ambiguous reuse flag.
- **Canonical param helpers:** `capability_surface_params.rs` shared by optional legends, reuse recap, and ranked compact deltas; named optional params use `wire=p#` in capability input signature gloss.
- **GitHub catalog v25:** `pr_patch` → `pr_update`; eval cases gh-68…gh-72 for ranked replay and label authoring.

### Added

- **Tests:** ranked replay fixtures, `commit_exposure_wave_delta` integration, `pr_create` ranked-at-score-zero, stale symbol-space metadata, qualified diagnostics, compact delta gloss synthesis.

## [0.3.93] - 2026-06-30

### Fixed

- **Invoke arg materialization:** cap-qualified `p#` → wire resolution only at materialization (no entity homograph / fuzzy cap lookup); logical param names preserved in plan IR.
- **Teaching gloss dedup:** cap-parameter `p#` no longer aliases entity-field `p#` when compact meaning matches (`gloss_dedup` + typed slot identity).
- **Dry-run ≡ live preflight:** every staged surface typechecks; concrete `ir` compiles to CML; `ir_template` is typechecked-only (no bogus CML simulation).
- **MCP plan UI contract:** full comp DAG under `_meta.ui.plasm`; compact `_meta.plasm` / `structuredContent` for agents.

### Changed

- **`catalog_ownership`:** canonical `catalog_entry_id_for_invoke`, federated relation ownership, and stamped-catalog CGS resolution (parser, typecheck, dry, DAG).
- **`PreflightNormalized`:** `Simulatable` vs `TypecheckedOnly` — template dry nodes emit `template_stage` simulation metadata.
- **Symbol map:** O(1) `cap_taught_p_syms` index for invoke error hints.
- **Teaching gloss modules:** `gloss_dedup.rs` + `teaching_gloss_emit.rs` (renamed from MCP-coupled frontmatter).

### Added

- **Tests:** GitHub `issue_create` label JSON smoke, matrix Hermit invoke materialization, `workflow_apps_e2e` UI wire contract, `github_symbol_resolution` logical-param IR asserts.

## [0.3.92] - 2026-06-29

### Fixed

- **MCP GitHub symbol resolution:** entity-qualified `p#` reverse lookup for row projection (Label `[p#]` no longer resolves Issue homographs like `state` / `issue_type_color`).
- **MCP mutator args:** capability-qualified `p#` in invoke object literals (`issue_create`, `repo_content_put`, …) — fields resolve against the cap parameter contract, not a generic input object.
- **MCP ranked replay:** `plasm_context` session reuse with new `ranked_capabilities` replays exposure when ranked mutators are missing from the teaching surface instead of returning Unchanged-only.

### Changed

- **Symbol map indexes:** build-time reverse maps for entity/cap-qualified `p#` tokens (replaces O(n) scans in hot parse paths).
- **Tests:** GitHub symbol regressions and matrix homograph fixture extracted to `plasm_dag/tests/`; ranked replay integration test on read-first defer path.

## [0.3.91] - 2026-06-29

### Fixed

- **Row-to-text templates:** bind the projected list under both `rows` and the source binding name (`collection_alias` on `ComputeTemplate`) so `{% for r in items %}` works without forcing the magic `rows` identifier.
- **Row-to-text compile guard:** reject `${…}` in template bodies with actionable errors; canonical Minijinja-body scanner in `template_ref` (respects `{% raw %}` and `$$`).
- **Row-to-text runtime errors:** classify Minijinja undefined values via `ErrorKind` and append iteration hints (`{% for r in rows %}` plus source alias when set).

### Changed

- **Render compile:** `resolve_render_collection_alias` and `render_context_hint` live in `plasm_render_compile.rs` (removed from `plasm_dag.rs` / `RenderColumns`).
- **MCP dry-run transport:** compact agent `_meta.plasm` / `structuredContent.plasm`; full `comp` DAG and `plan_ux_reflection` under `_meta.ui.plasm` for MCP Apps.
- **Teaching / docs:** row-to-text one-render-over-list semantics, source-name alias, and null-coalescing idioms in `plasm_tool.txt`, language definition, and AGENTS.

## [0.3.90] - 2026-06-29

### Fixed

- **Row-to-text templates:** preserve teaching `p#` aliases on `ComputeOp::Render` (`column_aliases`) through plan → comp wire → live execute; Minijinja `rows` expose both `r.pN` and wire field names.
- **Invoke teaching legend:** unseeded `EntityRef` gloss on all capability invoke rows via centralized `capability_legend_with_session_gloss`.

### Changed

- **Compute IR unification:** agent plan compute types (`ComputeOp`, `ComputeTemplate`, `FieldPath`, `PlanPredicate`, …) re-export from `plasm_core`; step convert uses direct clone instead of JSON bridging (prevents silent wire-field drift).
- **Render compile:** `RenderColumns` bundle + extracted `plasm_render_compile.rs`; strict alias validation at compile time.
- **MCP smoke scripts:** `plasm_run` calls use `run_ref`; HTTP progress URLs unchanged (`plan_commit_ref=`).
- **Teaching / tool contract:** row-to-text worked example in `plasm_tool.txt`; matrix views e2e asserts `column_aliases` on comp wire.

## [0.3.89] - 2026-06-29

### Fixed

- **Discovery catalog routing:** honor `registry_aliases` when matching intent tokens (e.g. `pokemon` → `pokeapi`); compound intents route multiple catalogs instead of homograph-filtering to a single `entry_id`.
- **Discovery presentation:** emit `# decision: clarify` when multi-catalog routing yields rows from only one API; surface `# routed:` and `_meta.plasm.discovery.catalog_route`.
- **MCP `plasm_run` transport:** when a run snapshot exists and rows exceed the in-band cap, omit duplicate inline TSV — artifact URI plus required read instruction only; suppress paging/`has_more` when the snapshot holds the full batch (`artifact_complete` in `_meta`).

### Changed

- **Discovery contract:** `CatalogRoute` newtype and `DiscoveryDecision::for_presentation()` live in `plasm-core`; MCP publish transport uses `StepInBandMode::resolve`, `ResolvedStepPublish::is_truncated_for_transport`, and `McpResultTransportPolicy::exceeds_in_band`.
- **MCP tool copy:** imperative snapshot-read guidance in `plasm_run` / initialize workflow assets; docs for artifact-only policy and stale MCP `tools/list` cache refresh.

## [0.3.88] - 2026-06-29

### Fixed

- **MCP Claude Desktop artifact read:** classify wire `clientInfo.name` `claude-ai` as `ToolFallback` so
  `plasm_read_run_artifact` is listed for model-callable snapshot reads (Desktop does not expose
  `resources/read` to the agent toolkit like Cursor).

### Changed

- **Agent program teaching:** `plasm` tool contract and language docs now require **one** final return line
  (comma-separated roots); clearer diagnostic when agents stack bare binding labels on separate lines.

## [0.3.87] - 2026-06-29

### Added

- **@plasm_lang/vercel-agent 0.3.87:** Eve-aligned Vercel deploy — Blob-only state/archives,
  Nitro scheduled tasks, Workflow dispatch routes, optional `x-vercel-cron` auth.
- **mcp-radar template:** Vercel Workflows scan path, blob proof store, storage provision script.

### Changed

- **Vercel serverless bundle:** split `readBuildManifest` from build CLI so prod handlers no longer
  trace `@workflow/nitro` / `@swc/core` at runtime.
- **CEP-14:** unify brand-new ref convergence (identical concurrent cold reads commit on both
  branches; divergent same-key writes remain single-winner).

## [0.3.86] - 2026-06-28

### Changed

- **MCP artifact access:** collapse client detection to researched wire-name exact match plus
  one undocumented Anthropic connector heuristic; remove presumptive `openai-mcp` entry.
- **MCP HTTP:** capture `User-Agent` on Streamable MCP requests (unified listener) keyed by
  `mcp-session-id` for artifact-access detection.
- **MCP `resources/read`:** extract handler to `resource_read.rs`; canonical
  `plasm_read_run_artifact` tool description in prompt assets.

## [0.3.85] - 2026-06-28

### Changed

- **MCP artifact access detection:** classify researched `initialize.clientInfo.name` wire
  values (`claude-code`, `Anthropic/ClaudeAI`, `Anthropic/API`, `openai-mcp`, plus existing
  connector heuristics) as `ToolFallback` so `plasm_read_run_artifact` is listed and markdown
  matches agent toolkit; `cursor-vscode`, `claude-ai`, and `Cline` stay `ResourcesRead`.
- **MCP observability:** log `client_info.name`, version, and resolved `ArtifactAccessMode` on
  first transport cache.

## [0.3.84] - 2026-06-28

### Added

- **MCP artifact access gate:** per-transport `ArtifactAccessMode` (`resources/read` vs
  `plasm_read_run_artifact` tool fallback) with `PLASM_MCP_ARTIFACT_ACCESS=tool|resources`,
  initialize client detection, and optional `PLASM_MCP_CLIENT_USER_AGENT`.
- **MCP `plasm_read_run_artifact`:** tool-only fallback for run snapshot JSON (parity with
  `resources/read` via shared `artifact_resolve`).

### Changed

- **MCP `plasm_run`:** unified **`run_ref`** parameter replaces `plan_commit_ref` and `page_handle`
  (commit `pcN` or paging handle). Dry-run `_meta.plasm` and markdown footers emit **`run_ref`**;
  paging meta uses **`next_run_ref`**. HTTP execute retains query `plan_commit_ref=pcN` and program
  `page(pgN)`.
- **MCP `tools/list`:** gate-aware `plasm_run` description; `plasm_read_run_artifact` listed only on
  tool-fallback transports.
- **Live run publish:** `mcp_result_policy` threaded via `LiveRunSpawnOpts` / `OpAcceptContext`
  (removed from `PlanRunTraceHooks`).

## [0.3.83] - 2026-06-27

### Added

- **MCP `plasm_run` paging:** optional `page_handle` (mutually exclusive with `plan_commit_ref`) for
  live continuation without a second `plasm(page(...))` dry-run; markdown footer hints `page_handle`
  only.
- **MCP ingress:** unified `execute_mcp_live_run` for reviewed commits and paging continuations;
  `resolve_mcp_live_run_ingress` extracted from the handler.

### Changed

- **MCP prompts / tool cards:** `plasm_run` and workflow initialize text aligned with `page_handle`
  workflow; insta snapshots updated.
- **plan_prepare:** `collect_plan_entity_names` includes relation target entities (unused-seeds fix).

### Fixed

- **Compile:** `infer_surface_contract` skips catalog entity resolution for `ResultShape::Page`
  (`page(handle)` programs).


### Fixed

- **@plasm_lang/vercel-agent:** use `LegacyOpenTelemetry` from `@ai-sdk/otel` (replaces removed
  `OpenTelemetryIntegration` export) so `plasm-agent build` and agent runtime load on current AI SDK.
- **@plasm_lang/vercel-agent:** bump `@ai-sdk/otel` to `^1.0.3` for the stable export surface.

## [0.3.81] - 2026-06-27

### Fixed

- **Federated execute rehydrate:** `catalog_waves_from_pairing` now run-length-encodes entity/catalog
  rows in pairing order (matches live federate waves). Cross-pod replay no longer regroups by catalog
  and reassigns `e#`/`p#` on interleaved sessions (e.g. linear → github → linear).
- **Symbol map LRU:** `SymbolMapCacheKey` fingerprints opaque assignment maps so the cross-request cache
  cannot serve a differently-numbered `SymbolMap` under a colliding surface key.

### Changed

- **MCP prompts:** smaller `initialize_workflow` / `plasm_tool` cards; symbols-only projection and
  row-filter diagnostics; `symbol_tuning` hash helper extracted to `opaque_symbol_hash.rs`.
- **Tests:** shared `test_support/exposure_replay_fixtures` for federated replay / rehydrate parity.

## [0.3.80] - 2026-06-27

### Changed

- **MCP tool prompts:** teach **symbols-only** programs (`e#`/`m#`/`r#`/`p#` from teaching TSV) — remove
  federated homonym / bare wire-name framing that blocked agents before `plasm_context` returned symbols.
- **plasm-agent devx:** in-package mcp-radar template, tsx CLI entry, Vercel build contract smokes.

## [0.3.79] - 2026-06-26

### Fixed

- **CEP-14 concurrent execute write conflicts:** branch commit validation is content-divergence
  aware (per-field / relation / completeness three-way merge) with lazy fork-base capture on
  first branch mutation per `Ref`. Parallel plan roots that share an ancestor (e.g.
  `e3("electric").r5[…]` and `e3("electric").r4[…]`) no longer spuriously fail with
  `session materialization changed during concurrent execute` when writes are additive or
  idempotent. New-`Ref` insert races remain CEP-3 single-winner semantics.

## [0.3.77] - 2026-06-26

### Added

- **MCP language conformance guards:** extract `plasm_dag_surface_guards` module; reject `=>` relation
  hops (teaching `r#` and wire names), bare JSON/data literal program roots on surface-line and DAG paths,
  and misleading `.content` on non-render bindings.
- **Teaching / smoke:** scalar predicate quote hint in predicate context; MCP language conformance smoke
  script; conformance probe table in docs.

### Fixed

- **Derive trap:** `source => e#.r#` / `source => binding.wire` no longer lowers as silent derive literals.
- **Literal no-op:** `{"foo":"bar"}` as a program root fails with actionable copy on all compile entry paths.

## [0.3.76] - 2026-06-26

### Added

- **Federated write triage:** pokeapi→linear dry-run smoke locks singleton GET + single write + non-fanout
  return shape; Linear `issue_create` exposes `description`; federated write recipe in Linear README.
- **GraphQL mutation envelope errors:** `success: false` wrappers on mutation paths surface actionable
  business failures before opaque items_path narrowing.
- **Teaching vocabulary:** disambiguate MCP handles, EntityRef slots, identity syntax, bindings, `$`
  placeholders, and rare exemplar literals (`pikachu`); keep positional identity exemplars only for
  high-signal cases (Pokemon, name-field, email).

### Fixed

- **D1 foreach fanout:** singleton `get` sources no longer count as foreach fanout risk; cardinality
  recomputed at plan prepare/dry-run instead of stored on validated nodes.
- **Discovery read-first gate:** self-relation mutation closure respects seeded target entities.
- **Parser:** nested id constructor sugar resolves via `sym_map.resolve_ident`.
- **Catalog-stale tests:** `CGS::fresh_catalog_digest()` for post-mutation hash recomputation.

## [0.3.75] - 2026-06-25

### Fixed

- **Agent-facing program errors:** multi-line programs with intermediate roots-only postfix lines
  (e.g. bare `comments.filter{…}` before a final projection) now fail with imperative bind-step
  guidance instead of leaking internal `return_1` duplicate-node ids. Bindings after a return line
  are rejected (`Return must be last`).
- **SymbolicLlm parse wrapper dedup:** MCP/HTTP agents see correction text only (no repeated
  `parse error at offset …` plus correction). `correction_predicate_field`, `correction_not_navigable`,
  and `correction_navigation_name` stay short when session teaching symbols are available.
- **Shorter row/compute diagnostics:** unknown postfix transforms, row field mismatches, and
  `.content` root misuse use concise imperative + `help:` lines.

## [0.3.74] - 2026-06-25

### Fixed

- **Flat single-line trailing-root clobber:** space-separated programs ending in a projection or
  postfix on an in-scope binding (e.g. `comments[p2,p14]`, `comments.limit(5)[p2,p14]`) now keep
  that expression as the return instead of silently replacing it with the first binding. Bare
  side-label echo (`… labels`) and fresh-entity trailing roots still coerce to the first binding.

## [0.3.73] - 2026-06-25

### Fixed

- **Multiline return position:** explicit trailing roots lines (ML-style final expression after
  bindings) are no longer overwritten by first-binding coercion — e.g. `limited[p2,p14]` after
  relation hop + `.limit` returns `limited`, not the first binding. Flat single-line space-split
  and binding-only omission sugars are documented as disjoint tiers in
  `docs/plasm-language-definition.md`.

## [0.3.72] - 2026-06-24

### Fixed

- **Agent-facing unknown-entity parse errors:** MCP/HTTP `SymbolicLlm` feedback is one short line
  (session `e#` summary capped at six rows) — no catalog dumps, Levenshtein guesses, or repeated
  expression lines.
- **Federated extend symbol drift:** after `plasm_context` federates a new catalog row (e.g. linear
  `e1`–`e3` then github `e4`), `plasm` / execute ingress no longer reuse a stale session-local
  `SymbolMap` memo from before the extend wave when the exposure fingerprint advances — TSV `e#` and
  compile-time resolution stay aligned on the same binding.
- **Program default-return coercion:** newline-separated binding lines (common when agents split a
  logical single-liner across lines) now coerce the first binding as return, same as one
  space-separated physical line; missing-roots parse errors are one short correction line.

## [0.3.71] - 2026-06-24

### Added

- **Regression coverage — relation-hop + `.limit` + projection scope:** new `plasm-agent-core` tests
  (`relation_hop_limit_projection_resolves_against_target_entity` — a table-driven case over
  `from_parent_get` / `query_scoped` materialize with wire and opaque `e#`/`r#`/`p#` symbols,
  `relation_hop_limit_then_separate_projection_resolves_target_entity`, and
  `linear_issue_comments_limit_projection_opaque_resolves_target` against the real `apis/linear`
  catalog) lock in that a projection after a relation hop with an intervening postfix transform
  resolves field tokens against the relation **target** entity (not the receiver).
- **`PLASM_MCP_RUN_AWAIT_MAX_SECS`:** env-tunable ceiling (default **600s**) for server-side terminal
  await (MCP `plasm_run`, HTTP `wait`); a stuck upstream operation now surfaces an explicit timeout at
  this bound instead of always hanging for the full default budget.

### Changed

- **Clearer "bare roots" diagnostic:** the multi-line program error now states that the
  default-return-is-first-binding coercion applies only to single-physical-line, space-separated
  programs; multi-line programs with two or more bindings must end with an explicit roots line.

## [0.3.70] - 2026-06-24

### Changed

- Semver tag for monorepo release: npm OIDC auth sourced-script exit fix (PlasmTools/plasm CI).

## [0.3.69] - 2026-06-24

### Changed

- Semver tag for monorepo release: Circle/npm OIDC trust ID verification in CI (fail fast before NAPI build).

## [0.3.68] - 2026-06-24

### Changed

- Semver tag for monorepo release: GitHub `--latest` install manifest + npm OIDC publish CI fixes on PlasmTools/plasm.

## [0.3.67] - 2026-06-24

### Changed

- Semver tag for monorepo release: npm OIDC auth no longer gates on `npm whoami` (OIDC only applies at publish).

## [0.3.66] - 2026-06-24

### Changed

- Semver tag for monorepo `v0.3.66` release (npm OIDC publish CI on PlasmTools/plasm; no OSS crate changes since 0.3.65).

## [0.3.65] - 2026-06-23

### Fixed

- **Parser:** union constructor brace maps resolve opaque `p#` keys via `SymbolMap::resolve_ident` (fixes teaching validation for capabilities such as `document_edit_v2` on Proof `Document`).
- **Teaching validation:** `line_validate` uses one `parse_session_line` → normalize → typecheck → wire render pass per cache miss (no string pre-expansion workaround).

### Changed

- **Docs:** language definition, incremental teaching prompts, and README describe in-grammar symbol resolution (removed `expand_path_symbols` / `expand_expr_for_teaching_session` ingress narrative).

## [0.3.64] - 2026-06-23

### Changed

- **Parse ingress:** in-grammar session [`SymbolMap`] resolution via `parse_session_line` and `parse_session_line_with_rewrite_recovery` (REPL, eval, execute) — no string pre-expansion before parse.
- **Teaching validation:** `prompt_render/line_validate.rs` fuses opaque parse, normalize, typecheck, and wire render into one pass per cache miss (eliminates double-parse in teaching synthesis).
- **Dry plan display:** session-aware wire surface via `render_expr_wire_for_execute_session` (compact IL summaries remain on separate hint surfaces).
- **Plan lowering:** one-cardinality relations over plural sources lower as per-parent fanout without requiring `Plan.singleton(...)`.

### Fixed

- **Teaching synthesis:** `receiver_for_dotted_suffix` returns anchor receiver only (callers append suffix once).
- **Execute errors:** collapsed redundant `expanded`/`source_line` params on session parse error messages.

## [0.3.63] - 2026-06-23

### Changed

- **npm CI:** Circle OIDC trusted publishing (`NPM_ID_TOKEN`) replaces `NPM_TOKEN`; see `docs/npm-publish-plasm-lang.md` and `scripts/ci/npm-trust-circleci-packages.sh`.

## [0.3.62] - 2026-06-23

### Fixed

- **npm CI publish:** force `--auth-type=legacy` (npm 11 defaults to web/browser login); npmrc uses `${NPM_TOKEN}` env expansion.

## [0.3.61] - 2026-06-23

### Fixed

- **npm CI auth:** isolated `NPM_CONFIG_USERCONFIG` + `npm whoami` gate (self-hosted runner `~/.npmrc` was triggering web login instead of `NPM_TOKEN`).

## [0.3.60] - 2026-06-22

### Fixed

- **Circle runner reuse:** reset `plasm-oss` before submodule checkout and restore after npm publish (sync/prepublish dirty `package.json`).

## [0.3.59] - 2026-06-22

### Fixed

- **npm publish on Circle:** `napi prepublish --skip-optional-publish` + explicit platform `npm publish --provenance=false` (prepublish was publishing optionals without provenance disable).

## [0.3.58] - 2026-06-22

### Fixed

- **npm NAPI artifacts:** Circle publish passes `--config-path napi.json` so `binaryName` is `plasm-engine`, platform npm dirs are created, and `napi artifacts` maps triples correctly.

## [0.3.57] - 2026-06-22

### Fixed

- **npm publish on Circle:** disable `--provenance` (GitHub Actions OIDC only); NAPI builds use `--platform` so optional platform packages include `.node` binaries.

## [0.3.56] - 2026-06-22

### Fixed

- **npm NAPI artifacts:** Circle publish passes a repo-relative artifact dir to `napi artifacts -d` (`@napi-rs/cli` breaks on absolute paths).

## [0.3.55] - 2026-06-22

### Fixed

- **npm NAPI docker build:** run napi compile step under `bash -ec` (Debian `/bin/sh` lacks `pipefail` and `[[`).

## [0.3.54] - 2026-06-21

### Fixed

- **npm NAPI linux build:** Circle `napi-engine-linux-gnu` cross-compiles on `$BUILDPLATFORM` via `rust-cross-setup.sh` (avoids QEMU `rustc` SIGSEGV on Apple Silicon runners).

## [0.3.53] - 2026-06-21

### Fixed

- **GitOps / ArgoCD:** `packages/plasm-agent/agent/catalogs/execute_tiny` symlink is repo-relative (was an absolute dev-machine path that blocked Argo manifest generation and pruned ingress).

### Changed

- **npm publish:** `@plasm_lang/engine` and `@plasm_lang/vercel-agent` publish via Circle **`npm_publish_plasm_lang`** on tag pipelines (replaces GitHub Actions workflow).

## [0.3.51] - 2026-06-21

### Fixed

- **Federated parse feedback on MCP/HTTP plan compile:** DAG and program compile paths now append SymbolicLlm corrections (session `e#` stamp lists for homonymous entities) via `format_session_symbolic_parse_error`.
- **Federated mutator `p#` params:** `expand_method_invoke_capability_param_keys` rewrites `.m#(…)` capability arg keys and nested `e#(…)` constructor field keys before global symbol expansion — fixes `e2.m9(p35=e3(p44=EVA), …)` dry-run without wire-name-only workarounds.

### Added

- **Tests:** `federated_ambiguous_entity_parse_includes_session_stamps`, `federated_linear_issue_create_dry_run_preflight_compiles_p_sym_tokens`.

## [0.3.50] - 2026-06-21

### Fixed

- **Federated ambiguous-entity feedback:** parse errors for homonymous wire names (e.g. bare `Issue.create`) now list matching session `e#` stamps (`e1` → github:Issue, `e2` → linear:Issue) via `SymbolMap::entity_stamps_for_wire`.
- **Linear issue create dry-run:** `apply_preflight_compile_stubs` injects compile-time preflight merge keys (`teamId`, …) so `issue_create` CML compiles in plan review without live HTTP hydration.

### Added

- **Tests:** `ambiguous_entity_catalog_feedback_lists_session_stamps`, `federated_linear_issue_create_dry_run_preflight_compiles`.

## [0.3.49] - 2026-06-21

### Fixed

- **Federated FromParentGet materialize:** `try_materialize_from_parent_get_relation` and `materialize_prefer_from_parent_get_relation` resolve source CGS via stamped `(entry_id, entity)` (`resolve_cgs_for_entry_entity`) instead of ambiguous `resolve_cgs_for_entity(..., None)` — fixes live `parent.r2` / linear `Issue.children` after v0.3.48 TC/DAG hardening.

### Added

- **Test:** `resolve_cgs_for_entry_entity_federated_homonym` in `catalog_ownership`.

## [0.3.48] - 2026-06-21

### Fixed

- **Federated catalog ownership:** every schema lookup at type-check, DAG, parser, and dry-run boundaries keys on `(entry_id, entity)` — not wire name or symbol alone.
- **`type_check_chain_federated`:** resolves source CGS from `chain.source.session_catalog_entry_id()` via `resolve_cgs_for_catalog_entity`; relation navigation uses the stamped catalog (fixes `e2.r2` / linear `Issue.children` vs github `sub_issues`).
- **DAG:** `resolve_cgs_for_qualified_entity` no longer scans catalogs by entity name; `lookup_relation_chain_meta` fails closed in federated sessions without row QE; text-parse continuations pass `contract.row_entity`.
- **Parser:** `cgs_for_entity_required` errors on multi-catalog homonyms; relation nav uses `cgs_for_expr_source`; federated parse threads parallel `layer_catalog_entry_ids` from session contexts.
- **Dry-run:** `ensure_relation_expr_matches_plan` requires `catalog_entry_id` on federated relation hops.

### Added

- **Tests:** real `apis/github` + `apis/linear` coverage for `Issue.children` chain type-check, DAG compile/dry-run, and `lookup_relation_chain_meta_requires_qe_federated`.

### Changed

- **Docs:** [`docs/plasm-language-definition.md`](../docs/plasm-language-definition.md) federated catalog ownership invariant at compile/TC/plan boundaries.

## [0.3.47] - 2026-06-21

### Fixed

- **Federated symbol tuning:** relation continuation lowers through row-hole IR with `row_entity.catalog_entry_id` — homonymous entities (`Issue`, `LangItem`) no longer resolve via bare wire names across catalogs.
- **Federated mutators:** `m#` and stamped `e#` anchor `catalog_entry_id` on dotted create/update/action/delete; ambiguous bare kebab labels fail closed when multiple catalogs expose the same method wire.
- **Federated queries:** `normalize_expr_query_capabilities_federated` honors `QueryExpr.catalog_entry_id` so `e2{…}` list queries match the correct catalog capability set.

### Added

- **Postfix chain:** `.group_by(keys).aggregate(specs)` is primary syntax; fused `group_by(key, n=count)` remains sugar.
- **Language matrix:** federated homonym rows (`r#` hop, `m#` mutator, parallel roots, group-by on `e1`, inline template on `e#`); MCP integration dry-run for federated relation hop.
- **`FederationDispatch::cgs_for_catalog_entry_id`:** canonical lookup for stamped-surface query/mutator resolution.

### Changed

- **MCP prompts:** single-brace filter grammar; homonym doctrine for all symbol classes (`e#`/`m#`/`r#`/`p#`); federated examples and chained group-by teaching.
- **Docs:** [`docs/plasm-language-definition.md`](../docs/plasm-language-definition.md) federated sessions + group-by chain syntax.

## [0.3.46] - 2026-06-21

### Fixed

- **Catalog IL wire format:** replace CBOR with canonical JSON bytes (`*.cgs.json`, `format_version = 2`, manifest `cgs_json`). Fixes large catalogs (e.g. clickup) that exceeded ciborium decode recursion limits on startup.
- **Release packaging:** stray quote in `oss-release-pack-native.sh` tar step.

### Changed

- **`PLASM_CATALOG_FORMAT_VERSION`:** `1` (CBOR) → `2` (JSON IL aligned with [`CGS::catalog_cgs_hash_hex`](crates/plasm-core/src/schema.rs)).

## [0.3.45] - 2026-06-21

### Changed

- **Catalog distribution cutover:** native compile `cdylib` plugins (`plasm-plugin-*`, `--compile-plugin`, `EXECUTION_PLUGIN_HOOKS`) removed in favor of **CBOR IL + JSON manifest** artifacts loaded via **`--catalog-dir`** and **`plasm-pack-catalogs`**.
- **Registry loader:** `catalog_data` single-pass version resolution with fail-fast artifact validation; IL boundary consolidated in `plasm-core::catalog_il`.
- **Hot reload:** control-plane **`POST /internal/catalog-registry/v1/reload`** (replaces plugin-registry path).

### Docs

- **Doc-site:** operator docs updated for **`--catalog-dir`**, **`plasm-pack-catalogs`**, and catalog-registry reload (no cdylib/plugin-dir paths).

### Removed

- **`plasm-plugin-abi`**, **`plasm-plugin-host`**, **`plasm-plugin-stub`**, **`plasm-pack-plugins`**, and **`plugin_generation_id`** session reuse key.

## [0.3.44] - 2026-06-21

### Fixed

- **Chained `from_parent_get` embeds:** nested relation embeds (e.g. `summary.detail`) decode transitively into the session graph and plan `row_source` preserves nested wire shape — fixes chained-hop materialize/hydrate (`lang_relation_hop_one_one`).
- **CEP-10 embed depth:** bounded iterative expand at HTTP decode (`decode_entities_with_cgs`), graph insert (`flatten_decoded_embed_descendants`), and wire-row rebuild (`wire_row_with_from_parent_embeds`) — cap [`MAX_FROM_PARENT_GET_EMBED_DEPTH`](crates/plasm-core/src/relation_materialize.rs); leaf embed decoders have no nested `.relations`.

### Changed

- **Embed decode compile/runtime split:** `embed_target_decoder`, `embed_tree`, `execution/embed_cache`; shared `entity_decoder_for_from_parent_get_target`; runtime GET paths use `decode_entities_with_cgs`.
- **Plan embed materialize:** `resolve_embed_target_entities` + `finalize_embed_relation_materialized_node` unify cached-ref and `from_parent_get` finalize paths; truncate before wire-row embed closure.
- **Template interpolation:** `plasm-core::text` module (`Utf8Text`, brace/dollar template IR) replaces ad-hoc `template_interpolate` internals.

### Tests

- **CEP-10:** `embed_decode`, `wire_row_embeds_declared_relation_from_graph`, `lang_relation_hop_one_one` matrix row; `scripts/guards/check_no_graph_recursion.sh` depth-cap guard.

## [0.3.43] - 2026-06-20

### Changed

- **Static MCP grammar cutover:** canonical Plasm language grammar (composition, postfix, heredoc, `binding.content`, row-to-text Minijinja) lives as compile-time consts in [`plasm-core/src/prompt_render/assets/`](crates/plasm-core/src/prompt_render/assets/) — primarily [`PLASM_TOOL_DESCRIPTION`](crates/plasm-core/src/prompt_render/mcp_tool_descriptions.rs). MCP `tools/list` descriptions are static; MCP initialize remains a supplementary workflow rollup only.
- **Removed dynamic guidance:** deleted `render_prompt_contract_dense`, grammar-revision hashing, HTTP `grammar_revision` on execute sessions, and per-wave `#` grammar comments in teaching TSV. Teaching tables are always table-only (`plasm_expr` / `Meaning`).
- **Eval / REPL / `plasm init`:** prepend the same static `PLASM_TOOL_DESCRIPTION` as production MCP agents see (`plasm-eval` first user turn).
- **Default read paging unified at 25:** `DEFAULT_HOST_PAGE_SIZE` is the single source of truth for unbounded list/query read roots (was 50); the MCP in-band row cap (`MCP_IN_BAND_ENTITY_ROW_CAP`) derives from it so the first host page always fits one MCP tool response. Dry-run treats paged-by-default reads as bounded (`plan ok`, not `plan review`); the unbounded-root warning now states the read returns the default host page (not all pages).

### Refactored

- **Shared exposure-wave commit tail:** federate and expand converge on `commit_exposure_wave_delta` ([`http_execute/context/session/commit.rs`](crates/plasm-agent-core/src/http_execute/context/session/commit.rs)), deleting duplicated derive-slots → admit → render-delta → persist logic.
- **`apply_capability_seeds` decomposition:** split into `resolve_execute_binding` (gate-free) and `commit_federate_and_expand_waves` (two-pass: federate network I/O before per-row CEP-13 exposure-commit gates), preserving the lock-light, agent-governed concurrency design.
- **Relation-slot repair:** `relation_slots_for_expand_wave` surfaces relation hops whose target entity qualifies in a later wave and assigns them `r#` symbols.
- **Dependency hygiene:** removed dead `ExecuteSessionGetQuery`, dead `principal` threading on the federate commit path, and `O(n²)` symbol-tuning dedup; inverted the `plan_read_bounds` ↔ `mcp_run_markdown` page-size dependency.

### Fixed

- **`expr_parser` id-field brace sugar:** `Entity{id=<literal>}` is referentially `Entity(<id>)` — rewritten to a `Get` that preserves string-typed ids verbatim (no integer coercion / precision loss on large numeric id literals).

### Tests

- **CEP-13 concurrent `plasm_run`:** `concurrent_mcp_plasm` asserts two concurrent legs on one execute session resolve distinct, correctly-namespaced operation handles.
- **Fixture-only language coverage:** relation-slot repair regression migrated from `apis/pokeapi` to the abstract `plasm_language_matrix` fixture (strict-rule compliance).

## [0.3.42] - 2026-06-20

### Fixed

- **Row compute postfix:** `.sort(p#, desc)` / `.group_by(p#, …)` / `.filter{…}` / `.dedupe` / `.distinct` resolve teaching `p#` symbols before schema checks; diagnostics steer to `rows:` contract instead of wire column names.
- **Default read paging:** unbounded query/list read roots get host page size 50; `plasm_run` surfaces `page(l_<token>_pgN)` continuations in-band.

### Changed

- **MCP language frontmatter:** symbolic postfix bullets (`p#` primary) and first-page paging guidance in grammar contract.
- **MCP session copy:** stable `intent` definition + anti-pattern in `plasm_context` workflow; removed agent-facing `unused_seeds` / over-seeding warnings (lazy evaluation — not an agent concern).
- **Docs:** `plasm-language-definition.md`, `plasm-row-compute.md`, `mcp-session-reuse.md`, `incremental-teaching-prompts.md` aligned with symbolic row fields and session discipline.

## [0.3.41] - 2026-06-20

### Changed

- **Concurrent execute invariants:** formalized CEP-1..12 as named, test-backed properties; added release-gated checks for graph version monotonicity, fan-out ordering, GraphBacked parent coherence, pre-layer materialized snapshots, and per-store write conflicts.
- **Graph execute commits:** per-store optimistic validation now uses graph `Ref`, response fingerprint, and query-index write sets so disjoint branches can commit without false conflicts while contended writes are retried at branch scope only.
- **Relation materialization:** projected rows retain canonical graph identity for project-then-relate flows, preferring graph parent refs and rejecting thin projected fallbacks when identity-bound lookup misses.
- **Teaching TSV:** bare query row emission is driven by explicit row-producer projection intent instead of string-shape heuristics.

## [0.3.40] - 2026-06-20

### Changed

- **MCP `plasm_context` reuse:** compact `e#=Entity` status line instead of verbose grammar replay; `render_compact_exposure_symbol_map` on reuse paths.
- **Unified `discover_capabilities`:** single TSV-first agent surface with `# decision:` / language-flow preamble; removed typed JSON mode and `typed`/`allowed_entry_ids`/embedding params from MCP tool schema; `_meta.plasm.discovery.decision` on every response.
- **Discover tool copy:** imperative “Plasm is a source language” entry-point framing in `tools/list` and initialize workflow.

## [0.3.39] - 2026-06-19

### Added

- **MCP prompt cutover:** canonical syntax/workflow fragments in `plasm-core` `mcp_prompt_fragments`; self-contained `tools/list` descriptions (no “see MCP initialize”); slim initialize rollup.
- **MCP run transport caps:** `McpResultTransportPolicy` — 25-row in-band limit; snapshot-backed large results defer to compact preview + `resources/read` (fixes megabyte inline TSV on `plasm_run`).
- **MCP publish refactor:** `http_execute/mcp_publish/{policy,render,meta}` modules; wired `PlasmMetaIndex` through `PlanRunTraceHooks` for live runs.

### Changed

- **MCP `plasm_run` copy:** removed redundant “do not echo program” from execute tool (schema has no `program`); review gate + `plan_commit_ref` only.
- **Discover TSV:** `outgoing_relations` column in discovery markdown tables.
- **Unified preview row cap:** `_meta.plasm` preview entities align with in-band row cap (25).

## [0.3.38] - 2026-06-19

### Added

- **Claude Web MCP OAuth discovery:** `/.well-known/openid-configuration` on plasm-mcp (same metadata as `oauth-authorization-server`) for ingress-routed production hosts.
- **Smoke:** `scripts/smoke/mcp-claude-web-oauth-chain.sh` — well-known metadata, DCR, optional full PKCE + initialize chain.
- **Docs/UI:** `docs/claude-web-mcp-connector.md`; Claude Web installer in project MCP settings.

### Changed

- **OAuth observability:** structured tracing for token mint and bearer verify accept/reject paths.
- **Phoenix compat:** `OauthDiscoveryCompatController` advertises `refresh_token` grant (local dev parity).

## [0.3.37] - 2026-06-19

### Changed

- **MCP inbound OAuth:** full cutover to auth-framework — DCR (`ClientRegistrationManager`), session KV (`oauth_auth_code:*`, `oauth_refresh:*`), and access tokens via `JwtManager` with RFC 8707 `resource` / `aud` scoping. Removed custom `plasm:incoming_oauth:*` KV.
- **Module split:** `mcp_inbound_oauth/` submodules (resource, client, session store, JWT, grants, DCR); `mcp_stream_auth.rs` is a thin HTTP/`AuthProvider` adapter. Single client store via DCR registration KV only; peek-then-consume auth codes; typed JWT claims.

## [0.3.36] - 2026-06-19

### Added

- **Inverse embed prefer + hydrate fallback:** `prefer_from_parent_get` with `hydrate_from_embed_path` plan-materializes per-ref GET jobs when wire embed is empty; `GetExpr.capability_name` honors catalog `get_capability`.
- **Plan helper module:** `plasm_plan_run::prefer_embed_hydrate` — wire ref extraction, graph-resident partition, hydrate GET job fan-out.
- **PokeAPI inverse relations restored:** `Type.pokemon` and `Ability.pokemon` use prefer embed with hydrate fallback (optional `fallback.path` defaults to prefer path).
- **Runtime guard:** `hydrate_from_embed_path` is plan-materialized only; runtime returns `ConfigurationError` if invoked directly.
- **Tests:** `prefer_embed_hydrate` wire/relation-ref coverage; runtime plan-only fallback test; entity decoder inverse embed tests.

### Changed

- **CGS validation:** plain `from_parent_get` embed graphs must stay acyclic; `prefer_from_parent_get` edges excluded from cycle detection.
- **Docs:** CEP-10 wording in `docs/concurrent-execute-invariants.md`; mutual inverse embed pattern in plasm-authoring reference.
- **Smoke:** optional `PLASM_SMOKE_PROGRAM_PK03` in `scripts/smoke/mcp-pokeapi-pc1-bounded.sh`.

### Fixed

- **Type.pokemon / Ability.pokemon live execute:** inverse relation nav resolves via prefer embed + hydrate GET without stack blow-up from cyclic embed graphs.

## [0.3.35] - 2026-06-19

### Added

- **Incremental teaching exposure replay:** `TeachingExposureSession` replays federated seed waves with monotonic `e#`/`m#`/`p#`/`r#`; scoped `r#` expansion emits `plasm_expr` on relation symbol rows; `prompt_render` module split from symbol tuning.
- **CEP-10 single-hop embed cutover:** flat `embed_decode` (no nested `.relations` queue); CGS cycle validation for `from_parent_get`; plan-scoped single-hop lookup; PokeAPI inverse embeds removed on `Type.pokemon` / `Ability.pokemon`.
- **CI guard:** `check_no_graph_recursion.sh` bans decode recursion env knobs and nested relation compile paths.
- **Smoke:** optional `scripts/smoke/mcp-pokeapi-pc1-bounded.sh` species hop boundedness check.

### Changed

- **Relation embed / decoder:** `wire_rows_with_path_embeds`, `extend_lookup_single_hop_embeds`, shared `json_to_value`; decoder construction extracted to `entity_decoder` module.
- **Docs:** incremental teaching exposure replay section in monorepo `docs/incremental-teaching-prompts.md`; CEP-10 in `docs/concurrent-execute-invariants.md`.

### Fixed

- **Graph recursion stack overflow:** relation materialize and wire embed cap at one hop (fixes unbounded `Pokemon.species` / cyclic PokeAPI embed chains during live execute).

## [0.3.34] - 2026-06-19

### Added

- **Concurrent execute invariants (CEP-1..9):** documented in monorepo `docs/concurrent-execute-invariants.md`; Shuttle PCT tests are the primary graph fork/commit validation gate (no feature flag).
- **CEP-4 materialize lock refactor:** `RelationEmbedSnapshot` — single graph lock for prefer-from-parent-get embed resolution; HTTP fan-out and spill rehydrate run without the session mutex.
- **`plasm-core::partition_prefer_resolutions`:** shared embed vs scoped classification for plan and runtime paths.
- **Operation lifecycle tests:** terminal failures surface as `OperationFailed` (not `UnknownHandle`); MCP await propagates terminal state (CEP-7/8).
- **E2E:** `lang_relation_hop_one_one` regression for nested `from_parent_get` relation decoders; graph spill + relation coverage extended.

### Changed

- **Stale-epoch retry:** branch-level only (`run_with_stale_epoch_retry`); removed full-plan stale retry that could re-issue committed mutating lines.
- **`materialize_prefer`:** apply-only orchestration; source-entity CGS lookup for relation cardinality (federated-safe).
- **CI guard:** `check_graph_cache_concurrency.sh` enforces CEP-4/5 lock routing, no await-under-lock in plan materialize paths.
- **Plan tests:** decomposed `integration_tests.rs` into focused modules under `plasm_plan_run/tests/`.

### Fixed

- **Cached-embed materialize:** `resolve_row_source_rows` no longer runs under graph lock (spill I/O CEP-4 violation).
- **Nested relation hop:** decoder + plan materialize preserve chained embed keys (`summary.detail` two-hop matrix case).
- **Shuttle:** `shuttle_parallel_fanout_commits` asserts exactly one commit winner; branch retry loop progress under bounded contention.

## [0.3.33] - 2026-06-18

### Added

- **Parallel plan execute:** bind-graph layer scheduler runs independent read/pure comp steps concurrently; row fan-out and relation GET hydrate use bounded parallel HTTP (`PLASM_PLAN_HTTP_CONCURRENCY`, default 16).
- **Plan fan-out module:** `plan_bounded_parallel`, `plan_fanout_parallel`, `plan_schedule`, and `step_materialize` consolidate relation scoped query, for_each reads, and prefer-mixed HTTP batches.
- **CI guard:** `check_graph_cache_concurrency.sh` enforces graph-cache lock routing during parallel rehydrate.

### Changed

- **plasm_plan_run decomposition:** split `compute_eval` into focused modules; extracted `integration_tests.rs` from `mod.rs`; removed 1.9k-line monolithic `compute_eval.rs`.
- **Prefer-mixed relations:** preserve parent row order via per-row entity slots; merge HTTP fan-out stats/fingerprints without replacing embed accumulators.
- **Fold semantics:** `read_cap` truncates entities only after merging all job stats and request fingerprints.

### Fixed

- **bounded_parallel_map deadlock** when batch size exceeded concurrency (permits acquired before futures polled).
- **Parallel comp layer** now passes cloned `ExecutionScope` into step materialize (progress + cancellation).
- **Relation hydrate tests** use `fixtures/schemas/plasm_language_matrix` instead of production `apis/pokeapi`.

## [0.3.32] - 2026-06-18

### Added

- **Typed trace comp boundary:** `TraceCompWire` on `PlasmPlanRunResult`, trace segments, and run-explorer op state; shared `fixtures/trace/minimal_comp.json` contract fixture.
- **Trace comp module:** `plasm-trace/src/trace_comp.rs` with validated wire type and `trace_comp_arc` serde adapter for `Arc<TraceCompWire>` segments.

### Changed

- **Comp-only cutover:** removed legacy plan DAG JSON builders and the Arc-backed segment wrapper; single dry-run comp mint per MCP execute (started + completed share one `Arc`).
- **HTTP/MCP JSON edges:** `to_json_value()` only at wire boundaries; removed the untyped dry-run comp JSON helper.
- **Phoenix trace UI:** comp-only `TracePlanDag.normalize/1`; LiveView and JS hook resolve topology from `comp.bind.topo` only.
- **CI guard:** extended `check_no_legacy_plan_ir.sh` for typed comp fields and banned legacy helpers.

### Fixed

- **HTTP execute trace:** strict comp validation on complete emit (no silent fallback).
- **Plan UX reflection:** edges derived from typed `PlasmBindGraph` instead of untyped JSON.

## [0.3.31] - 2026-06-18

### Added

- **Run Explorer cross-origin progress:** inject `window.__PLASM_API_ORIGIN__` from `PLASM_MCP_PUBLIC_BASE_URL` on MCP resource read; `resolveProgressApiOrigin()` in run-explorer-ui.
- **Public progress routes:** `/v1/run/ui/progress/*` mounted outside incoming-auth (capability-scoped by `logical_session_ref`).
- **Progress smoke:** `scripts/smoke/mcp-ui-progress-live.sh`.

### Fixed

- **Cursor in-chat Run Explorer:** HTTP progress polls hosted API origin instead of opaque iframe `window.location.origin` (fixes perpetual "Waiting for live run to register…" during live `plasm_run`).
- **Run Explorer status footer:** no longer duplicates elapsed telemetry header text.

### Changed

- **Run Explorer bundle:** regenerated `run_ui.html` with progress API origin client.
- **docs/mcp-client-conformance.md:** document cross-origin progress model.

## [0.3.30] - 2026-06-18

### Added

- **Trace execution phases:** MCP `plasm_run` emits `code_plan_execute` with `execution_phase=started` before live await, `completed` on success, and `failed` on error (same `plan_id` links the lifecycle).
- **Resource read attribution:** `McpResourceRead.read_source` (`run_explorer_ui` vs agent); separate KPI `mcp_resource_read_ui_chars`; URI query `plasm.read_source=` stripped before resolution.
- **Trace contract:** `plasm-trace/src/contract.rs` canonical phase/read-source strings and shared segment counter helpers.
- **Code-plan trace input:** `CodePlanTraceInput` + `emit_code_plan_trace` replaces duplicated 12-arg wrappers; split `CodePlanEvaluateTrace` / `CodePlanExecuteTrace`.
- **MCP resource-read trace module:** `resource_read_trace.rs` with `error`/`success` builders.

### Fixed

- **Run Explorer progress:** elapsed timer no longer cleared by watchdog arm; HTTP 404 waiting state; prefer HTTP `artifactPath` before tagged MCP reads.
- **Plan DAG labels:** remove horizontal glyph stretching on short node titles (clip-path instead of `textLength` spacing).

### Changed

- **Run Explorer / plan UI bundles:** regenerated embedded MCP app assets.
- **Phoenix traces UI:** artifact-only hint, execution-phase titles, Run Explorer read badge; logic extracted to `plasm_ui_core` Wire/TracePlanDag/TraceTimeline/TraceChartlets.

## [0.3.29] - 2026-06-18

### Fixed

- **plasm-web Docker build:** copy `apps/mcp-ui-shared` into the `web-build` stage so `web/assets` `npm ci` resolves the `@plasm/mcp-ui-shared` file dependency.

## [0.3.28] - 2026-06-18

### Added

- **Run Explorer HTTP progress:** `GET /v1/run/ui/progress/{logical_session_ref}` and `/stream` for Cursor in-chat live op strip (same-origin SSE/poll fallback).
- **`OpUiTelemetry`:** canonical progress wire type shared by HTTP JSON, SSE events, and MCP notify stats; golden fixture under `fixtures/run_explorer/`.
- **`resolve_running_operation`:** host-level logical-ref → running op resolution (live session or persisted descriptor).
- **Shared SSE helpers:** `operation_progress_sse` for execute wire stream, Run Explorer JSON stream, and cross-replica poll.

### Fixed

- **MCP server-await:** poll until terminal plan result (no bare `` `l_*_oN` = `` gibberish in tool responses).
- **Relation materialize:** normalize parent-get target rows in live compute/materialize paths.
- **Run Explorer artifacts:** lossy preview coverage when step previews omit rows present in snapshots.
- **Live SSE stats:** `OpProgressEvent` carries `OpNotifyStats`; JSON SSE no longer drops `calls`/`elapsed_ms` after the first tick.

### Changed

- **Run Explorer UI bundles:** regenerated embedded MCP app assets (`run_ui`, `run_shell`).
- **MCP UI host:** `attachPlasmOpProgressBridge` extracted; token-only `plasm_run` tool-input classification.

## [0.3.27] - 2026-06-18

### Fixed

- **Trace correlation:** MCP `code_plan_execute.plasm_call_index` aligns with hub `plasm_line.call_index` (restores nested CALLS in Program DAG UI).
- **Trace list NET=0:** head `totals_json` with code-plan KPIs but missing line rollups triggers segment recompute; field-wise merge preserves code-plan totals.
- **HTTP trace detail:** live runs emit populated `http_calls` on `plasm_line` segments via `LiveRunTelemetry`.

### Changed

- **Canonical trace emit:** single `emit_plasm_line_trace` for hub and durable paths.
- **Plan naming:** dry-run plans use `trace_record_plasm_invocation` `call_index` for `plasm_dag_call_{n}` labels.
- **Trace totals merge:** shared `plasm_trace::merge_trace_totals`; simplified stale-head predicate (line rollups required).

## [0.3.26] - 2026-06-18

### Added

- **Trace observability:** HTTP execute emits `code_plan_evaluate` / `code_plan_execute` when an MCP logical session binding exists; debug log when skipped.
- **Trace sink list:** recompute totals from hot `trace_segments` when head `totals_json` is empty (list/detail parity with projection detail reads).
- **Trace segments:** optional `dag` alongside `comp` on `code_plan_*` segment payloads.

### Changed

- **MCP trace archive:** unified evaluate/execute archive+emit dispatch; HTTP emit shares one dry-run context builder.
- **Trace totals:** `trace_totals_from_head_or_records` prefers segment recompute only when head snapshot JSON is absent.

## [0.3.25] - 2026-06-18

### Removed

- **Bounded sync live run:** delete `sync_live_run`, `begin_sync_live_run`, and the 90s bounded-sync path — all MCP `plasm_run` and HTTP `wait=true` live executes use async spawn + terminal await only.

### Added

- **`LivePlanRunPool`:** injected on [`PlasmOssHostState`](plasm-oss/crates/plasm-agent-core/src/server_state.rs) (`live_plan_pool`); configurable stack (`PLASM_LIVE_RUN_STACK_BYTES`, default **4 MiB release** / **16 MiB debug**).
- **Plan commit dry cache:** `PlanCommitDryCache` on `pcN` records — `plasm_run` rehydrates dry eval without re-simulating when bundle ≡ committed artifact.
- **MCP committed run ingress:** `ExecuteCommittedMcpRun` + `McpExecuteWire` + `CommittedRunArtifacts` replace `CommittedPlasmRunContext` god-bag.
- **Terminal watch notify:** pod-local `await_operation_terminal` uses `watch` channels instead of 200ms polling.
- **Unified run delivery:** `deliver_http_live_run` + `LiveRunAwaitContext` builders consolidate MCP and HTTP ingress; dry evaluation passed once into spawn.
- **`terminal_plan_run`:** shared `resolve_terminal_plan_run` for MCP server-await and HTTP `wait(oN)` (decouples `mcp_run_await` from `http_execute`).
- **Smoke:** `scripts/smoke/mcp-pokeapi-pc1-bounded.sh` — pokeapi `pc1` + `e1.limit(3)` must complete within deadline (regression for async-await hang).
- **`PlanLineExecuteShared`:** hoists per-plan execute line setup (session tokens, spill, cancel) for live `run_plasm_comp`.
- **`alloc-bench` feature:** optional dhat heap regression test for matrix `e1.limit(3)` plan setup.

### Changed

- **`spawn_async_plan_run`:** routes live work through `PlasmHostState::live_plan_pool()`; accepts optional precomputed dry review.
- **Run delivery refactor:** `RunDeliveryDecision`, `OperationWire`, `LiveRunSpawn` / `spawn_live_plan_run`, `HttpLiveRunRequest`, `LiveRunError` (HTTP timeout → 504).
- **Resolved-plan HTTP live:** `session.rs` routes live runs through `LivePlanRunPool` (fixes tokio-stack bypass).
- **CI guard:** `check_live_run_await_invariants.sh` replaces `check_sync_live_run_invariants.sh`.
- **`run_plasm_comp` allocations:** `Arc<ExecutionResult>` on publish; single `MaterializedRowSource` row store; finalize moves `return_steps` without deep clone; detach `node_results` during live execution.
- **Live surface steps:** match owned `ValidatedPlanNode::Surface` without clone before budget apply.

## [0.3.24] - 2026-06-17

### Fixed

- **Bounded sync live run:** unify HTTP and MCP synchronous `plasm_run` / execute paths via `run_bounded_sync_live_run` — shared 90s deadline, single cooperative `CancelSignal`, and RAII `SyncLiveRunGuard` (fixes inverted deadline and cancel split from v0.3.23).
- **MCP sync progress:** emit terminal `Done`/`Failed` op notifications without registering fake async `OperationState` entries; progress uses `queue_mcp_notify` only.

### Added

- **`sync_live_run` module:** `ExecutionScope::for_sync_live`, shared MCP progress emit helper, CI guard `check_sync_live_run_invariants.sh`, invariant-indexed tests.

### Changed

- **HTTP execute:** bounded sync runs now use the same deadline + cancel policy as MCP.

## [0.3.23] - 2026-06-17

### Added

- **Run Explorer live progress:** sync and async MCP `plasm_run` emit enriched `notifications/plasm/op` with outbound HTTP call count, last latency, elapsed time, and row materialization; sync runs use an ephemeral internal operation + 1s progress ticker.
- **Run Explorer UI:** telemetry header and status footer update during in-flight runs from `hostcontextchanged` (plus local elapsed ticker when the host is silent).
- **Plan Review UI:** prominent `plan_commit_ref` (`pcN`) chip in the header (click-to-copy), review banner badge, and status line (`pcN · N nodes`).

### Changed

- **`plasm-runtime`:** session-scoped `LiveRunTelemetry` task-local hook on outbound HTTP completion.

## [0.3.22] - 2026-06-17

### Fixed

- **Cursor in-chat Plan Review MCP App:** accept plan payloads from `structuredContent.plasm` even when the host forwards a display `toolName` (not wire `plasm`); show an initial waiting hint instead of a blank canvas on connect.
- **Cursor in-chat Run Explorer:** same payload-first routing for run-shaped `structuredContent` / `_meta.plasm.steps`.
- **MCP dry-run wire:** include `logical_session_ref` and `program` in `_meta.plasm` / `structuredContent.plasm` for session strip hydration.
- **MCP UI smoke:** `mcp-ui-live.sh` executes `plasm_run` with `plan_commit_ref` from the preceding dry-run (v0.3.18 contract).

## [0.3.21] - 2026-06-17

### Fixed

- **MCP `plasm_run` cross-pod `pcN`:** hot in-memory execute rows now merge durable `plan_commits` (and async operation metadata) from Redis on every `get_execute_session` hit, so `plasm` on pod A and `plasm_run` on pod B no longer fail with `unknown plan_commit_ref pc0`.
- **Browser MCP App shells:** appliance shells connect to the ingress Streamable HTTP path (`/plasm/mcp` when `PLASM_MCP_PUBLIC_BASE_URL` is set) instead of hardcoded `/mcp` (404 on `platform.plasm.tools`).

## [0.3.20] - 2026-06-17

### Fixed

- **Hosted MCP App UI:** nginx ingress now routes `/v1/plan/ui`, `/v1/run/ui`, and `/v1/workflows/ui` to the plasm-mcp HTTP listener so bundled shell assets (`/v1/.../shell.js`, `app.js`) load on `platform.plasm.tools` (previously HTML-only via `/plasm/http/v1/...` with 404 asset paths).

## [0.3.19] - 2026-06-17

### Fixed

- **MCP `plasm_run` / `pcN` durability:** fail loud when plan commits cannot be durably persisted; rollback in-memory `pcN` on persist failure; upsert execute descriptors with reuse-key fallback.
- **MCP committed run path:** extract `execute_committed_plasm_run`, wrap phases in real tracing spans, bounded sync deadline, and HTTP semaphore acquire timeout (`RateLimited` on queue stall).
- **MCP App bridge:** forward tool errors to plan/run iframes even when the host call throws.

## [0.3.18] - 2026-06-17

### Changed

- **MCP `plasm_run` contract:** split plan and execute fully: `plasm` accepts `program` and returns a reviewed `plan_commit_ref` (`pcN`); `plasm_run` now accepts only `logical_session_ref` + `plan_commit_ref` and executes the stored reviewed plan.
- **MCP prompt/docs discipline:** initialize, tool descriptions, Tool Explorer notes, and doc-site references now teach token-only `plasm_run`; explicit `wait(oN)` / `cancel(oN)` continuations are documented as HTTP / remote CLI only.
- **MCP UI clients:** Plan UI, Run Explorer, appliance shell, and shared MCP UI host fixtures now use committed-plan run arguments and reject the old `program` / `wait` / `force` shape.

### Fixed

- **Plan commit durability:** HTTP dry-run persists `pcN` records, including compiled plan artifact and source program, so reviewed plans survive execute-session rehydrate.
- **Run Explorer selection:** artifact prefetch no longer steals active selection, and step rail re-selection keeps its handler after inline and artifact result renders.
- **TypeScript package checks:** MCP UI packages declare Node test types explicitly and pass strict package-local typechecks.

## [0.3.17] - 2026-06-16

### Added

- **`plan_prepare` pipeline:** lift comp → validated plan → `apply_read_budgets` → unified `ReadBoundedness` + review; shared by dry-run gates and live validated-plan cache.
- **Live execute budget overlay:** phased runner applies prepared surface `pushed_read_budget` / `page_size` before HTTP (same budgets dry-run uses for policy).
- **Run Explorer:** dry-run review warnings under program panel; 630s watchdog aligned with MCP server-await.
- **Regression tests + CI guard:** `dry_live_boundedness_isomorphism`, `mcp_deliver_query_limit_not_expensive`; `scripts/guards/check_dry_live_boundedness.sh`.

### Changed

- **MCP `plasm` dry-run:** `plan_commit_ref` (`pcN`) visible in Markdown body on review verdict (not only `_meta`).
- **MCP sync `plasm_run`:** reuses prior dry-run evaluation (no second full dry pass on the hot path).
- **`PlanDryReview::execution_is_expensive`:** delegates to shared `read_execution_is_expensive` helper.

### Fixed

- **False expensive-run await:** `query.limit(N)` programs no longer trigger MCP 600s server-await when limit pushdown bounds the read.
- **False dry-run warnings:** unused-seed detection walks plan IR (nested entity refs); unbounded-read cost flags use post-budget prepared plan.

## [0.3.16] - 2026-06-16

### Added

- **Shared exposure replay:** `exposure_replay` canonicalizes federate + cross-pod rehydrate teaching waves; persisted descriptors store per-entity `entity_catalog_entry_ids`.
- **Run delivery policy:** `RunDeliveryPolicy` separates MCP await-terminal vs HTTP async surfaces; `deliver_mcp_expensive_live_run` + `mcp_run_await` for server-side terminal await.
- **Plan commit store:** `register_plan_commit_and_persist` writes `pcN` into Redis execute descriptors; typed `PlanCommitVerifyError` variants.
- **Terminal hydrate formatting:** cross-pod wait hydrates run artifacts through the shared TSV publish path (`terminal_result_format`).
- **Tests:** plan-commit registry roundtrip, bounded MCP deliver gate, artifact meta on non-truncated publish, federated rehydrate pairing rejection.

### Changed

- **MCP `plasm_run` (default `wait: true`):** expensive plans spawn internally and return one terminal TSV/table response (no `+` accept + client poll loop); initialize async-poll appendix removed.
- **HTTP execute:** async spawn gate routed through `RunDeliveryPolicy::HttpExecute` (behavior unchanged).
- **Symbol map:** single `resolve_session_symbol_map` boundary; continuation parse passes cross-request cache for federated `e#` after async finalize.

### Fixed

- **Federated compile after async finalize:** `e2` (and cross-catalog symbols) resolve when posting continuations with symbol-map cross-cache.
- **Cross-pod terminal wait:** hydrates stored run artifacts as TSV, not hard-coded JSON fences.
- **Rehydrate pairing:** partial `entity_catalog_entry_ids` vs `entities` length mismatch fails loud (`EntityCatalogPairingMismatch`) instead of silent primary-catalog padding.
- **Publish meta:** artifact handles always appear in `_meta.plasm.steps` when a step carries a run snapshot (hydrate no longer patches meta ad hoc).

## [0.3.15] - 2026-06-16

### Added

- **MCP integration tests:** federated multi-catalog `apply_capability_seeds` + distinct `e#` dry-run compile; async `wait(l_<token>_oN)` poll through terminal result.

### Changed

- **MCP initialize paging copy:** paging continuations explicitly require **`plasm_run`** (live resume only).

### Fixed

- **`plasm` dry-run continuations:** reject `wait(...)` / `cancel(...)` on plan-only **`plasm`**; poll/cancel remain on **`plasm_run`** only (`plasm_dry_run_continuation_error` in `operation.rs`).

## [0.3.14] - 2026-06-16

### Changed

- **MCP grammar compression:** canonical initialize grammar ratcheted from 6106 to 4310 bytes; initialize/grammar budget guards tightened accordingly.
- **MCP server layout:** split `mcp_server.rs` into focused submodules (`prompt`, `discover`, `trace`, `transport`, `schema`, `tool_parse`, `tests`) while keeping handler impl in `mod.rs`.
- **Grammar opener copy:** initialize contract now points agents at `plasm_context` TSV for symbols instead of implying teaching rows ship in initialize.

### Fixed

- **Search inline projection:** surface `[fields]` on search rows now validates against capability `provides` (rejects filter inputs like `q`); matrix tests updated after `team_key` joined search `provides`.

## [0.3.13] - 2026-06-16

### Changed

- **MCP grammar placement:** MCP initialize now teaches the canonical Plasm grammar once; `plasm_context` open/federate/expand waves return table-only teaching TSV rows and do not repeat the grammar preamble.
- **MCP prompt budget:** canonical grammar is ratcheted to 6106 bytes; initialize prompt budget now guards grammar and non-grammar workflow copy separately.
- **Prompt copy ownership:** removed duplicate MCP prompt fragments so tool descriptions point to initialize instead of restating grammar.

### Fixed

- **Linear search rows:** `Issue` / matrix search rows expose `team_key`, enabling search results to be grouped or filtered by team key.
- **Seeded abstract entities:** explicitly seeded abstract entities receive stable `e#` symbols and executable teaching rows in MCP contexts.

## [0.3.12] - 2026-06-13

### Added

- **View-backed preflight v2:** shared sync/async view DAG orchestrator (`ViewNodeRunner` / `ViewNodeRunnerAsync`); inner Query/Get CML compile gates on preflight nodes; schema-derived stub rows for downstream binds.
- **`validate_cgs_views`:** static view DAG validation at plugin pack (`plasm-pack-plugins`) alongside CML template validation.
- **`ViewRunProof`** and **`view_test_support`** fixture re-exports for deterministic view conformance tests.
- **Docs:** informal MCP client conformance notes and Plasm evidence bundle reference (doc-site).

### Changed

- **MCP UI (plan / run explorer):** terminal error surfaces for `isError` tool results and stripped `_meta`; stale plan watchdog (120s); clearer incomplete host-forward copy; embedded bundles refreshed.
- **`ViewAmbientContext`:** explicit threading at dispatch boundaries (`ExecuteOptions::view_ambient`, `ExecuteSession::view_ambient`); no task-local lookup for view scope injection.
- **Dry ≡ live compile gate:** view capabilities no longer skip CML preflight in `compile_preflight`.
- **Execute session operations:** operation wire/persist helpers extracted to `execute_session_operations.rs`.
- **MCP teaching copy:** async poll discipline notes in initialize / `plasm_run` tool descriptions.

### Removed

- **`view_typestate.rs`** (superseded by shared DAG orchestrator in `view_dag_run.rs`).

## [0.3.11] - 2026-06-15

### Added

- **Cross-pod async operation persistence:** thin operation descriptors (phase, coalesced progress, terminal `run_artifact_id`) persist in the Redis execute session descriptor; any replica can `wait(h)` / resolve terminal results via shared run artifacts. Running ops on another pod return **`operation_not_on_replica`**; foreign **`cancel(h)`** is rejected with the same code.
- **`OperationError`** structured codes: `unknown_operation_handle`, `operation_not_on_replica`, `operation_result_unavailable`.
- **Tests:** cross-pod integration in `server_state`, HTTP cap + cross-pod wait smokes in `long_operation_http`; multireplica smoke extends async accept → fresh-transport `wait(h)`.

### Changed

- **Rehydrate:** restore persisted operation stubs (no live executor); **`PLASM_MAX_RUNNING_OPS_PER_SESSION`** counts pod-local live executors only.
- **Docs:** cross-pod long-op behavior in `plasm-long-operations.md`, `mcp-logical-sessions.md`, `env-profiles.md` (+ doc-site mirrors).

## [0.3.10] - 2026-06-15

### Changed

- **Parallel async operations:** remove execute-session single-flight gate; multiple `plasm_run` async programs may run concurrently on one session (distinct `l_<token>_oN` handles). Cap via `PLASM_MAX_RUNNING_OPS_PER_SESSION` (default **16**); `too_many_operations` lists outstanding handles with wait/cancel hints.
- **Sync live runs:** `begin_sync_live_run` is sync-only re-entrancy — async ops no longer block bounded sync runs or each other (until cap).
- **Agent teaching:** MCP initialize long-ops lifecycle copy, `plasm_run` tool async-accept note, teaching TSV frontmatter open-handle rule, async accept response poll nudge, `tool_model` long_operations string.

## [0.3.9] - 2026-06-13

### Fixed

- **Distributed execute sessions (hosted tenants):** catalog rotation checks now compare registry-base `catalog_cgs_hash_hex` pins captured before tenant OAuth hosted-KV / resolved `http_backend` / schema-overlay materialization — fixes immediate `Execute session unavailable` after `plasm_context` on GitHub, Linear, and Proof while pokeapi (no tenant patch) continued to work.
- **Cross-pod rehydrate:** replay tenant outbound hosted-KV, binding-resolved backend, and overlay via shared `execute_session_materialize` helper; persist `registry_catalog_hashes_by_entry` and `outbound_hosted_kv_by_entry` on Redis descriptors.
- **Session reuse:** `try_reuse_session` paths validate through `get_execute_session` so reused rows are not returned when registry rotation would discard them.

## [0.3.8] - 2026-06-13

### Fixed

- **Run Explorer async accept:** `isPlanShapedPlasmMeta` no longer misclassifies live `plasm_run` payloads carrying `comp` + `plan_ux_reflection` as dry-run plan review.

### Changed

- **Run Explorer MCP App UX:** flatten instrument-panel layout for ~390px iframe — edge-to-edge results table, hide step rail and plan DAG on single-step runs, human summary header (`{label} · N rows`), copyable session ref chip; remove redundant Technical details disclosure and eternal DAG loading placeholder.
- **Run Explorer layout module:** extract `run-layout.ts` for DAG visibility, chrome sync, and header summary policy.

## [0.3.7] - 2026-06-13

### Fixed

- **Multi-replica execute sessions after plugin catalog reload:** purge Redis execute descriptors and logical bindings when plugin-dir catalog reload detects hash changes; discard stale bindings on `CatalogHashMismatch` during cross-pod rehydrate; validate in-memory sessions against live catalog pins before serve.
- **MCP session errors:** distinguish catalog rotation from generic expiry in `plasm` / `plasm_run` messages; stale logical bindings recover via existing `plasm_context` path.

## [0.3.6] - 2026-06-13

### Fixed

- **Run Explorer op state / CI build:** complete `operation.rs` + `run_explorer_meta.rs` wiring omitted from v0.3.5 (`comp`, `plan_ux_reflection`, `step_order` on async live runs); regenerate embedded Run Explorer MCP App bundle.

## [0.3.5] - 2026-06-13

### Changed

- **`http_execute` modularization (Phase B):** rename `prelude.rs` → `deps.rs`; extract wire helpers to `wire.rs`; split `routes/handlers.rs` into focused modules (`stream`, `create`, `session_get`, `artifacts`, `plan_run_response`, `run_post`); split `context/session.rs` into `open`, `federate`, `expand`, `seeds_apply`.

### Added

- **Graph execute fork/commit:** `GraphExecuteBranch` COW snapshot execute with stale-epoch retry (`MAX_STALE_EPOCH_RETRIES = 3`); unified rehydrate API; Shuttle concurrency tests; CI guard `check_graph_cache_concurrency.sh`.


### Fixed

- **MCP rollout survival:** `RedisSessionStore::delete` drops local cache only; explicit HTTP DELETE uses `delete_persistent` so Redis transport metadata survives SSE pod loss during rollouts. SDK graceful shutdown extended to 30s.
- **MCP transport persistence:** `persist_transport_state` after `plasm` / `plasm_run` and binding hydration so per-transport slot maps mirror to Redis before pod termination.

## [0.3.3] - 2026-06-13

### Fixed

- **`wait()` / `cancel()` / `page()` handle parse:** continuation operands accept `-` in base64url logical session tokens (`wait(l_<token>_oN)`), not only `[A-Za-z0-9_]` ident prefixes.

## [0.3.2] - 2026-06-13

### Fixed

- **Run Explorer MCP App:** async `plasm_run` accept responses (`continuity` + compact `op`, no `steps`) render a pending op panel instead of "Host did not forward run data"; tool errors show MCP `content` text. Server attaches Run Explorer UI for operation-pending meta; embedded bundle regenerated.

## [0.3.1] - 2026-06-13

### Fixed

- **Multi-replica MCP logical sessions:** `LogicalSessionRegistry` mirrors minted sessions to Redis so `plasm` / `plasm_run` on a different pod than `plasm_context` pass `verify_tenant` (fixes ~50% cross-pod smoke failures on hosted multi-replica).

## [0.3.0] - 2026-06-13

### Changed

- **Breaking — stateless MCP logical sessions:** `plasm_context` returns **`logical_session_ref`** as `l_<token>` (22 URL-safe base64 chars over the canonical UUID bytes). Legacy transport slots (`s0`, …), raw UUID text, and `plasm://session/s0/…` URIs are **rejected**. MCP paging/operation handles are namespaced (`l_<token>_pgN`, `l_<token>_oN`); HTTP execute uses plain `pgN` / `oN` on the same execute row.
- **Multi-replica execute:** Redis-backed execute descriptors now persist binding maps, federated catalog hashes, plugin generation pins, and plan-commit records so any `plasm-mcp` pod can rehydrate material session state without transport stickiness.
- **`resources/read`:** `plasm://session/l_<token>/r|p/{n}` resolves without `MCP-Session-Id` when the logical binding exists in Redis.
- **Docs / smoke:** multireplica execute smoke (`scripts/smoke/mcp-multireplica-execute-live.sh`), MCP client conformance notes, teaching-table BNF uses `l_<token>_…` handle shapes.

### Migration

- Replace stored `s0` / `sN` refs with the `l_<token>` from each `plasm_context` response.
- Scale `plasm-mcp` beyond one replica only with **`PLASM_MCP_TRANSPORT_REDIS_URL`** (`transportRedis.enabled` in Helm) and ingress MCP stickiness **`none`**.

## [0.2.6] - 2026-06-13

### Fixed

- **MCP App OAuth stickiness:** ingress default `upstream-hash-by` is **`$http_mcp_session_id` only** (composite `$http_mcp_session_id$http_authorization` broke pod affinity on every OAuth access-token refresh → `-32016` Session not found and Cursor “Not connected” MCP Apps while tools still worked).
- **MCP App AppBridge:** plan/run iframe hosts retry AppBridge connect (3×5s) and expose `window.__plasmUiConnectionState` for host-debug status.

### Changed

- **Smoke:** `mcp-stream-common.sh`, `mcp-ingress-check.sh`, `mcp-sticky-session-live.sh`; DEMO.md OAuth troubleshooting + API-key workaround.

## [0.2.5] - 2026-06-12

### Fixed

- **HTTP outbound JSON UTF-8:** serialize JSON bodies as explicit UTF-8 bytes with `Content-Type: application/json; charset=utf-8` (reqwest `.json()` dropped charset and could cause mojibake on downstream APIs such as Proof).
- **MCP App CSP:** `resources/read` UI bundles allow `connectDomains` for `platform.plasm.tools` and `*.plasm.tools` (hosted artifact fetch in Run Explorer standalone mode).

### Changed

- **E2E:** `render_unicode_markdown_survives_live` Hermit regression for `Pokémon` / `→` in render output; wire-level unit test on compiled JSON requests.

## [0.2.4] - 2026-06-12

### Fixed

- **Typed relation rows:** relation materialization decodes embed payloads as target CGS entities and GET-hydrates when declared fields are missing; evicts graph-cache stubs before hydrate so plan compute sees full rows (fixes dry-green / live-red on `Plan.render` columns such as `capture_rate` on `specimen.species`).
- **Plan/run isomorphism:** dry-run `dry_validate_render_nodes` preflights `render_compute` against the target entity field set (same keys as live execution).

### Changed

- **PokeAPI catalog:** `Pokemon.species` declares `from_parent_get` on path `[species]`.
- **HTTP outbound JSON:** set `Content-Type: application/json; charset=utf-8` on JSON request bodies.
- **Docs:** DEMO.md states relation hops yield typed target-entity rows (removes sparse-embed workaround).

## [0.2.3] - 2026-06-13

### Changed

- **MCP UI PlasmComp cutover (zero legacy):** single `finalize_mcp_tool_result` MCP exit attaches `_meta.ui` from `dry_run + comp` / live `steps`; UI attach removed from `http_execute`. Dropped `plan` wire aliases on resolved-plan and code-plan archive types.

## [0.2.2] - 2026-06-12

### Fixed

- **MCP UI attach gates:** plan-review and run-explorer `_meta.ui.resourceUri` attach on `comp` / `dry_run` / `steps` wire (v0.2.0 dropped legacy `_meta.plasm.plan`; hosts no longer skip iframe render).
- **Relation fanout (FromParentGet):** materialize embedded relation refs from session graph cache before wire-path flatten (fixes 0-row returns on bindings like `types = pikachu.types`).

## [0.2.1] - 2026-06-12

### Fixed

- **Live relation returns:** run artifacts store typed `parsed_preimage`; `run_sealed` no longer re-parses synthetic `plan.relation(...)` display lines (fixes `plasm_run` failure on relation fanout returns).
- **GET by id_field filter:** `Entity(name == "value")` in GET parens lowers to Get instead of mangling the path id as `= "value"`.

### Changed

- **Run artifact schema v2:** `parsed_preimage` (required on new writes) + `display_lines` (human lineage only; `expressions` alias on read). No re-parse fallback for digest recovery.

## [0.2.0] - 2026-06-12

### Added

- **Monadic execution contract (`PlasmComp`):** programs compile to typed `PlasmComp` with `PlasmStepPayload` steps; plan runner executes via `run_plasm_comp` / `ExecutablePlasmComp`.
- **Language matrix witness:** `monadic_comp_witness` tag in `plasm_language_matrix` e2e; `compile_plasm_program` is the public compile entry.
- **CI guards:** `check_plasm_comp_single_topo`, `check_plasm_comp_strict_steps`, `check_no_legacy_plan_ir`, `check_evidence_no_trace_chain`.
- **Hash-chained evidence bundles (opt-in):** `PLASM_EVIDENCE_CHAIN=1` emits tamper-evident sidecars (`GET …/artifacts/{run_id}/evidence`, `plasm evidence verify`). Segment hashing uses RFC 8785 JCS schema v2 (breaks preview v1 chains). Optional Ed25519 signing via `PLASM_EVIDENCE_SIGNING_KEY` and rotation window via `PLASM_EVIDENCE_TRUSTED_PUBLIC_KEYS`.
- **`plasm-evidence` crate:** JCS canonical segments, chain verify modes, optional Ed25519 (`signatures` feature).
- **Evidence run seal at emit:** `run_sealed` segments recompute digest from artifact preimage; wire `run_id` must match content hash.
- **Evidence sidecar retention:** object-store GC skips `execute/…/evidence/` paths; head-dedup storage layout documented.

### Changed

- **Breaking — MCP/HTTP wire:** `_meta.plasm.comp` replaces `plan_dag`; consumers must read `comp` (TypeScript: `requirePlasmComp()`).
- **`plan_commit_id`:** hashes semantic comp subset via `comp_canonical` / `plasm_comp_commit_canonical`.
- **Evidence HTTP serve:** verifies full chain + optional signature trust; cross-checks `run_sealed` when the run artifact is co-located in the store.
- **Evidence module layout:** `evidence_chain/` submodules (`session`, `plan`, `error`, …); `ExecuteSession` uses lazy `Option<Arc<EvidenceChainSession>>` slot (no nested `OnceLock`).
- **`EvidenceEmitError`:** typed `Chain` / `Canonical` / `ChainLockPoisoned` variants replace stringly `Verify`.
- **`run_artifacts/` decomposition:** types, backends (memory/fs/object store), keys, GC, URI helpers; `mod.rs` slimmed (~390 lines).
- **`ChainBuilder`:** removed misleading `finish_trusted()` alias; use `finish()`.

## [0.1.107] - 2026-06-11

### Added

- **`entity_to_agent_row_json`:** agent-facing rows (preview_entities, TSV) omit cache metadata (`_ref`, `_version`, `_completeness`, `_last_updated`).
- **`column_schema` on `_meta.plasm.steps`:** CGS-informed column types for Run Explorer typed cells.
- **`@plasm/mcp-ui-shared`:** shared ref chips + typed cell renderer for Run Explorer and Plan Review.
- **Portrait-first MCP UI:** stacked Run Explorer (horizontal step tabs) and Plan Review (canvas → inspector).

### Changed

- **TS UI consolidation:** extract `@plasm/mcp-ui-host` (host protocol) and `@plasm/plan-dag` (shared Program DAG layout); Phoenix trace hook and Plan Review share one renderer.
- **Run Explorer:** dense table layout, relation arrays as chips, sticky header.
- **Plan Review:** inspector as structured definition table with shared chips.
- **MCP TSV:** includes relation columns; no `_ref` fallback column.

## [0.1.106] - 2026-06-11

### Added

- **MCP App view-only contract:** Plan Review and Run Explorer never call `callServerTool` — they render from the first host-forwarded tool result only.
- **`structuredContent.plasm` mirror:** `plasm` / `plasm_run` responses duplicate `_meta.plasm` for hosts that strip `_meta` on iframe forward.
- **Run content fallback:** parse Plasm markdown `## label (N rows)` + fenced TSV from forwarded `content` when meta channels are absent.
- **Cursor forward fixtures + Vitest guards:** assert zero re-execution; structuredContent and TSV-only forwards render.

### Fixed

- **Removed v0.1.105 self-hydrate:** deleted `hydratePlanFromServer` / `hydrateRunFromServer` (wrong architecture — UI is a view, not an execution surface).
- **Actionable host-forward errors:** “Host did not forward plan/run data” instead of misleading “Plan request failed” when the agent call succeeded.

### Removed

- **Self-hydrate tests** that mocked successful `callServerTool` re-invocation.

## [0.1.105] - 2026-06-11

### Added

- **Cursor MCP UI self-hydrate:** Plan/Run iframes call `app.callServerTool` when host `toolresult` omits `_meta.plasm` (Cursor strips meta on forward).
- **Argument-based tool routing:** infer plan vs run from `toolinput.arguments` (ext-apps omits `toolName`); ignore cross-app results.
- **Run watchdog:** 120s stale skeleton teardown with interrupted/in-progress copy.
- **Dev aid:** `window.__plasmUiLastEvent` for Cursor host debugging.

### Fixed

- **Plan session strip:** `logical_session_ref` from toolinput arguments before toolresult arrives.
- **Plan app:** no longer shows "Plan meta incomplete" for `plasm_run`-shaped results.

## [0.1.104] - 2026-06-11

### Added

- **MCP UI testing pyramid:** golden `CallToolResult` fixtures, plan/run host lifecycle Vitest suites, AppBridge bridge tests, `scripts/smoke/mcp-ui-live.sh` post-release gate.
- **CI gates:** `ensure-plan-ui-bundle.sh` runs `plan-ui` vitest + `mcp-appliance-shell` bridge tests before asset drift check.

### Fixed

- **Plan Review stuck loading:** defer canvas spinner until confirmed plan meta on `toolresult`; normalize via `normalizePlasmMeta` (incl. `plan_dag`); error surfaces on incomplete meta.
- **Run Explorer cancel/error:** `toolcancelled` and `isError` toolresult clear skeleton with user-visible copy.
- **E2E contract:** bounded `plasm_run` asserts `preview_entities`; truncated runs assert artifact/`dict_ref` handles.

## [0.1.103] - 2026-06-11

### Added

- **MCP UI live fix:** informational read-only Plan Review + Run Explorer surfaces driven by host `toolinput` / `toolresult` (no operator form in iframe).
- **Run Explorer node-result tables:** `_meta.plasm.steps` emits every return node with `preview_entities` (cap 100) or artifact handles; tabular UI per step.
- **Shared MCP tokens:** `apps/mcp-ui-tokens.css` (OKLCH neutrals, violet accent).
- **Appliance shell reference:** forwards `plasm` / `plasm_run` tool lifecycle + `notifications/plasm/op` via AppBridge.

### Fixed

- **Inline MCP bundle corruption:** function replacer in `inline-bundle.mjs` (no `$&` expansion); CI sanity guards for nested `<!DOCTYPE html>`.
- **Run Explorer event wiring:** `addEventListener("toolresult")` instead of deprecated `.on()`.

## [0.1.102] - 2026-06-11

### Added

- **Run Explorer MCP App:** `ui://plasm/run-explorer`, `GET /v1/run/ui`, live `plasm_run` / `run_workflow` artifact step rail (`apps/run-explorer-ui/`).
- **Plan Review MCP App:** `ui://plasm/plan-review`, `GET /v1/plan/ui`, dry-run plan DAG + `plan_ux_reflection` (`apps/plan-ui/`).
- **Workflow MCP App:** `ui://plasm/workflow`, `open_workflow` / `dry_workflow` / `run_workflow`, federated workflow manifest registry.
- **Embedded UI bundles:** Vite build → `include_str!` assets under `plasm-agent-core/src/assets/` with compile-time dev-ref guards (`build.rs`).
- **CI bundle gates:** `ensure-plan/run/workflow-ui-bundle.sh` + `ensure-workflow-ts-codegen.sh` in `circle-validate.sh`.
- **E2E:** `cargo test -p plasm-e2e --test workflow_apps_e2e` (HTTP routes, MCP resources, live `_meta.ui` on truncated runs).

### Fixed

- **MCP inline bundle:** `$&` replacement corruption in `inline-bundle.mjs`; CSS-before-JS ordering via `buildMcpInlineHtml`.
- **Catalog YAML:** `apis/github/domain.yaml` block-scalar typo (`description: >-.`).

## [0.1.101] - 2026-06-09

### Changed

- **Catalog description hygiene:** teaching-table-facing `description` prose across `apis/*/domain.yaml` — drop identity restatement, eval-key examples, field inventories, generic get boilerplate, and tabular jargon; CI lint `scripts/check_catalog_description_hygiene.py` (warn tier) via monorepo `scripts/guards/check_catalog_description_hygiene.sh`.

## [0.1.100] - 2026-06-08

### Added

- **Limit pushdown:** `.limit` / filter+limit / sort+limit read budgets on surface nodes (`plan_read_bounds`, `stream_consume`); streaming top-k and row-match collection via `PageCollector` (`paginated_collect`, `top_k`, `row_predicate`).
- **E2E:** `cargo test -p plasm-e2e --test limit_pushdown_e2e` (Hermit pokeapi_mini).

### Changed

- **Selective auto-async:** only **expensive** review plans (unbounded paginated reads, relation fanout, mutating for_each) auto-async on default `wait`; advisory review alone stays sync.
- **Progress:** `=` unchanged polls include step/rows; explicit `rows_progress` param (removed task-local); `sync_rows_materialized` fixes double row counting during async pagination.
- **`effective_host_page_size()`** merges explicit `.page_size` with pushed read budgets.

### Fixed

- **Fail-closed predicate lowering** for unsupported plan predicate values (pushdown and in-plan filter eval share `row_predicate`).
- **Top-k min-heap ordering** for descending sort keys.
- **E2E stack guards:** `long_operation_e2e` and `operation_progress_push_e2e` run on 16MB stack threads (debug overflow).

## [0.1.99] - 2026-06-08

### Changed

- **Release-only bump:** no OSS crate changes since 0.1.98; monorepo tag picks up portal install-manifest CI fix for semver bakes.

## [0.1.98] - 2026-06-07

### Added

- **Session graph spill + plan rehydrate:** when `PLASM_GRAPH_CACHE_URL` is set, paginated fetch-all spills durable graph page deltas (v2 JSON), trims in-process hot cache (`PLASM_GRAPH_HOT_MAX_ENTITIES`, default **2048**), and plan compute rehydrates via `MaterializedRowSource` (streaming `limit` / full `aggregate`).
- **OTLP metrics + spans:** `plasm.runtime.graph.page_spill.*`, `plasm.runtime.graph.hot_cache.evictions_total`, `plasm.execute.graph.*` (delta append, rehydrate, snapshot finalize).
- **E2E:** `cargo test -p plasm-e2e --test graph_spill_e2e` (Hermit pokeapi_mini + `file://` persistence).

### Changed

- **Unified session delta seq:** graph page deltas and run artifact appends share `SessionCore::alloc_delta_seq` (no collision with run artifact seqs).

### Operations

- **No object-store GC** for graph deltas/snapshots yet — use bucket lifecycle rules or manual cleanup. Run artifacts retain existing `PLASM_RUN_ARTIFACTS_*` retention.

## [0.1.97] - 2026-06-07

### Added

- **Operation progress (poll + push):** token-minimal wire lines (`+` accept · `~` running · `=` unchanged · `!`/`x`/`?` terminal) and short-key `_meta.plasm.op` on poll responses.
- **HTTP SSE:** `GET /execute/{prompt_hash}/{session}/operations/{handle}/stream` — plain wire line in `data` (`snapshot` / `progress` / `terminal` events).
- **MCP push:** `notifications/plasm/op` with `{ line, n }` (optional `c` on accept).
- **Tests:** coalesce integration (`ExecutionScope` → broadcast + MCP hub); push E2E (HTTP SSE + MCP notification).

### Changed

- **`wait`/`cancel` poll responses:** compact op lines instead of verbose poll copy; unchanged polls emit `=` without repeating instructions.
- **Tool-model `execute.long_operations`:** documents compact op sigs and optional SSE/MCP push surfaces.
- **MCP workflow tail:** compact op sigs for long-running runs.
- **E2E harness:** shared-server respawn when background task exits; stricter running poll assertions.

## [0.1.96] - 2026-06-07

### Added

- **Long-running plan execute:** `wait(sN_oM)` and `cancel(sN_oM)` continuations (same model as `page(sN_pgM)`); cooperative cancel and progress polling for in-flight async plans.
- **`plan_commit_ref` (`pcN`) review gate:** dry-run mints content-addressed tokens over the **semantic plan DAG** only (`version`, `nodes`, `edges`, `topological_order`, `returns`); volatile plan `name` / `summary` excluded from commit id.
- **HTTP execute:** `?mode=plan`, `?wait=false`, `?force=true`, `?plan_commit_ref=pcN`; synthetic logical session `s0` for operation handles without MCP `plasm_context`.
- **MCP `plasm` / `plasm_run`:** `wait`, `force`, and `plan_commit_ref` tool args; teaching TSV preamble for wait/cancel handles.
- **CLI:** `plasm run --mode plan`, `--wait`, `--force`, `--plan-commit-ref`.
- **Tool-model HTTP:** additive `execute` block on `GET /v1/registry/:entry_id/tool-model` (continuations + review gate notes).
- **E2E:** dual-surface `long_operation_e2e` (HTTP + MCP); HTTP smokes in `long_operation_http`.
- **Doc-site:** [plasm-long-operations.md](doc-site/docs/reference/plasm-long-operations.md).

### Changed

- **pokeapi eval:** rewrite `apis/pokeapi/eval/cases.yaml` for long-op / review-gate coverage.
- **MCP prompt char budget:** trim `workflow_tail.txt` and `program_contract.txt` for wait/cancel workflow copy.
- **Teaching TSV / snapshots:** GitHub prompt baseline ~31k; projection witness invariant tests.

### Fixed

- **`plasm-eval`:** `Wait` / `Cancel` expression coverage in eval harness.
- **Language matrix live runs:** run on 16MB stack thread to avoid debug stack overflow.

## [0.1.95] - 2026-06-07

### Changed

- **MCP `plasm_context` response trim:** agent-facing tool text is `` `{sN}` `` + teaching TSV (or continuity one-liners) only — entity/char accounting moves to logs/traces; `_meta.plasm` keeps `logical_session_ref`, `continuity`, `domain_revision`, and optional `relations`.
- **HTTP backend provenance:** split `CatalogHttpBackend` (catalog YAML placeholders) from `ReplHttpOverride` (REPL/`--backend`); REPL binding synthesis no longer mis-gates on Fibery account placeholder detection.

### Fixed

- **Views `relation_outputs`:** materialize relation rows from cached entity refs when live fanout is unavailable (fixes `views_digest_relation_outputs` live run).
- **REPL bindings:** `repl_session_binding_map` resolves from entry connect requirements only, not catalog placeholder heuristics.
- **Tests:** serialize `PLASM_WORKSPACE` in terminal mirror/state tests; update language-matrix view MCP expectations for slim TSV responses.

## [0.1.94] - 2026-06-07

### Fixed

- **Parser:** zero-arity `Entity(scope).delete()` on compound-key entities (Fibery `Record`) preserves the receiver [`Ref`] instead of defaulting id to `"0"` (fixes `entity.error/schema-type-not-found` for `"0" database`).
- **Parser:** dotted-call delete with arguments emits [`DeleteExpr`](plasm-oss/crates/plasm-core/src/expr.rs) (runs `execute_delete`) instead of misrouting through [`InvokeExpr`](plasm-oss/crates/plasm-core/src/expr.rs).
- **Parser:** zero-arity update/action invoke on a `Get` receiver preserves compound/simple refs when the CML template has no path vars.

## [0.1.93] - 2026-06-07

### Fixed

- **Fibery create/update/delete:** `execute_create` and invoke responses use capability-aware decoders with overlay `database` ambient; create decode errors propagate instead of returning empty rows.
- **Fibery `entity_update`:** runtime merges `id_from` wire key (`fibery/id`) into JSON `input` patch objects before compile.
- **Fibery `entity_delete`:** surface Fibery `{ success: false }` command envelope failures on delete.
- **Fibery `view_query`:** JSON-RPC response decode uses `result` array directly (remove erroneous `result.views` path segment).

### Added

- **Fibery eval:** `apis/fibery/eval/cases.yaml` (fb-01…fb-16) with correct `search_text` coverage token.
- **Fixtures/tests:** `sample_view_query.json`; runtime tests for update id merge, delete compile, view list decode.

## [0.1.92] - 2026-06-07

### Fixed

- **Fibery `user_get_me`:** add empty `params: {}` to command args; trim `q/select` to `fibery/id`, `user/name`, `user/email`.
- **Runtime:** preflight Fibery `{ success, result }` envelopes before single-entity narrowing — surface `success: false` command errors and empty `result: []` (`$my-id` unmatched) instead of opaque “missing path segment `0`”.

### Added

- **`scripts/fibery-curl-triad.sh`:** A/B/C curl probes for schema, `fibery/user` list, and `$my-id` get-me.

## [0.1.91] - 2026-06-07

### Fixed

- **Fibery catalog:** correct CML response narrowing for the `{ success, result }` command envelope — `user_get_me` / `entity_get` use `items_path: [result, "0"]`; entity and schema-batch creates use `items: result` + `single: true` (fixes singleton GET and create/update decode without OpenAPI validation).
- **Fibery docs:** response-envelope table, discover vs overlay, curl smoke tests, eval index; add Fibery to `apis/README.md` catalog table.

## [0.1.90] - 2026-06-07

### Fixed

- **Fibery catalog:** add `id_from` wire paths (`fibery/name`, `fibery/id`) so `schema_query` and entity reads decode live API rows (fixes MCP `plasm_run` “No valid ID field” after Connect).

### Changed

- **MCP `discover_capabilities`:** initialize workflow and tool copy default to fenced TSV; `typed: true` is opt-in for structured disambiguation only.

## [0.1.89] - 2026-06-07

### Fixed

- **Hosted `plasm-mcp-app` (`plasm-saas`):** mount `/internal/mcp-bindings/v1/put-scoped` (Phoenix Connect binding sync 404 on v0.1.88).
- **Phoenix Connect:** stage personal MCP `disabled` before host-binding allowlist upsert, then `put-scoped`, then re-enable (agent 409 on active upsert without workspace URL binding).

## [0.1.88] - 2026-06-07

### Fixed

- **Hosted `plasm-mcp-app` (monorepo `plasm-saas`):** mount `/internal/mcp-bindings/v1/put-scoped` and `delete-scoped` — Phoenix Connect binding sync no longer returns 404.
- **Phoenix (monorepo):** readiness gates filter agent binding gaps to projected enabled graphs so personal revoke/remove is not blocked by catalogs being dropped.

## [0.1.87] - 2026-06-07

### Added

- **Per-MCP catalog binding KV:** scoped encrypted envelopes for host-owned connect fields (Fibery workspace URL as `catalog_http_origin`); sqlx pointer table `project_mcp_entry_bindings`; `POST /internal/mcp-bindings/v1/put-scoped` and `GET …/readiness-gaps`; `GET /v1/registry/:entry_id/connect-requirements`.
- **Fibery:** catalog uses scoped binding + outbound API key; no deploy-time `FIBERY_HTTP_BACKEND` env.

### Changed

- **Execute / MCP:** Fibery on an MCP auth map requires a scoped binding envelope; legacy outbound `http_backend` alone is not sufficient — operators must re-connect Fibery after upgrade (see monorepo `docs/fibery-binding-reconnect-runbook.md`).
- **Active MCP upsert:** agent rejects activation when enabled graphs have secret or binding readiness gaps.

## [0.1.86] - 2026-06-06

### Added

- **Row contract:** search/query teaching table separates capability filter `inputs` from `provides` row fields; projection, `group_by`, sort, dedupe, and row filters reject filter-only params with capability-scoped errors.
- **Fibery hosted MCP:** tenant connect via outbound API key plus scoped workspace URL binding (`catalog_http_origin`); catalog `hosted_kv` for connect UX. (Legacy outbound KV `http_backend` is not used without a scoped binding row.)
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
- **Teaching QA:** teaching rows validate on the opaque agent parse path (`parse_with_cgs_layers` + session [`SymbolMap`]); nav receiver checks use the same path when symbols are enabled.

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
