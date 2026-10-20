#!/usr/bin/env bash
# Build one declared artifact and print its stable absolute path (logs on stderr).
set -euo pipefail
target="${1:?Usage: path.sh <target> [Bazel flags...]}"
shift
launcher="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/bazel.sh"
workspace="$(pwd -P)"
while [[ ! -f "$workspace/MODULE.bazel" ]]; do
  [[ "$workspace" != / ]] || exit 2
  workspace="$(dirname "$workspace")"
done
bash "$launcher" build "$target" "$@" >&2
output="$(bash "$launcher" cquery "$target" --output=files "$@")"
[[ -n "$output" && "$output" != *$'\n'* ]] || { echo "Expected exactly one output: $target" >&2; exit 2; }
path="$workspace/$output"
if [[ -d "$path" ]]; then
  cd "$path" && pwd -P
else
  [[ -f "$path" ]] || exit 2
  printf '%s/%s\n' "$(cd "$(dirname "$path")" && pwd -P)" "$(basename "$path")"
fi
