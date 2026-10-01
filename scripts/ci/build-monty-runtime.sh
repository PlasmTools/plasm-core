#!/usr/bin/env bash
# Build the worker from the same vendored source used by plasm-agent-core.
set -euo pipefail
oss_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
install_root="${1:?usage: build-monty-runtime.sh ABSOLUTE_INSTALL_ROOT [TARGET]}"
case "$install_root" in /*) ;; *) echo 'Monty install root must be absolute' >&2; exit 2;; esac
monty_root="${oss_root}/vendor/monty"
[[ -f "${monty_root}/crates/monty-runtime/Cargo.toml" ]] || {
  echo 'Missing vendor/monty; run git submodule update --init --recursive' >&2
  exit 2
}
target_args=()
if [[ -n "${2:-}" ]]; then target_args=(--target "$2"); fi
cargo install --path "${monty_root}/crates/monty-runtime" \
  --bin monty --locked --force --root "$install_root" \
  --target-dir "${install_root}/build" "${target_args[@]}"
