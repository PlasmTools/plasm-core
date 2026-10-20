# Build and test (contributors)

!!! tip "Read this when…"

    You are changing Rust crates, running CI-parity checks, or cutting release binaries — not just validating one catalog with `plasm-cgs`.

**Prerequisites:** a checkout with `apis/` populated. Catalog-only work can stay on **`cargo run`** (see [Start here](../getting-started.md)).

**Authoritative deep dive:** [`build/bazel/README.md`](https://github.com/PlasmTools/plasm-core/blob/main/build/bazel/README.md) in the repository (Bzlmod graph, crate-universe repin, Linux Docker wrapper, nextest profiles). This page is the operator summary; do not duplicate patch-level detail here.

---

## Choose your path

| Goal | Command |
|------|---------|
| Validate / REPL one catalog | `cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/<name>`; `cargo run -p plasm-repl -- --schema apis/<name> --backend <url>` |
| Format + Clippy (declared graph) | `bash build/bazel/quality.sh --jobs=4` |
| Full Rust test suites (nextest, CI profile) | `bash build/bazel/bazel.sh test //:nextest --test_arg=--profile=ci` |
| Release archives / native binaries | `bash build/bazel/export.sh //:release_archives /path/to/artifacts --config=release` |
| Linux parity from macOS | `bash build/bazel/linux.sh test //:nextest --test_arg=--profile=ci` |

Always invoke Bazel through **`bash build/bazel/bazel.sh`** (or **`build/bazel/linux.sh`** inside Docker). The launcher pins the Bazel executable and configures a shared disk cache.

---

## Environment

| Variable | Role |
|----------|------|
| `PLASM_BAZEL_CACHE` | Override the shared action cache directory (default: `~/Library/Caches/plasm-bazel` on macOS, `${XDG_CACHE_HOME:-$HOME/.cache}/plasm-bazel` on Linux) |
| `CARGO_BAZEL_REPIN=workspace` | Regenerate crate-universe metadata after intentional `Cargo.toml` / workspace changes (see README in `build/bazel/`) |

Each checkout keeps its own Bazel output base; the disk cache is shared across checkouts and CI when paths align.

---

## Relationship to Cargo

**Cargo remains the fast path** for authoring loops (`plasm-cgs`, `plasm-repl`, `plasm-pack-catalogs`, local `cargo run -p plasm-server`). **Bazel is the pinned graph** for reproducible builds, release packaging, and the main CI test partitions (`scripts/ci/test-python-pool.sh`, `oss-release-pack-native.sh`, Monty export).

`Cargo.lock` stays the application dependency lock; `Cargo.Bazel.lock` is generated for crate-universe. Do not treat ad-hoc `cargo test --workspace` as a substitute for the declared Bazel nextest targets when you are validating a compiler or runtime change.

---

## Related

- [CLI & environment index](cli-and-env.md)
- [AGENTS.md](https://github.com/PlasmTools/plasm-core/blob/main/AGENTS.md) — agent validation commands and cloud VM notes
- [Changelog](https://github.com/PlasmTools/plasm-core/blob/main/CHANGELOG.md) — release-level build changes
