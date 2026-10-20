#!/usr/bin/env bash
set -euo pipefail
: "${PLASM_BAZEL_CORE:?Linux launcher must identify the core module}"
: "${PLASM_BAZEL_OUTPUT_BASE:?Linux launcher must identify the checkout}"
: "${PLASM_BUILD_UID:?Linux launcher must identify the caller}"
: "${PLASM_BUILD_GID:?Linux launcher must identify the caller group}"

case "$(uname -m)" in
  aarch64)
    bazel_arch=arm64
    cmake_arch=aarch64
    bazel_sha=58e6042fc54f3bf5704452b579f575ae935817d4b842a76123f05fae6b1f9a83
    cmake_sha=609735983e3bdf24b6ab379d918458d64196fe72b98226f62dd5e9fe7b2997cc
    ;;
  x86_64)
    bazel_arch=x86_64
    cmake_arch=x86_64
    bazel_sha=4dc8e99dfa802e252dac176d08201fd15c542ae78c448c8a89974b6f387c282c
    cmake_sha=630615d8e98ac33eba7fbe472626dff5c899c85af3c024585ae109166a6909d0
    ;;
  *) echo 'Unsupported native Linux architecture' >&2; exit 2 ;;
esac
bazel_binary="/tools/bazel-8.4.2-${bazel_arch}"
cmake_dir="/tools/cmake-3.31.8-linux-${cmake_arch}"
cmake_archive="/tools/cmake-3.31.8-${cmake_arch}.tar.gz"

# Serialize preparation only; independent checkouts can build concurrently.
(
  flock 9
  if [[ ! -x "${bazel_binary}" ]]; then
    curl --fail --location --retry 3 \
      "https://releases.bazel.build/8.4.2/release/bazel-8.4.2-linux-${bazel_arch}" \
      --output "${bazel_binary}.download"
    echo "${bazel_sha}  ${bazel_binary}.download" | sha256sum --check
    chmod +x "${bazel_binary}.download"
    mv "${bazel_binary}.download" "${bazel_binary}"
  fi
  if [[ ! -x "${cmake_dir}/bin/cmake" ]]; then
    curl --fail --location --retry 3 \
      "https://github.com/Kitware/CMake/releases/download/v3.31.8/cmake-3.31.8-linux-${cmake_arch}.tar.gz" \
      --output "${cmake_archive}"
    echo "${cmake_sha}  ${cmake_archive}" | sha256sum --check
    tar -xzf "${cmake_archive}" -C /tools
  fi
) 9>/tools/preparation.lock

export PATH="${cmake_dir}/bin:${PATH}"
if [[ $# -eq 0 ]]; then
  echo 'Usage: bash build/bazel/linux.sh <build|test|run> <Bazel arguments...>' >&2
  exit 2
fi
command_name=$1
shift
case "${command_name}" in
  build|test|run|export) ;;
  *) echo 'The Linux launcher supports build, test, run and export.' >&2; exit 2 ;;
esac
# Only tool provisioning runs as root. Builds and their checkout reports retain
# the caller's ownership; the cache volume is shared by that user's checkouts.
export HOME=/cache/home CARGO_HOME=/cache/cargo USER="plasm-${PLASM_BUILD_UID}"
mkdir -p "${HOME}" "${CARGO_HOME}"
chown "${PLASM_BUILD_UID}:${PLASM_BUILD_GID}" /cache "${HOME}" "${CARGO_HOME}"
bazel_command() {
setpriv --reuid="${PLASM_BUILD_UID}" --regid="${PLASM_BUILD_GID}" --clear-groups \
  "${bazel_binary}" --batch --nosystem_rc --nohome_rc --noworkspace_rc \
  --bazelrc="${PLASM_BAZEL_CORE}/build/bazel/common.bazelrc" \
  --output_base="${PLASM_BAZEL_OUTPUT_BASE}" \
  "$1" --disk_cache=/cache/disk --repository_cache=/cache/repository \
  --symlink_prefix=/tmp/plasm-bazel-links/ \
  --platform_suffix=linux-rust-1.99-trixie-6ff07edc \
  --action_env="CMAKE=${cmake_dir}/bin/cmake" \
  --jobs=4 "${@:2}"
}

if [[ "${command_name}" != export ]]; then
  bazel_command "${command_name}" "$@"
  exit
fi

export_target=${1:?Export requires a Bazel target}
shift
bazel_command build "${export_target}" "$@"
output_files="$(bazel_command cquery "${export_target}" "$@" --output=files)"
[[ -n "${output_files}" ]] || { echo 'Export target has no outputs' >&2; exit 2; }
declare -A exported_names=()
while IFS= read -r relative; do
  name=$(basename "${relative}")
  if [[ -n "${exported_names[${name}]:-}" ]]; then
    echo "Export outputs have duplicate basename: ${name}" >&2
    exit 2
  fi
  exported_names[${name}]=1
  source_path="${PLASM_BAZEL_OUTPUT_BASE}/execroot/_main/${relative}"
  if [[ ! -f "${source_path}" ]]; then
    echo "Export requires file outputs; archive directory targets first: ${relative}" >&2
    exit 2
  fi
  setpriv --reuid="${PLASM_BUILD_UID}" --regid="${PLASM_BUILD_GID}" --clear-groups \
    cp -f "${source_path}" "/artifacts/${name}"
done <<< "${output_files}"
