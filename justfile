# plasm-core (OSS subtree): catalog packing only. For Phoenix + SaaS Tool Explorer, use the plasm monorepo root — `just local-web`.

set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

root := justfile_directory()
export PATH := env_var_or_default("PATH", "/usr/bin:/bin")

default:
	@just --list

# Pack apis/* into target/plasm-catalogs (CBOR IL + manifests).
build-catalogs:
	bash -c 'set -euo pipefail; cd "{{root}}"; _cr=(); [[ -z "$${PLASM_OSS_RUST_DEBUG:-}" ]] && _cr=(--release); cargo run "$${_cr[@]}" -p plasm --bin plasm-pack-catalogs -- --workspace "{{root}}" --apis-root "{{root}}/apis" --output-dir "{{root}}/target/plasm-catalogs; if ! find "{{root}}/target/plasm-catalogs" -maxdepth 1 -name "*.cgs.cbor" | grep -q .; then echo "build-catalogs: no *.cgs.cbor in {{root}}/target/plasm-catalogs — apis/ may be empty." >&2; exit 1; fi'
