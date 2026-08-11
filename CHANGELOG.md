# Changelog

All notable changes to this OSS workspace are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Entries before **0.4.35** live in [`CHANGELOG-ARCHIVE.md`](CHANGELOG-ARCHIVE.md).

## [Unreleased]

### Changed

- Drop checked-in `@plasm_lang/*` `0.3.78` npm tarballs; ignore `*.tgz` under
  `packages/plasm-engine` and `packages/plasm-agent`. Workspace
  `@plasm_lang/vercel-agent` depends on `file:../plasm-engine` so the lockfile
  no longer pins registry `0.3.112` (published as GPL-3.0-or-later).

### Fixed

- **In-repo overlay fixtures:** schema-overlay JSON/bootstrap trees now live under
  `fixtures/schemas/*_overlay/` so `plasm-core` test compiles (docs CI fenced-example
  gate) no longer `include_str!` files from the private parent monorepo.

## [0.4.45] - 2026-08-10

### Changed

- **Plasm Core licensing:** Plasm-authored code is now available under
  `MIT OR Apache-2.0`, at the recipient's option. `plasm-trace-sink` remains
  separately licensed under BUSL-1.1.
- **Version boundary:** `v0.4.44` is the final BUSL release. Historical BUSL
  releases retain their original per-version conversion rule: the earlier of
  2030-04-24 or four years after that version's first public distribution.
- **Docs contract cutover:** public `doc-site` is the canonical OSS documentation
  source (no private-monorepo `docs/` import). Language/MCP/CLI/appliance prose
  aligns with wire-first ingress (`e#`/`m#`/`r#` + catalog wire names; MCP
  `run_ref`), and CI gates forbid reintroducing removed contracts.
- **Optional BAML:** `plasm-eval`, `plasm-repl`, and `plasm-semantic-seed` gate
  generated `baml_client` behind a `baml` feature. `plasm-server` still defaults
  to `semantic-auto-seed` for the appliance.

## [0.4.44] - 2026-08-08

### Changed

- **Observability crate map:** rename `plasm-observability-contracts` → `plasm-trace-wire`; unify `TraceTotals` on the wire type; document OTEL (`plasm-otel`) vs execution-trace (`plasm-trace` / wire) vs optional SaaS sink (`plasm-trace-sink`).
- **MCP inline run content:** fully inline `plasm` / `plasm_run` results are body-only (no leading `kind` / `artifact_uri` token fence); truncated / `snapshot_only` still emit `artifact_uri`.
- **Repo hygiene:** drop tracked `.fastembed_cache/`; archive changelog entries before 0.4.35; clarify catalog count (42) and agent/core boundaries.

## [0.4.43] - 2026-08-06

### Fixed

- **Multi-brand federation one-shot (bundle fallback):** ship of the v0.4.42 repair — when provider clarify under `brand_lock` cannot collapse via one-catalog-per-alt coverage, repair selects one seed per locked catalog from candidate bundles (alt prefs → intent entity hint → lexical score) instead of `routing_error`.

## [0.4.42] - 2026-08-06

### Fixed

- Same federation bundle-fallback repair as 0.4.43 (tag/pipeline re-ship).

## [0.4.41] - 2026-08-06

### Fixed

- **Multi-brand federation one-shot:** provider-level clarify under a complete named `brand_lock` is deterministically repaired into federation **ready** (one seed per catalog, ≤3) instead of `routing_error`.

## [0.4.40] - 2026-08-06

### Fixed

- **Semantic auto-seed lifecycle:** route before mint/append so clarify/hard_miss no longer orphan sessions or poison accumulated intent.
- **Delta extend:** route on the current-turn intent only and exclude already-exposed `(catalog, entity)` pairs so multi-catalog federation does not re-litigate prior seeds.
- **Clarify continuation:** bound `routing_ref` + `clarify_choice` receipts (`ClarifyBinding`) redeem only under the matching `new`/`extend` session; no forged supporting capability ids.
- **Mode-aware breakouts + brand lock:** extend/new copy no longer contradicts auto-seed policy; provider-level clarify under named brands is rejected via `ClarifyKind`.

### Changed

- Unified `resolve_context_seeds(phase, policy)` and domain `ContextRouteDecision` (presentation split from routing); deleted vestigial post-route delta rewrite.

## [0.4.39] - 2026-08-04

### Fixed

- **Plan Review only for review plans:** fused clean-reads no longer attach plan DAG / Plan Review UI. `plasm` returns rows (Run Explorer); Plan Review paints only when dry-run yields a `run_ref` for review.
- **Plan Review hang on fused reads:** host clears the toolinput watchdog on run-shaped `plasm` results (“Clean read executed inline — see Run Explorer”) instead of waiting forever for a plan.
- **Advisory structural review gates MCP fuse:** unprojected multi-row reads and unnarrowed list roots set `needs_review` even when the default host page caps fetch cost — MCP returns a `run_ref` plan instead of auto-executing. Default host page remains non-expensive (first page stays sync).

## [0.4.38] - 2026-08-04

### Fixed

- **Plan Review hang on fused clean-reads:** `plasm` auto-execute responses now attach plan DAG + `plan_ux_reflection` on `structuredContent.ui` (SEP-1865 View lane) while agent `content` / `_meta.plasm` stay run-shaped (rows + thin continuity only). Oversized plans get thin `plan_uri` / `plan_http_path` fetch refs instead of inlining the DAG into agent context.

## [0.4.37] - 2026-08-04

### Fixed

- **SaaS Trace plans:** durable `mcp_trace_segment` decode no longer silently drops `code_plan_*` rows; instrument deserialize failures; keep Plan Security `flow` seal when slimming oversized durable `plan_ux_reflection`.
- **Plan Security flow graph:** fail closed unless `flow.counts` / `trace` / `violations` are present (no invented sparse seals).

### Changed

- **`AuditEvent.logical_session_id`:** first-class Iceberg `audit_events` column (field id 21). Payload is pure `TraceEvent` JSON; nested `_plasm_audit` is legacy-only and stripped on decode. **Reset the Iceberg warehouse** on deploy (no additive lake migration).
- **Trace → Plan Security deep-link:** archive hydrate loads program + sealed `comp` / `plan_ux_reflection` via query params.

## [0.4.36] - 2026-07-24

### Fixed

- **plasm-web Docker bake:** load Plan Security scenario/preset packs from `web/priv/flow_policies/` (ships with `COPY web/priv`) instead of monorepo `/fixtures` path expand.

## [0.4.35] - 2026-07-23

### Fixed

- **plasm-web Docker bake:** copy `fixtures/flow-policies` to `/fixtures/flow-policies` so compile-time `@external_resource` packs resolve under `/app` WORKDIR.

