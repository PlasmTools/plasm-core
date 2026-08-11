# Agent Instructions

This repository contains the OSS Plasm compiler/runtime workspace and curated API catalogs under `apis/`.

## API Authoring

`plasm-core` owns CGS / CML / catalog authoring doctrine. When creating or editing an API catalog, follow the local skill suite under `skills/`:

- [skills/plasm-authoring/SKILL.md](skills/plasm-authoring/SKILL.md) — primary workflow (read spec → model → map → validate → e2e test → eval).
- [skills/plasm-authoring/reference.md](skills/plasm-authoring/reference.md) — deep CGS / CML reference.
- [skills/plasm-catalog-e2e-test/SKILL.md](skills/plasm-catalog-e2e-test/SKILL.md) — Hermit, live, and sandbox transport testing.
- [skills/plasm-catalog-polish/SKILL.md](skills/plasm-catalog-polish/SKILL.md) — autonomous diagnostic / fix loop.
- [skills/plasm-catalog-score/SKILL.md](skills/plasm-catalog-score/SKILL.md) — rubric scorecard.
- [skills/plasm-catalog-reprint/SKILL.md](skills/plasm-catalog-reprint/SKILL.md) — full-cutover regeneration of weak catalogs.
- [skills/plasm-catalog-retro/SKILL.md](skills/plasm-catalog-retro/SKILL.md) — post-authoring retrospective.
- [skills/plasm-forge/SKILL.md](skills/plasm-forge/SKILL.md) — entry redirect to the skill suite above.
- [.cursor/agents/plasm-forge.md](.cursor/agents/plasm-forge.md) — Cursor agent (**Plasm forge**) that drives the loop autonomously.

API authoring is semi-autonomous. Agents may read specs, design entities, edit YAML, run validation, test against mocks and sandboxes, and add eval cases, but `domain.yaml` is a semantic CGS model, not a deterministic OpenAPI dump.

Default loop:

```text
read spec/docs -> design graph -> author domain.yaml -> author mappings.yaml -> validate -> e2e test (Hermit, then live/sandbox) -> eval coverage -> iterate
```

Do not add scripts or generator crates that mechanically emit canonical `domain.yaml` or `mappings.yaml` from a spec.

## Validation Commands

Use these commands as appropriate:

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/<api>
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec path/to/openapi.json apis/<api>
cargo run -p plasm-repl --features baml -- --schema apis/<api> --backend http://localhost:1080 --help
cargo run -p plasm-eval --features baml -- coverage --schema apis/<api> --cases apis/<api>/eval/cases.yaml
```

Use Hermit for mock-backed transport checks when an OpenAPI spec is available, then live or vendor sandbox testing per the e2e-test skill:

```bash
hermit --specs path/to/openapi.json --port 9090 --use-examples
cargo run -p plasm-repl --features baml -- --schema apis/<api> --backend http://localhost:9090
# In-session: expressions from teaching table; optional :output table
```

## Core Boundaries

Prefer catalog edits over core runtime changes when the gap is authoring or mapping.

If current CGS / CML / runtime semantics cannot express a real catalog need, **do** change core —
but state the missing expressiveness and the minimal semantic extension first (short note in the
PR / commit body). Do not invent one-off catalog escapes that paper over a language hole.

Keep secrets out of schema files. Catalog auth reads from environment variables or supported runtime secret providers.

## Cursor Cloud specific instructions

Durable, non-obvious notes for working in this OSS subtree on a Cursor Cloud VM. The startup update script runs `cargo fetch` and regenerates `baml_client`; the VM snapshot already carries the Rust toolchain, system packages, `baml-cli`, and a warm `target/` cache.

- **Toolchain**: requires Rust stable **≥ 1.85** (a transitive `alloy-eip7702` dep needs `edition2024`). The base image ships 1.83, which is too old; `rustup default stable` is set in the snapshot. There is no `rust-toolchain.toml` pin.
- **System deps** (already installed in snapshot, not in the update script): `libssl-dev` + `pkg-config` (`openssl-sys`, pulled by `auth-framework`'s native-tls path) and `protobuf-compiler` (`baml` build script needs `protoc`).
- **Generated `baml_client`**: `crates/plasm-eval/baml_client`, `crates/plasm-semantic-seed/baml_client`, and `crates/plasm-discovery-eval/baml_client` are **gitignored** and produced by `baml-cli generate` (v0.220.0) from the repo root. Default **library** builds of `plasm-eval` / `plasm-repl` / `plasm-semantic-seed` do not require them (`baml` / `llm-rerank` are opt-in). **`plasm-server` defaults include `semantic-auto-seed`**, so appliance source builds still need `plasm-semantic-seed/baml_client` unless you pass `--no-default-features`. Re-run `baml-cli generate` after editing anything under `baml_src/`.
- **`plasm-trace-sink` (SaaS ops binary)**: durable Iceberg ingest for the execution-trace lane (`PLASM_TRACE_SINK_URL`); not OTEL and not a Cargo dep of the appliance. Often fails to build in this OSS subtree (floating `datafusion_iceberg` vs workspace `datafusion` pin). Build/lint/test with `--exclude plasm-trace-sink`.
- **Schema overlay fixtures** live in this tree at `fixtures/schemas/*_overlay/` (not the parent monorepo). `plasm-core` / `plasm-runtime` lib tests `include_str!` them via `CARGO_MANIFEST_DIR/../../fixtures/schemas/`. `workflow_matrix` still lives only in the private super-repo, so some `plasm-e2e` tests that probe parent paths will skip or fail here. `plasm-agent-core`'s `cross_pod_operations` test overflows the type-layout recursion limit on current rustc. Overlay-free smoke: `cargo test --workspace --exclude plasm-trace-sink --no-fail-fast` (network/live tests self-ignore).
- **Lint**: `scripts/ci/rust-quality.sh` runs `cargo fmt --all -- --check` + `cargo clippy --workspace --all-targets -- -D warnings`. Under this OSS subtree/newer clippy it will trip on the items above; `cargo clippy --workspace --exclude plasm-trace-sink` (lib/bins) is clean apart from one newer style lint in `plasm-runtime/src/view_template.rs`.
- **Running live queries**: `plasm-cgs` (package `plasm-cli`) does schema validation/round-trips; after BAML generation, the live REPL is `cargo run -p plasm-repl --features baml -- --schema apis/<x> --backend <url>` (pipe an expression then `:quit` on stdin for non-interactive use; get-by-id form is `Entity(id)`). Debug builds can **stack-overflow** on large recursive catalogs (e.g. `pokeapi`) because debug stack frames are larger — use a small flat catalog (`xkcd`, backend `https://xkcd.com`) or build `--release` for those.
- **Appliance**: `cargo run -p plasm-server -- --no-tui --schema fixtures/schemas/capability_with_input` boots headless, **auto-starts an embedded Postgres** (`pg-embed`), and serves HTTP+MCP on `127.0.0.1:3000` (`/v1/health`, `/v1/registry`, `/execute`). Pass a split catalog directory (`domain.yaml` + `mappings.yaml`) or a packaged plugin dir.
