#!/usr/bin/env bash
# Check the declared Rust graph with the same pinned toolchain used to build it.
set -euo pipefail
launcher="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/bazel.sh"
workspace="$(pwd -P)"
while [[ ! -f "$workspace/MODULE.bazel" ]]; do
  [[ "$workspace" != / ]] || { echo 'Run within a Bazel module.' >&2; exit 2; }
  workspace="$(dirname "$workspace")"
done
targets=('//crates/...')
if [[ -f "$workspace/plasm-oss/MODULE.bazel" ]]; then
  targets+=('@plasm_core//crates/...')
fi
bash "$launcher" build "${targets[@]}" \
  --aspects=@rules_rust//rust:defs.bzl%rustfmt_aspect \
  --output_groups=rustfmt_checks "$@"
bash "$launcher" build "${targets[@]}" \
  --aspects=@rules_rust//rust:defs.bzl%rust_clippy_aspect \
  --output_groups=clippy_checks \
  --@rules_rust//rust/settings:clippy_flags=-Dwarnings "$@"
