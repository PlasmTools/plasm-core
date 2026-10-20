#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
bash build/bazel/bazel.sh test //:nextest --test_arg=--test-threads=1 \
  --test_arg=-E --test_arg='package(plasm-agent-core) & (test(python_pool) | test(map_body) | test(python_host) | test(membership_evidence) | test(python_iteration_stateful_contract_both_drivers))' \
  --test_output=errors --test_timeout=7200
bash build/bazel/bazel.sh test //:nextest_integrations \
  --test_arg=-E --test_arg='binary(python_compute_feasibility)' \
  --test_output=errors --test_timeout=7200
