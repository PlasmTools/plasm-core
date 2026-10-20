#!/usr/bin/env bash
# Canonical native launcher shared by the standalone core and SaaS modules.
set -euo pipefail
workspace="$(pwd -P)"
while [[ ! -f "$workspace/MODULE.bazel" ]]; do
  [[ "$workspace" != / ]] || { echo 'Run within a Bazel module.' >&2; exit 2; }
  workspace="$(dirname "$workspace")"
done
version="$(cat "$workspace/.bazelversion")"
[[ "$version" == 8.4.2 ]] || { echo "Unpinned Bazel version: $version" >&2; exit 2; }
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) platform=darwin-arm64; checksum=45e9388abf21d1107e146ea366ad080eb93cb6a5f3a4a3b048f78de0bc3faffa ;;
  Darwin/x86_64) platform=darwin-x86_64; checksum=ce73346274c379f77880db8bd8b9c8569885fe56f19386173760949da9078df0 ;;
  Linux/aarch64|Linux/arm64) platform=linux-arm64; checksum=58e6042fc54f3bf5704452b579f575ae935817d4b842a76123f05fae6b1f9a83 ;;
  Linux/x86_64) platform=linux-x86_64; checksum=4dc8e99dfa802e252dac176d08201fd15c542ae78c448c8a89974b6f387c282c ;;
  *) echo 'Unsupported Bazel host platform.' >&2; exit 2 ;;
esac
case "$platform" in
  darwin-*) cache="${PLASM_BAZEL_CACHE:-$HOME/Library/Caches/plasm-bazel}" ;;
  *) cache="${PLASM_BAZEL_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/plasm-bazel}" ;;
esac
mkdir -p "$cache/tools"
binary="$cache/tools/bazel-$version-$platform"
verify() {
  [[ -f "$1" ]] && [[ "$(shasum -a 256 "$1" | cut -d ' ' -f 1)" == "$checksum" ]]
}
if ! verify "$binary"; then
  download="$(mktemp "$cache/tools/.bazel.XXXXXXXX")"
  trap 'rm -f "$download"' EXIT
  curl --fail --location --retry 3 --output "$download" "https://releases.bazel.build/$version/release/bazel-$version-$platform"
  verify "$download" || { echo 'Bazel checksum mismatch.' >&2; exit 1; }
  chmod 755 "$download"
  mv -f "$download" "$binary"
  trap - EXIT
fi
# Keep the output base workspace-specific; only content-addressed artifacts share storage.
rc="$(mktemp "$cache/tools/.bazelrc.XXXXXXXX")"
trap 'rm -f "$rc"' EXIT
printf 'build --disk_cache="%s"\n' "$cache" > "$rc"
"$binary" --bazelrc="$rc" "$@"
