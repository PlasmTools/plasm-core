# Building from source

**Prerequisites:** Rust toolchain (stable ≥ 1.85 for direct `cargo` use), repo
clone with `apis/` populated, and system packages noted in [`AGENTS.md`](https://github.com/PlasmTools/plasm-core/blob/main/AGENTS.md) (OpenSSL dev headers, `protoc` when building BAML-enabled crates).

From **0.5.5** onward, release binaries, CI test suites, and npm artifacts are
produced through the **pinned Bazel graph**. Day-to-day catalog work still works
with **`cargo run`** for fast iteration; use Bazel when you need CI parity,
nextest partitioning, or release exports.

---

## Fast path — Cargo (catalog authors)

Typical validate → REPL → appliance loop (also [Start here](../getting-started.md)):

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/dnd5e

cargo run -p plasm-repl -- \
  --schema apis/dnd5e \
  --backend https://www.dnd5eapi.co

cargo run -p plasm-catalog --bin plasm-pack-catalogs -- \
  --workspace . --apis-root apis --output-dir target/plasm-catalogs

cargo run -p plasm-server --release -- --catalog-dir target/plasm-catalogs
```

Optional quality gate (Cargo-native):

```bash
bash scripts/ci/rust-quality.sh
```

Exclude `plasm-trace-sink` when the Iceberg sink crate does not build in your
checkout (`--exclude plasm-trace-sink`).

---

## CI parity — Bazel

Canonical commands, cache layout, nextest registration, Linux container wrapper,
and release export live in **[`build/bazel/README.md`](https://github.com/PlasmTools/plasm-core/blob/main/build/bazel/README.md)** in the repository root. Do not duplicate that file here.

Quick smoke after cloning:

```bash
bash build/bazel/bazel.sh test //crates/plasm-trace-wire:all
bash build/bazel/quality.sh --jobs=4
```

Full registered Rust suites:

```bash
bash build/bazel/bazel.sh test //:nextest --test_arg=--profile=ci
```

The launcher sets `PLASM_MONTY_BINARY` to the Bazel-built Monty worker for matrix
tests. Repin crate-universe metadata only after intentional `Cargo.toml` / config
changes (`CARGO_BAZEL_REPIN=workspace` — see the Bazel README).

---

## Release artifacts

- **Platforms (0.5.5+):** macOS Apple Silicon and Linux x86-64 GNU. Intel macOS
  release archives are discontinued; use ARM macOS or Linux CI exports.
- **Catalog embeddings for clean checkouts:** run
  `bash scripts/ci/prepare-catalog-inputs.sh` before packaging (see Bazel README
  “Catalog inputs”).
- **Export locally:** `bash build/bazel/export.sh //:release_archives /path/to/artifacts --config=release`

Operator install without a full source build: [plasm.tools/get](https://plasm.tools/get/).
