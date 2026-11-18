#!/usr/bin/env bash
# Fetch a pinned data artifact; catalog packing itself remains network-free.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tag=v0.6.1
asset=catalog-embedding-inputs-discovery-v34.tgz
sha256=caf38b2cffe386985ab9e4c90bd47bc9f11a4abd5f6912de41a5acc6552c1b47
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
