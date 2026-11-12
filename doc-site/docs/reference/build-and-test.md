# Build and test

This guide is for contributors working on the compiler, runtime, or release
artifacts. For catalog authoring, see [Start here](../getting-started.md).

The [Bazel reference](https://github.com/PlasmTools/plasm-core/blob/main/build/bazel/README.md)
is the authoritative guide to the pinned graph, prerequisites, cache, nextest,
Linux wrapper, and release exports. Cargo remains useful for focused iteration.

## Common commands

Run these from the repository root, using the repository's launcher:

```bash
# Check formatting and Clippy on declared targets.
bash build/bazel/quality.sh --jobs=4

# Run registered Rust test executables under the CI nextest profile.
bash build/bazel/bazel.sh test //:nextest --test_arg=--profile=ci

# Exercise a small target when checking the toolchain.
bash build/bazel/bazel.sh test //crates/plasm-trace-wire:all

# Export release archives.
bash build/bazel/export.sh //:release_archives /path/to/artifacts --config=release
```

For Linux builds from macOS, the repository also provides
`build/bazel/linux.sh`. Test filters and partitions use repeated `--test_arg`
options; inspect the summary to ensure the intended tests actually ran.

The graph uses `Cargo.lock` for application dependencies and `Cargo.Bazel.lock`
for crate-universe metadata. After an intentional manifest or configuration change,
follow the [repinning instructions](https://github.com/PlasmTools/plasm-core/blob/main/build/bazel/README.md#validation)
and inspect the resulting changes. Release packaging has additional catalog-input
requirements described in the same reference.

## Language and design references

Start with the [language definition](plasm-language-definition.md) and
[Python compute contract](python-compute-contract.md). The
[coverage ledger](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-e2e/tests/plasm_language_matrix/PYTHON-COVERAGE.md)
records matrix evidence. Maintainer-facing design notes are indexed in the
[repository docs directory](https://github.com/PlasmTools/plasm-core/blob/main/docs/README.md).
