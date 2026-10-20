#!/usr/bin/env bash
# Publication may compile schemas, but must not pull in execution or host services.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tree="$(cargo tree --locked --offline --manifest-path "$root/Cargo.toml" -p plasm-catalog --edges normal --prefix none)"
if printf '%s\n' "$tree" | grep -E '^(plasm-agent-core|plasm-runtime|plasm-server|rmcp) v'; then
  echo 'Catalog publication depends on execution or host services' >&2
  exit 1
fi
echo 'Catalog publication boundary passed'
