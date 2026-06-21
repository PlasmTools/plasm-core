#!/usr/bin/env bash
# Pack three OSS release tarballs from a `cargo build --release` in the workspace root.
# Usage: oss-release-pack-native.sh <rust-triple> <output-dir>
#
# Optional env:
#   PLASM_RELEASE_WORKSPACE_ROOT — monorepo root (default: auto-detect plasm-core vs parent monorepo)
#
# Writes (version is the Git release tag, not in filenames):
#   plasm-appliance-<triple>.tar.gz  (plasm-server + catalogs/)
#   plasm-<triple>.tar.gz            (plasm client)
#   plasm-cgs-<triple>.tar.gz        (plasm-cgs)

set -euo pipefail

triple="${1:?oss-release-pack-native: rust triple required}"
out_dir="${2:?oss-release-pack-native: output dir required}"

oss_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

resolve_workspace_root() {
  if [[ -n "${PLASM_RELEASE_WORKSPACE_ROOT:-}" ]]; then
    cd "${PLASM_RELEASE_WORKSPACE_ROOT}" && pwd
    return
  fi
  if [[ -f "${oss_root}/../Cargo.toml" ]] \
    && grep -q 'plasm-oss/crates/plasm-server' "${oss_root}/../Cargo.toml" 2>/dev/null; then
    cd "${oss_root}/.." && pwd
    return
  fi
  printf '%s' "${oss_root}"
}

workspace_root="$(resolve_workspace_root)"
cd "${workspace_root}"

release_dir="${CARGO_TARGET_DIR:-${workspace_root}/target}/release"

resolve_apis_root() {
  if [[ -d "${workspace_root}/apis" ]]; then
    printf '%s' "${workspace_root}/apis"
  elif [[ -d "${workspace_root}/plasm-oss/apis" ]]; then
    printf '%s' "${workspace_root}/plasm-oss/apis"
  else
    printf '%s' "${oss_root}/apis"
  fi
}

resolve_package_list() {
  local candidate
  for candidate in \
    "${workspace_root}/plasm-oss/scripts/oss-packaged-apis.txt" \
    "${oss_root}/scripts/oss-packaged-apis.txt"; do
    if [[ -f "${candidate}" ]]; then
      printf '%s' "${candidate}"
      return
    fi
  done
  echo "oss-release-pack-native: no oss-packaged-apis list found under ${workspace_root}" >&2
  exit 2
}

apis_root="$(resolve_apis_root)"
package_list="$(resolve_package_list)"

echo "oss-release-pack-native: workspace=${workspace_root} apis=${apis_root}"

build_binaries() {
  cargo build --release \
    -p plasm-server --bin plasm-server \
    -p plasm --bin plasm \
    -p plasm-cli --bin plasm-cgs
}

pack_catalogs() {
  local catalogs_dir="${pack_root}/catalogs"
  mkdir -p "${catalogs_dir}"
  cargo build --release -p plasm --bin plasm-pack-catalogs
  cargo run --release -p plasm --bin plasm-pack-catalogs -- \
    --workspace "${workspace_root}" \
    --apis-root "${apis_root}" \
    --output-dir "${catalogs_dir}" \
    --package-list "${package_list}"
}

pack_root="$(mktemp -d)"
trap 'rm -rf "${pack_root}"' EXIT

echo "oss-release-pack-native: building release binaries…"
build_binaries

for bin in plasm-server plasm plasm-cgs; do
  if [[ ! -f "${release_dir}/${bin}" ]]; then
    echo "oss-release-pack-native: missing ${release_dir}/${bin} after cargo build" >&2
    exit 2
  fi
done

echo "oss-release-pack-native: packing catalogs for ${triple}…"
pack_catalogs

appliance="${pack_root}/appliance"
client="${pack_root}/client"
cgs="${pack_root}/cgs"
mkdir -p "${appliance}" "${client}" "${cgs}"

cp "${release_dir}/plasm-server" "${appliance}/"
if find "${pack_root}/catalogs" -maxdepth 1 -name '*.cgs.json' 2>/dev/null | grep -q .; then
  cp -R "${pack_root}/catalogs" "${appliance}/catalogs"
fi
cp "${release_dir}/plasm" "${client}/"
cp "${release_dir}/plasm-cgs" "${cgs}/"

mkdir -p "${out_dir}"

appliance_out="${out_dir}/plasm-appliance-${triple}.tar.gz"
client_out="${out_dir}/plasm-${triple}.tar.gz"
cgs_out="${out_dir}/plasm-cgs-${triple}.tar.gz"

tar -czf "${appliance_out}" -C "${appliance}" .
tar -czf "${client_out}" -C "${client}" plasm
tar -czf "${cgs_out}" -C "${cgs}" plasm-cgs

echo "oss-release-pack-native: ${appliance_out}"
echo "oss-release-pack-native: ${client_out}"
echo "oss-release-pack-native: ${cgs_out}"
