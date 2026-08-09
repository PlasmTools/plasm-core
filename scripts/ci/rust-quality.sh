#!/usr/bin/env bash
# Rust fmt + clippy (-D warnings). OSS workspace root (plasm-core checkout).
# PLASM_RUST_FMT_MODE=fix  — format in place (pre-commit); default check (CI).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

_fmt_mode="${PLASM_RUST_FMT_MODE:-check}"
if [[ "${_fmt_mode}" == fix ]]; then
  echo "rust-quality: cargo fmt --all"
  cargo fmt --all
else
  echo "rust-quality: cargo fmt --all -- --check"
  cargo fmt --all -- --check
fi

echo "rust-quality: cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

# Explicit NAPI crate (anyhow/`Display` consumers) — must stay green for npm publish.
echo "rust-quality: cargo clippy -p plasm-node --all-targets -- -D warnings"
cargo clippy -p plasm-node --all-targets -- -D warnings

if [[ -x "${ROOT}/scripts/guards/check_docs_contracts.sh" ]] || [[ -f "${ROOT}/scripts/guards/check_docs_contracts.sh" ]]; then
  echo "rust-quality: docs contract guard"
  bash "${ROOT}/scripts/guards/check_docs_contracts.sh"
fi

echo "rust-quality: fenced doc examples"
cargo test -p plasm-core doc_fenced_plasm_examples_parse_under_language_matrix -- --nocapture

echo "rust-quality: ok"
