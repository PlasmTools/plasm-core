# Vendored Monty

`monty/` is a Git submodule of https://github.com/ryan-s-roberts/monty,
forked from https://github.com/pydantic/monty. Its recorded commit pins the source.
Initialize it with `git submodule update --init --recursive` from either Plasm root.

Plasm's `monty-analysis`, `monty`, `monty-pool` and `monty-types` use local path dependencies. Monty retains
its own Cargo workspace and lockfile; both enclosing workspaces exclude it.
`scripts/ci/build-monty-runtime.sh` builds the worker from this same source, including
local edits, and forces replacement of an existing installation. Rebuild host and
worker together after protocol changes; do not pair a stale packaged worker with
modified pool code. The NAPI build's prepare-runtime step performs the worker build.

Work on Monty inside the submodule. The local development branch is
`codex/plasm-expression-inference`; add an `upstream` remote to pydantic/monty when
cloning afresh. Commit fork changes before recording the resulting submodule commit in plasm-core.
Before publishing parent commits, publish the referenced Monty commits first;
local checkpoint commits may refer to local submodule commits.
The fork's MIT license remains in the submodule; packaged workers retain the
existing `licenses/monty-MIT.txt` notice.

Initial vendored revision: `e007685fbb06494c13b9a7b3fede9f8e6a54a2be`
(upstream main, 2026-09-28; v1.0.0 plus subsequent fixes).

Verified on macOS arm64: both parent and standalone Cargo dependency graphs resolve
the vendored paths; `cargo check -p plasm-agent-core --lib` passes; the local worker
build succeeds; `profile_syntax_families_check_and_execute` passes against the
rebuilt worker and host test executable (including nullable coalescing, chained
calls and operand-selecting short-circuiting). Docker source-copy stages include
the submodule, but Linux image builds have not been rerun for this setup change.


The development fork now contains `monty-analysis` and a pinned read-only
structured-export patch to `ty_python_semantic` under `monty/vendor/`.
All enclosing Cargo roots select that same patch. The new analysis library and
in-process API are documented in `monty/crates/monty-analysis/README.md` and
`monty/docs/design/analysis-library.md`. Salsa and its macro-rules dependency are
constrained to the standalone lockfile's tested 0.28.2 in consuming workspaces.
The vendored checker retains its upstream
MIT license and source revision in `MONTY-PATCH.md`.

Plasm uses monty-analysis for in-process checking and MontyRun::new for compile-only
profile validation. User Python runs only in the execution worker.
