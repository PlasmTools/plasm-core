#!/usr/bin/env bash
# Export declared archive/package outputs for release upload without rebuilding them.
set -euo pipefail
if [[ $# -lt 2 ]]; then
  echo 'Usage: export.sh <target> <output-directory> [Bazel flags...]' >&2
  exit 2
fi
target=$1
destination=$2
shift 2
launcher="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/bazel.sh"
workspace="$(pwd -P)"
while [[ ! -f "$workspace/MODULE.bazel" ]]; do
  [[ "$workspace" != / ]] || { echo 'Run within a Bazel module.' >&2; exit 2; }
  workspace="$(dirname "$workspace")"
done
bash "$launcher" build "$target" "$@"
outputs="$(mktemp "${TMPDIR:-/tmp}/plasm-bazel-exports.XXXXXXXX")"
trap 'rm -f "$outputs"' EXIT
bash "$launcher" cquery "$target" --output=files "$@" > "$outputs"
[[ -s "$outputs" ]] || { echo "Target has no files: $target" >&2; exit 1; }
mkdir -p "$destination"
while IFS= read -r output; do
  source="$workspace/$output"
  [[ -f "$source" ]] || { echo "Export requires a regular file: $output" >&2; exit 1; }
  cp -f "$source" "$destination/$(basename "$output")"
done < "$outputs"
