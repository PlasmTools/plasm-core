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
cargo run -p plasm-cli --bin plasm -- schema validate apis/<api>
cargo run -p plasm-cli --bin plasm -- validate --schema apis/<api> --spec path/to/openapi.json
cargo run -p plasm-repl -- --schema apis/<api> --backend http://localhost:1080 --help
cargo run -p plasm-eval -- coverage --schema apis/<api> --cases apis/<api>/eval/cases.yaml
```

Use Hermit for mock-backed transport checks when an OpenAPI spec is available, then live or vendor sandbox testing per the e2e-test skill:

```bash
hermit --specs path/to/openapi.json --port 9090 --use-examples
cargo run -p plasm-repl -- --schema apis/<api> --backend http://localhost:9090
# In-session: expressions from teaching table; optional :output table
```

## Core Boundaries

Prefer catalog edits over core runtime changes. If an API cannot be represented with current CGS / CML / runtime semantics, stop and describe the missing expressiveness before modifying core crates.

Keep secrets out of schema files. Catalog auth reads from environment variables or supported runtime secret providers.

## Cursor Cloud specific instructions

Durable, non-obvious notes for working in this OSS subtree on a Cursor Cloud VM. The startup update script runs `cargo fetch` and regenerates `baml_client`; the VM snapshot already carries the Rust toolchain, system packages, `baml-cli`, and a warm `target/` cache.

- **Toolchain**: requires Rust stable **≥ 1.85** (a transitive `alloy-eip7702` dep needs `edition2024`). The base image ships 1.83, which is too old; `rustup default stable` is set in the snapshot. There is no `rust-toolchain.toml` pin.
- **System deps** (already installed in snapshot, not in the update script): `libssl-dev` + `pkg-config` (`openssl-sys`, pulled by `auth-framework`'s native-tls path) and `protobuf-compiler` (`baml` build script needs `protoc`).
- **Generated `baml_client`**: `crates/plasm-eval/baml_client` (and `crates/plasm-extract/baml_client`) are **gitignored** and produced by `baml-cli generate` (v0.220.0) from the repo root. `plasm-eval` and `plasm-repl` will not compile without it. Re-run `baml-cli generate` after editing anything under `baml_src/`. `cargo fmt`/clippy diffs in `baml_client` are expected (the generator output is not fmt-clean); the pre-commit hook formats it in `fix` mode.
- **`plasm-trace-sink` does not build here**: the floating git dep `datafusion_iceberg` (locked at `JanKaul/iceberg-rust`) now requires `datafusion` 54 while the workspace pins `datafusion` 53, yielding an `IcebergCatalog: CatalogProvider` trait mismatch. It is an optional Iceberg trace sink; no core or appliance crate depends on it. Build/lint/test with `--exclude plasm-trace-sink`.
- **Some test targets can't compile in this subtree** (they assume the deeper parent-monorepo layout / unexported fixtures): `plasm-core` & `plasm-runtime` lib tests and several `plasm-discovery`/`plasm-e2e` tests `include_str!`/read `../../../fixtures/schemas/*_overlay/`, `pokeapi_mini`, `workflow_matrix` paths that resolve outside `/workspace`; `plasm-agent-core`'s `cross_pod_operations` test overflows the type-layout recursion limit on current rustc. Run the rest with `cargo test --workspace --exclude plasm-trace-sink --exclude plasm-core --exclude plasm-runtime --exclude plasm-agent-core --no-fail-fast` (≈235 pass; network/live tests self-ignore).
- **Lint**: `scripts/ci/rust-quality.sh` runs `cargo fmt --all -- --check` + `cargo clippy --workspace --all-targets -- -D warnings`. Under this OSS subtree/newer clippy it will trip on the items above; `cargo clippy --workspace --exclude plasm-trace-sink` (lib/bins) is clean apart from one newer style lint in `plasm-runtime/src/view_template.rs`.
- **Running live queries**: `plasm-cgs` (package `plasm-cli`) does schema validation/round-trips; the live REPL is `cargo run -p plasm-repl -- --schema apis/<x> --backend <url>` (pipe an expression then `:quit` on stdin for non-interactive use; get-by-id form is `Entity(id)`). Debug builds can **stack-overflow** on large recursive catalogs (e.g. `pokeapi`) because debug stack frames are larger — use a small flat catalog (`xkcd`, backend `https://xkcd.com`) or build `--release` for those.
- **Appliance**: `cargo run -p plasm-server -- --no-tui --schema fixtures/schemas/capability_with_input` boots headless, **auto-starts an embedded Postgres** (`pg-embed`), and serves HTTP+MCP on `127.0.0.1:3000` (`/v1/health`, `/v1/registry`, `/execute`). Pass a split catalog directory (`domain.yaml` + `mappings.yaml`) or a packaged plugin dir.
