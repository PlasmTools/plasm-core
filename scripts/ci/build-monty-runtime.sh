#!/usr/bin/env bash
# Export the declared Monty worker from core's pinned Bazel graph.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
install_root="${1:?Usage: build-monty-runtime.sh ABSOLUTE_INSTALL_ROOT [TARGET]}"
case "$install_root" in /*) ;; *) echo "Install root must be absolute" >&2; exit 2 ;; esac
cd "$root"
case "${2:-}" in
  "") bash build/bazel/export.sh //:monty "$install_root/bin" --config=release ;;
  aarch64-apple-darwin)
    [[ "$(uname -s).$(uname -m)" == Darwin.arm64 ]] || exit 2
    bash build/bazel/export.sh //:monty "$install_root/bin" --config=release ;;
  x86_64-unknown-linux-gnu)
    bash build/bazel/linux.sh export //:monty "$install_root/bin" --config=linux-x86_64 --config=release ;;
  *) echo "Unsupported Monty target: $2" >&2; exit 2 ;;
esac
