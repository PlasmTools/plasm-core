# Semantic rows and resolved Get requests

Execution rows cross decoder, cache, spill, graph-embedding and host boundaries. Their identity must survive those boundaries without becoming display text, and storage metadata must not become rowset columns.

## Ownership

`plasm_core::row_contract::EntityRow` exposes identity, typed fields, observed relations and unavailable fields. Implementations for `DecodedEntity`, `CachedEntity` and `RowRecord` expose storage data only. `RowCodec` owns the shared execution wire format and its inverse. A codec receives the caller's pinned CGS; catalogs remain separate.

`RowRecord` contains semantic data. Cache timestamps, versions and completeness remain in `CachedEntity`. Cache serialization adds those metadata fields around the semantic row. Cache and host rematerialization remove the exact storage keys before decoding. Public underscore-prefixed fields such as `_tag` remain ordinary fields.

Relations carry structural `RefWire` identities, with CGS-typed identity fields. A missing relation is unobserved; a present empty collection is observed empty. Relation order and duplicates survive serialization. Decoding rejects display strings, malformed references, wrong target entities and incorrect relation cardinality. Presentation methods are terminal projections and are not execution codecs.

Host input has two distinct forms: execution rows carrying `_ref`, and raw API embeds or computed rows without a structural reference. Execution rows use the shared codec and propagate errors. Computed rows can receive synthetic identities; that rule cannot repair a malformed execution reference.

`PublicRowSchema` projects declared columns before union. Top-level cache metadata cannot change union compatibility or distinctness. Column names beginning with underscores are preserved when declared. Ordinary nested JSON values retain their value semantics.

## Get binding boundary

`GetBindings` exposes parameters from materialization, ambient context or an inherited `CapabilityParamEnv`. `ResolvedGet` selects and checks the capability, combines parameters with explicit precedence, projects identity and normalizes reference operands. Live HTTP dispatch requires this resolved value. Dry preflight uses the same resolver, including ambient parameters. Composed and derived Gets receive the resolved ambient environment.

Session/row bindings take precedence over ambient bindings. Inherited bindings continue through the existing provider-provenance checks (RA-17). These are capability parameters, not Plasm transport authentication. The resolver does not inspect token strings or encode any evaluation-specific behavior.

The identity AST does not carry execution parameters. Helpers accepting parameters but discarding them have been removed; callers pass context explicitly. Reconciliation now passes its inherited context as well.

## Validation

The same generated row law is exercised against decoded, cached and owned row adapters. The same generated binding law is exercised against session, ambient and inherited binding adapters. Tests serialize between boundaries and cover identity, scalar types, relation presence, order, multiplicity and parameter precedence.

`hydration_boundary_matrix` exercises actual compiled requests and host/native execution, including delayed fanout completion, mixed complete and partial embedded payloads, cache reuse, public-row union and a subsequent Get. The language matrices and graph-spill integration exercise the wider runtime paths. These are deterministic runtime checks, not an AppWorld score.

Commands (from the parent workspace):

```sh
cargo test -p plasm-runtime --lib -- --test-threads=1
cargo test -p plasm-agent-core --lib
cargo test -p plasm-e2e --test plasm_language_matrix --test plasm_language_matrix_views --test graph_spill_e2e
cargo test -p plasm-node --features napi/noop native_boundary_hydration --lib
```

The native test uses `napi/noop` to test the Rust engine and JSON boundary without linking to a running Node process. It does not exercise JavaScript loading of the addon.

## Infrastructure-only tests

On 2026-09-21, all four ignored `plasm-agent-core` library tests passed with explicit `--ignored --test-threads=1` against disposable local containers:

- `discovery_database_burst_waits_without_pool_timeouts`: 96 queued callers, one database connection, transaction rollback and no admission timeout.
- `discovery_database_cancellation_and_errors_release_admission`: canceled waiters/holders and failed queries release admission.
- `postgres_generation_and_retrieval_contract`: pgvector-backed generation, retrieval, pinned sessions and concurrent provenance updates.
- `redis_commit_rehydrates_and_write_failure_is_not_acknowledged`: durable credential references, lifetime and explicit failure when an ACL denies writes.

The ignores remain intentional. Set `PLASM_TEST_POSTGRES_URL` to an isolated PostgreSQL database with pgvector and `PLASM_TEST_REDIS_URL` to a disposable Redis instance with ACL administration. These tests mutate infrastructure and must not target production services. Both test containers were removed after the run.
