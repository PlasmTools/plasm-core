#!/usr/bin/env bash
# Provider-free Rust receipt -> TypeScript decoder/presentation and bootstrap gate.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
contract="$(mktemp)"
trap 'rm -f "$contract"' EXIT
cd "${PLASM_WORKSPACE_ROOT:-$root}"
cargo run --locked --quiet -p plasm-agent-core --example routing_contract > "$contract"
PLASM_ROUTING_CONTRACT="$contract" npm run test:context-routing --prefix "$root/packages/plasm-agent"
npm run test:session-extension --prefix "$root/packages/plasm-agent"
npm run test:initial-context --prefix "$root/packages/plasm-agent"
