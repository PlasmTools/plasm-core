#!/usr/bin/env bash
# Run the same Bazel graph natively in the pinned Linux environment.
set -euo pipefail
workspace="$(pwd -P)"
while [[ ! -f "${workspace}/MODULE.bazel" ]]; do
  if [[ "${workspace}" == / ]]; then
    echo 'Run this command within a Bazel module.' >&2
    exit 2
  fi
  workspace="$(dirname "${workspace}")"
done
if [[ -f "${workspace}/plasm-oss/MODULE.bazel" ]]; then
  core=/workspace/plasm-oss
elif [[ -f "${workspace}/build/bazel/common.bazelrc" ]]; then
  core=/workspace
else
  echo "Run this command from the core or SaaS repository." >&2
  exit 2
fi
checkout_key="$(printf '%s' "${workspace}" | shasum -a 256 | cut -d ' ' -f 1)"
export_mount=()
if [[ "${1:-}" == export ]]; then
  if [[ $# -lt 3 ]]; then
    echo 'Usage: linux.sh export <target> <output-directory> [Bazel flags...]' >&2
    exit 2
  fi
  export_target=$2
  export_directory=$3
  shift 3
  mkdir -p "${export_directory}"
  export_directory="$(cd "${export_directory}" && pwd -P)"
  export_mount=(--mount "type=bind,source=${export_directory},target=/artifacts")
  set -- export "${export_target}" "$@"
fi
host_arch="$(docker info --format '{{.Architecture}}')"
case "${host_arch}" in
  aarch64|arm64) docker_platform=linux/arm64 ;;
  x86_64|amd64) docker_platform=linux/amd64 ;;
  *) echo "Unsupported Docker host architecture: ${host_arch}" >&2; exit 2 ;;
esac
exec docker run --rm --platform "${docker_platform}" \
  "${export_mount[@]}" \
  --mount "type=bind,source=${workspace},target=/workspace" \
  --mount type=volume,source=plasm-bazel-linux-tools-v1,target=/tools \
  --mount "type=volume,source=plasm-bazel-linux-cache-v2-$(id -u),target=/cache" \
  --workdir /workspace \
  --env "PLASM_BAZEL_OUTPUT_BASE=/cache/outputs/${checkout_key}-${host_arch}" \
  --env "PLASM_BAZEL_CORE=${core}" \
  --env "PLASM_BUILD_UID=$(id -u)" \
  --env "PLASM_BUILD_GID=$(id -g)" \
  rust:1.99-trixie@sha256:6ff07edce8775d0f64be7aba9197229407301bddf2054d62c27b541a6238a181 \
  bash "${core}/build/bazel/linux-entrypoint.sh" "$@"
