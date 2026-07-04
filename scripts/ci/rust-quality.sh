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

echo "rust-quality: ok"
