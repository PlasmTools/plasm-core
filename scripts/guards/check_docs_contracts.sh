#!/usr/bin/env bash
# Fail CI when published docs / skills / READMEs reintroduce removed contracts.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

fail=0
hit() {
  local label="$1"
  shift
  local out
  if out="$(rg -n --glob '!**/target/**' --glob '!**/node_modules/**' --glob '!**/site/**' "$@" 2>/dev/null)"; then
    echo "docs-contracts: FAIL (${label})"
    echo "${out}"
    fail=1
  fi
}

# Removed catalog flag.
hit "plugin-dir" -e '--plugin-dir' -e 'plugin_dir' \
  README.md AGENTS.md doc-site/docs skills crates/*/README.md apis/*/README.md packages

# Wrong cargo package/bin pairs (plasm-cli has plasm-cgs, not plasm; REPL is -p plasm-repl).
hit "wrong-cargo-repl" -e 'cargo run -p plasm --bin plasm-repl' \
  README.md AGENTS.md doc-site/docs skills crates apis packages
hit "wrong-cargo-cgs-on-plasm" -e 'cargo run -p plasm --bin plasm-cgs' \
  README.md AGENTS.md doc-site/docs skills crates apis packages
hit "wrong-cargo-bin-plasm" -g '*.md' -e 'cargo run --bin plasm[^-a-zA-Z]' \
  README.md AGENTS.md doc-site/docs skills crates apis packages

# MCP agents must not be taught plan_commit_ref as a plasm_run argument.
if out="$(rg -n -g '*.md' 'plasm_run[^\n]{0,120}plan_commit_ref\s*=' doc-site/docs skills 2>/dev/null)"; then
  echo "docs-contracts: FAIL (plasm_run plan_commit_ref assignment)"
  echo "${out}"
  fail=1
fi
if out="$(rg -n -g '*.md' 'pass `plan_commit_ref`|accepts the reviewed `plan_commit_ref`|returns `plan_commit_ref`' doc-site/docs skills 2>/dev/null)"; then
  echo "docs-contracts: FAIL (MCP plan_commit_ref teaching)"
  echo "${out}"
  fail=1
fi

# Executable legacy p# samples in public docs/skills.
hit "executable-legacy-p" -g '*.md' \
  -e 'e[0-9]+\(p[0-9]+=' \
  -e '\[[pP][0-9]+,' \
  -e '=[pP][0-9]+=' \
  doc-site/docs skills

# UUID-shaped run URI teaching (canonical is pr + 64 hex).
hit "uuid-run-uri" -g '*.md' \
  -e 'plasm://[^`[:space:]]*/run/\{?uuid' \
  -e 'plasm://session/[^`[:space:]]+/r/\{' \
  doc-site/docs skills

# Stale semantic plan hash vocabulary.
hit "stale-plan-hash-fields" -g '*.md' \
  -e 'nodes.*edges.*topological_order.*returns' \
  doc-site/docs skills

if [[ "${fail}" -ne 0 ]]; then
  echo "docs-contracts: rejected — restore wire-first / run_ref / catalog-dir contracts"
  exit 1
fi

echo "docs-contracts: ok"
