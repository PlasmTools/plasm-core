# Bazel foundation

`MODULE.bazel` owns core's dependency graph. SaaS consumes the core module rather
than redefining its targets. `Cargo.Bazel.lock` is crate-universe's generated
resolution; `Cargo.lock` remains the application dependency lock.

## Nested workspace correction

`rules_rust_nested_workspace.patch` applies to rules_rust 0.74.0:

1. `cargo-bazel` adds a synthetic package to resolve transitive proc macros. Cargo
   already includes a root package in its workspace. Adding `.` explicitly to
   `workspace.members` causes nested Monty path dependencies to inherit Plasm's
   package metadata instead of Monty's, failing at `rust-version`. Remove that
   redundant membership insertion, retaining the package and its dependencies.
2. Source-bootstrap the generator so the corrected code is actually executed.
   Patching Rust source while downloading an unmodified prebuilt generator would
   not fix the failure.
3. Keep generated path-crate BUILD files in Bazel's repository output. The
   `local_crate_mirror` repository already renders its own BUILD file; generation
   must not write BUILD files into the pinned Monty source checkout.

Bzlmod overrides apply only in the root module, and patch labels must also be
root-local. The SaaS root applies the same patch through a symlink to this file.
Do not fork a second patch copy or modify Monty's package inheritance to mask the
generator defect.

The failure was minimized by adding a synthetic root package to a temporary copy
of Plasm's Cargo workspace. `cargo tree --offline --workspace` succeeds without an
explicit `.` member and fails with one; expanding `exclude` does not fix it.

## Validation

`rules_rust_relocatable_lock.patch` stores path dependencies relative to the core
Cargo.lock. Rendering expands them for the current checkout; Bzlmod mirrors use
the consuming module's location. Absolute paths and parent traversal are rejected
when reading a portable lock. Both root modules must apply this patch too.

A relocated standalone checkout reused 109 of 109 Rust compilation actions from
the shared disk cache. The test was run without repinning. This establishes the
initial trace-wire target's portability, not the complete product graph's parity.

```sh
# Standalone core; performs dependency resolution and compiles the public crate.
bash build/bazel/bazel.sh test //crates/plasm-trace-wire:all
# In the SaaS root; consumes core's lockfile without repinning.
bash plasm-oss/build/bazel/bazel.sh test //:trace_wire_tests
```

`wire_roundtrip_tests` exercises serde serialization/deserialization across the
public Rust library boundary. The library unit-test harness currently has zero
unit cases; it is a compilation check, not runtime coverage by itself.

Regenerate crate-universe metadata in core after intentional manifest/config
changes:

```sh
CARGO_BAZEL_REPIN=workspace bash build/bazel/bazel.sh test //crates/plasm-trace-wire:all
```

`skip_cargo_lockfile_overwrite` prevents this operation from rewriting Cargo.lock.
Compare generated crate versions with Cargo.lock before accepting the output.
Verify regeneration has not written additional files into Monty's source tree.
The migration deliberately changes its typeshed build script to sort ZIP entries
and use fixed timestamps. The same correction is applied to registry `ty_vendored`
through `ty_vendored_deterministic.patch`; both root modules expose that patch.
A relocated optimized release with a fresh output base reused all 1,544 Rustc
actions from the shared disk cache after this correction.

## Nextest execution

`bash build/bazel/quality.sh --jobs=4` checks formatting and Clippy for the declared
Rust targets, including test harnesses. From SaaS, use
`bash plasm-oss/build/bazel/quality.sh --jobs=4`; this also checks the external core
targets. These checks use Bazel's pinned toolchain and retain `-D warnings`.

`bash build/bazel/bazel.sh test //:nextest --test_arg=--profile=ci` builds the registered test executables
with Bazel, then runs them with nextest 0.9.132. Native runner archives are pinned
by SHA-256 for macOS and Linux x86_64/aarch64. Cargo and rustc come from Bazel's
Rust toolchain; Cargo is used only for offline package metadata. The metadata
launcher uses the pinned Bazel Python 3.12.7 toolchain.

