#!/usr/bin/env bash
set -euo pipefail
oss_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workspace="$(pwd)"
bash "${oss_root}/scripts/ci/build-monty-runtime.sh" "${workspace}/target/monty-runtime"
export PLASM_MONTY_BINARY="${workspace}/target/monty-runtime/bin/monty"
cargo test -p plasm-agent-core --lib python_pool -- --test-threads=1
cargo test -p plasm-agent-core --test python_compute_feasibility
cargo test -p plasm-agent-core --lib map_body
cargo test -p plasm-agent-core --lib python_host
