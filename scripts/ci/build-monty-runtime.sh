#!/usr/bin/env bash
# Build the upstream worker at exactly the revision used by plasm-agent-core.
set -euo pipefail
oss_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
install_root="${1:?usage: build-monty-runtime.sh ABSOLUTE_INSTALL_ROOT [TARGET]}"
case "$install_root" in /*) ;; *) echo 'Monty install root must be absolute' >&2; exit 2;; esac
revision="$(sed -n '/^monty-pool = /s/.*rev = "\([^"]*\)".*/\1/p' "${oss_root}/crates/plasm-agent-core/Cargo.toml")"
[[ "$revision" =~ ^[0-9a-f]{40}$ ]] || { echo 'Missing pinned monty-pool revision' >&2; exit 2; }
target_args=()
if [[ -n "${2:-}" ]]; then target_args=(--target "$2"); fi
cargo install --git https://github.com/pydantic/monty --rev "$revision" \
  monty-runtime --bin monty --locked --root "$install_root" \
  --target-dir "${install_root}/build" "${target_args[@]}"