The launcher validates each registered test against its Cargo target and passes
explicit binary metadata to nextest. Tests execute in their source package
directory, and live-service results are not cached by Bazel. The CI profile writes
`nextest/ci/junit.xml` in each Bazel test's undeclared outputs and does not retry
failed tests. A direct `bazel run` invocation writes `target/nextest/ci/junit.xml`.
Nextest filters and
partition options can be passed with repeated `--test_arg` flags. Use `bazel test`
to hold the workspace output lock through staging and execution. Service-sensitive
targets run locally with test-result caching disabled.

Each run snapshots its test executables and declared runtime tools into a private
temporary directory. Later builds may replace Bazel outputs without changing the
executables in an already-running suite. Test fixture files still come from the
checkout; do not edit fixtures during a run intended as completion evidence.

The initial registered suite passed 3,340 cases across core, CML, compiler,
runtime, agent-core, evidence, catalog library/packer and wire integration tests.
Expanded runtime and host integration targets are being validated. The launcher
supplies the declared Bazel-built Monty worker through
`PLASM_MONTY_BINARY`. Registration of the remaining integration targets is part
of the migration; this is not yet the complete CI test set.

`--config=local` enables the existing `local_env_bootstrap_secrets` feature for
local hosted development. Default builds retain production bootstrap behavior.

## Linux execution

For native development and CI, use `bash build/bazel/bazel.sh` from core or
`bash plasm-oss/build/bazel/bazel.sh` from SaaS. The shared launcher verifies the
pinned Bazel executable and uses an external action cache by default:
`~/Library/Caches/plasm-bazel` on macOS, or
`${XDG_CACHE_HOME:-$HOME/.cache}/plasm-bazel` on Linux. `PLASM_BAZEL_CACHE` overrides
that directory. Each checkout retains its own Bazel output base. Native and
container Linux builds have separate caches; different target configurations
have different action keys.

Export native archives with
`bash build/bazel/export.sh //:release_archives /path/to/artifacts --config=release`.
The same exporter handles `//:npm_packages`; pass
`--define=plasm_release_version=X.Y.Z` to stamp the published npm metadata.
Exporting prepares local artifacts and does not publish them.

From either repository, invoke core's `build/bazel/linux.sh` with `build`, `test`
or `run` and the ordinary Bazel arguments. It runs native Linux in the existing
digest-pinned Rust image, with checksummed Bazel 8.4.2 and CMake 3.31.8. Docker is
the execution environment; no image build or Buildx command is used.

The Docker volumes `plasm-bazel-linux-tools-v1` and `plasm-bazel-linux-cache-v2-<uid>`
retain tools and action cache entries across invocations. Output bases are keyed
by the host checkout path, so separate worktrees do not share mutable outputs.
Linux cache entries are separate from the macOS disk cache. Bazel runs as the
host UID/GID. Native ARM hosted compilation and fresh-checkout wire-target cache
reuse pass; this does not yet establish complete release parity.
The amd64-emulated Rust compiler segfaulted on the ARM workstation, matching the
existing Dockerfile warning. Release amd64 binaries require native execution plus
a declared cross toolchain; that target-platform gate remains open.

## Catalog inputs

For the released catalog set, run `bash scripts/ci/prepare-catalog-inputs.sh`
before building archives or catalog images. It downloads the versioned
`catalog-embedding-inputs-v0.5.5.tgz` release asset, verifies its pinned SHA-256,
and materializes the declared JSON inputs. The archive is cached outside the
checkout under `~/.cache/plasm-build-inputs` (override with
`PLASM_BUILD_INPUT_CACHE`). This uses the GitHub CLI; access to a draft release
requires repository authentication. No embedding API is called.

`plasm-pack-catalogs` now consumes existing embedding cache entries by default.
Missing or corrupt vectors fail without network access or repairs to the input
cache. `--prepare-embeddings` explicitly acquires missing vectors and exits before
catalog packing; that command requires credentials and may incur charges. Run
preparation separately, then pass the resulting cache directory as a declared
input to packaging. No embedding acquisition is part of validation here.
