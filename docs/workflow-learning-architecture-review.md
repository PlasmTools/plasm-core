# Workflow learning architecture review

## Scope and standard

This review covers the whole-program teaching library, independent catalogue guidance, immutable compiler context, evidence lifecycle, native/MCP integration and their verification. It compares the scoped working tree against OSS commit `24d12f54`; unrelated discovery experiments, provider catalog changes and test removals are excluded. Standards come from the parent repository's `production-rust.md` and `AGENTS.md`. Fowler smells are architectural heuristics, not automatic violations.

The unit of learning is a complete authored program. Compiler checks and retained execution evidence have separate authority. An example teaches authoring; it does not authorize effects or claim a formal reusable-template equivalence proof.

## Findings and resolutions

| Finding | Resolution |
| --- | --- |
| Serving converted several distinct preparation errors into strings, losing typed causes. | `ServingError` now preserves catalogue, prerequisite, exposure, domain-teaching and session errors. Persisted diagnostic messages remain presentation data. |
| Durable receipts represented unrelated identities with interchangeable strings. | Receipts, review inputs and saved contexts use logical-session, run-artifact, compiled-program, prompt-hash and execute-session identity types. Parsing occurs at external boundaries. |
| Learning interpreted the computation and relation strategies independently of the compiler. | Static catalogue dependencies are issued by the compiler, bound to the complete computation digest and retained in the immutable compilation snapshot. Teaching and catalogue guidance consume this evidence. Core relation capability requirements are shared with runtime preflight; dynamic row/cache execution remains runtime-owned. |
| Python nominal-domain symbols have an authority distinct from the entity/relation ledger. | Compiler snapshots retain the actual prepared Python teaching state; serving relocates nominal annotations through that authority and checks catalogue ownership, pins and named-domain contracts. Type-only exposure does not invent callable capabilities. |
| Confidential literals routed through local bindings could escape direct-operand classification. | Compiler disclosure tracks the sensitive operand's authored origin. Retrieved dataflow remains distinct from an embedded confidential default; unresolved flow is withheld rather than claimed safe. |

`CompiledCatalogueDependencies` describes static lowered/typechecked IR. It is captured before full Python/Monty admission and is deliberately not named or treated as proof of that later gate, execution success or behavioral equivalence. Its absence is explicit; consumers do not reconstruct a second interpretation as a fallback.

The execution ticket, evidence journal and review gate are useful behavioral seams: their types enforce sequencing and their offline implementations support crash/concurrency checks. They are not interchangeable wrappers introduced merely to increase trait count. Review sealing and portfolio publication retain their durable transaction/fence ownership; optional retrieval stages exposure before publication.

## Verification

Current verification uses compiler-artifact paths from Cargo's JSON output, an explicit runtime `CARGO_MANIFEST_DIR`, local Monty admission and the isolated PostgreSQL database `workflow_revision_test` with per-test schemas.

| Check | Result |
| --- | --- |
| Workflow learning suite, including typed identities and version-2 migration/publication refusal | 71 passed |
| Immutable compilation context and isolated repair | 8 passed |
| Compiler-issued dependency evidence | 3 passed |
| Compiler operand evidence | 1 passed |
| Plan commit persistence/coherence | 21 passed, 1 ignored |
| Runtime relation tests | 17 passed |
| Native fixture export | 1 passed |
| Core/node all-target strict Clippy | Passed |
| Explicit fresh-binding cross-process proof | 1 passed |

The full workflow filter initially returns early from the opt-in native proof when its binding environment is absent; the separate explicitly configured proof also passed (4.65 seconds), using `/tmp/plasm-purity-final.node` and the fresh abstract fixture. No external provider calls are part of this verification. The only Clippy warning is in the external vendored Monty dependency; scoped crates pass `-D warnings`.

Commands:

```sh
cargo test -p plasm-agent-core -p plasm-node --features napi/dyn-symbols --lib --no-run --offline --message-format=json
cargo test -p plasm-runtime --lib relation --offline
cargo clippy -p plasm-agent-core -p plasm-node --all-targets --offline -- -D warnings
cargo build -p plasm-node --lib --offline
```

The final direct core test filter is `workflow_learning`; related filters are `program_symbol_context`, `plasm_dag::dependencies`, `plasm_dag::python::operand_evidence` and `plan_commit_store`. The native proof is `postgres_native_process_restart_promotes_and_fetches_checked_example` with `PLASM_NATIVE_BINDING`, `PLASM_NATIVE_LEARNING_FIXTURE_MANIFEST`, `PLASM_TEST_POSTGRES_URL` and `PLASM_MONTY_BINARY` explicitly set.

## Remaining boundaries

Typed compiler disclosure is not a universal secret detector. Successful execution evidence and agent-attested goal satisfaction remain separate assurance levels. The prior retained AppWorld check established exact computation/source preservation and teaching eligibility for two reconstructable contexts out of seven reviewed programs; it did not establish held-out usefulness or promotion quality. Further task evaluation must use its own frozen inputs and report its complete denominator.

Native binding SHA-256 for this verification: `01417ef3afc329fba754ca2af3df654cacb18426f6200b6b8e57d9c7333a4f67`. The cross-process proof collects real local-fixture executions, seals and curates their review, persists and publishes examples plus independent guidance, and fetches checked whole-program source from a separate process under reordered session symbols.
