#!/usr/bin/env bash
# Export the independently buildable core release archives for this host.
set -euo pipefail
triple="${1:?Usage: oss-release-pack-native.sh <host triple> <output directory>}"
out_dir="${2:?Output directory required}"
mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
case "$(uname -s).$(uname -m)" in
  Darwin.arm64) host=aarch64-apple-darwin ;;
  Linux.x86_64) host=x86_64-unknown-linux-gnu ;;
  Linux.aarch64) host=aarch64-unknown-linux-gnu ;;
  *) echo "Unsupported native release host" >&2; exit 2 ;;
esac
[[ "$triple" == "$host" ]] || { echo "Requested $triple on $host; use the explicit Bazel cross-platform configuration" >&2; exit 2; }
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
bash build/bazel/export.sh //:release_archives "$out_dir" --config=release
