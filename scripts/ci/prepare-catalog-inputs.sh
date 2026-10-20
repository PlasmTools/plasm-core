#!/usr/bin/env bash
# Fetch a pinned data artifact; catalog packing itself remains network-free.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tag=v0.5.5
asset=catalog-embedding-inputs-v0.5.5.tgz
sha256=94d33a2b67bb1a7fa2bc5959c5415ef21516af35cd1ad68cc8677493dfa93749
cache="${PLASM_BUILD_INPUT_CACHE:-$HOME/.cache/plasm-build-inputs}"
archive="$cache/$sha256.tgz"
mkdir -p "$cache"
verify() {
  [[ -f "$1" ]] && [[ "$(shasum -a 256 "$1" | cut -d ' ' -f 1)" == "$sha256" ]]
}
if ! verify "$archive"; then
  temporary="$(mktemp -d "$cache/.catalog-inputs.XXXXXXXX")"
  trap 'rm -rf "$temporary"' EXIT
  gh release download "$tag" --repo PlasmTools/plasm-core --pattern "$asset" --dir "$temporary"
  verify "$temporary/$asset" || { echo 'Catalog input archive checksum mismatch.' >&2; exit 1; }
  mv "$temporary/$asset" "$archive"
fi
mkdir -p "$root/build/catalog-embeddings"
tar -xzf "$archive" -C "$root/build/catalog-embeddings"
echo "Catalog embedding inputs verified: $sha256"
