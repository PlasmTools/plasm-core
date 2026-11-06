# Workflow learning: discovery and atomic example serving

Status: offline contract plus Rust integration in progress. This document states
the target guarantee; the implementation limitations below are release gates.

## Boundary

This is an OSS subsystem, partitioned by tenant and project. Return at most three
suggestions per intent across its subtasks. Examples are reusable fragments with
explicit typed inputs; they do not capture caller-specific values or hidden
session artifacts as reusable defaults.

Examples contribute bounded recall evidence to discovery. They do not grant
catalogue access, execute a program, or establish that the current intent is
fulfilled. Search uses current self-contained intent and authorized catalogue
contracts. Literal similarity alone is insufficient. Discovery retains its
contribution judgment and prerequisite closure.

An optional fetch returns an executable example rebound to the caller's current
logical session, together with its minimal additional declarations. Existing
symbols are never renumbered. A dependency projection preserves authoritative
catalogue ownership, argument roles, constraints, effects, units and relations;
it is not a separately authored or merged CGS.

## Interfaces and classifications

- Recall input: current intent, principal/scope, pinned generation, current
  capability authorization and exposure. Output: bounded example revision IDs,
  applicability summaries and required capability references. Filter scope and
  authorization before returning summaries, titles, scores or IDs.
- Fetch input: example revision ID and logical session reference. Output: bound
  Python source, exact teaching delta and a committed receipt identifying the
  example revision, session exposure revision and validation fingerprints.
- Stable references cover entities, capabilities, properties and relations;
  entity-only template holes cannot guarantee rebinding of stored programs.
- Acquisition capabilities come from the compiled program, including implicit
  query/Get operations. Method symbol occurrences alone are not a dependency
  manifest. Apply prerequisite closure to this compiled manifest before staging.
- Reject hidden session dependencies such as `self.recorded` references during
  promotion unless transformed into explicit typed inputs and independently
  validated. Fetch must not acquire business data or invoke outbound operations.
- A deterministic source/type failure quarantines the example revision for the
  effective catalogue/compiler fingerprint. Preserve source, provenance and
  structured diagnostics. Catalogue versions are validation metadata: a version
  change triggers normal compilation against current contracts, not automatic
  quarantine. Unrelated catalogue additions remain compatible. Referenced
  contract changes are checked separately from the whole-catalogue digest.
- Unauthorized, unavailable connection, expired session, stale revision and
  infrastructure/checker unavailability are request failures, not evidence that
  the stored example is bad. They must not globally quarantine the example.

## Atomic fetch protocol

1. Resolve the logical session and example under the same principal/scope. Read
   its immutable revision and current compatibility state. Authorize the entire
   dependency closure, including prerequisites and acquisition declarations.
2. Materialize only catalogue/schema information outside the exposure gate. If
   materialization requires business acquisition, stop with an explicit unmet
   dependency; do not execute it implicitly.
3. Capture the session exposure revision, catalogue pin fingerprints,
   authorization revision, example revision and status revision. Clone an
   isolated preflight session. Stage all new catalogues and exposure waves,
   including relation slots, symbol allocation and Python declarations, in this
   clone. Render one accumulated delta without publishing it.
4. Rebind stable references against the staged map and compile through the
   production Python admission/typechecking path. No plan execution occurs.
   Compilation failure discards staging and records the scoped diagnostic.
5. Under the exposure commit gate, compare all captured revisions, recheck
   authorization and example servability, and verify the session still exists.
   On conflict discard the staged result and return a typed retryable conflict;
   never publish source referring to the discarded allocation.
6. Atomically persist the execute-session exposure snapshot, logical symbol
   ledger and fetch receipt, using one transaction and compare-and-swap against
   the durable session revision. Publish hot state only after durable commit.
   A local mutex alone does not protect multi-replica writers.
7. Return source and declarations from the committed receipt. If delivery fails
   after commit, retrieving that receipt is idempotent and allocates no new
   symbols. A receipt contains the full fetch result; repair or supersession
   cannot silently rewrite it. Access is checked again on receipt retrieval.

The transaction is the publication point. Failed validation, authorization,
stale revision or persistence must leave both the live session and durable
ledger unchanged. In-memory deployments apply the same compare-and-swap to
one aggregate state. Do not claim durable atomicity across independent stores.

## Exact integration targets and required refactor

All paths below are relative to `crates/plasm-agent-core/src/`.

- `discovery_service.rs::route_uncached`: add bounded example-derived recall
  evidence before `select_recalled_indexed`; retain `prerequisite_closure` and
  `validate_closure_authorization` as authoritative checks.
- `http_execute/context/session/{expand,federate,commit}.rs`: extract pure
  staging of exposure/declarations from publication. Existing
  `commit_federate_and_expand_waves` publishes separate waves and cannot provide
  all-or-none fetch. `commit_exposure_wave_delta` currently renders and persists
  immediately; its staging half must return an unpublished session and delta.
- `plasm_compile.rs::compile_program`: compile the staged snapshot through the
  same Python diagnostic and compute admission path. Its recorded-output
  prefetch is why hidden artifact references must be excluded beforehand.
- `server_state.rs::durable_snapshot_and_write`: currently upserts the logical
  ledger before writing the execute snapshot. A later failure can leave the
  ledger advanced. Introduce a transactional aggregate write for this feature;
  do not compose these two independent writes and describe them as atomic.
- `mcp_server/plasm_context.rs`: existing scope/permission intersection and
  logical-session resolution are reusable. Fetch must not create an unrelated
  session merely to obtain convenient symbols.

### Redis and in-memory publication

`mcp_transport_store/redis_backend.rs` already uses strict `EVAL` calls for
credential commits. Add an analogous compare-and-swap publication that checks
the current descriptor revision and writes descriptor, logical ledger and fetch
receipt atomically, with consistent expiry. Validate every argument before the
first write; Lua execution errors do not roll back earlier writes. Return a
typed conflict or committed receipt, and propagate Redis errors. Do not call
`execute_session_registry::write_descriptor` for this guarantee: its current
Redis path uses best-effort `set_json` and returns `Durable` without an explicit
write acknowledgement. In-memory publication uses one mutex over the same
aggregate, with no await between validation and publication.

All exposure writers, including normal expand/federate, must use the same
durable compare-and-swap revision; otherwise a stale ordinary writer can
overwrite a successful fetch. On conflict refresh the snapshot and recompile;
the caller receives a typed retryable conflict rather than an unbounded retry
loop. The archive object store is post-commit replication, not a second atomic
publication point. Archive restoration must reject an uncommitted or obsolete
revision using the authoritative publication record. Redis Cluster support
requires colocated keys in one hash slot; existing unconstrained key layouts
must not be advertised as supporting multi-key Cluster transactions.

No new dependency is required for the offline contract model. Production
transaction support must use existing storage infrastructure or obtain explicit
dependency approval. Before enabling serving, prove atomicity in the actual
durable backend and session concurrency tests, not solely in this model.

## Verification

### Execution completeness watermark

Before any live effects, the authoritative session backend issues a monotonic
attempt token. Redis stores an `issued`/`completed` hash plus idempotent attempt
IDs; the memory backend uses one locked aggregate. These records have no TTL and
are removed only with the execute session. A missing record is not evidence of
completeness. Completion advances only for the newest issued token whose exact
predecessor was already completed. A failed journal write, crash, missing receipt
or out-of-order concurrent execution therefore leaves the session ineligible for
review across replicas. Optional PostgreSQL learning outages cannot erase this
watermark or make another replica mistake incomplete evidence for success.

Review sealing acquires a 30-second fenced lease on the complete watermark;
execution admission rejects new effects while it is active. PostgreSQL prepares
the immutable review in an uncommitted transaction. The authoritative backend
then checks the same fence, unexpired lease and unchanged complete watermark,
records the seal witness and releases the lease. Only then does PostgreSQL
commit the prepared row, with no intervening work. The complete sequence has a
20-second timeout. A failure before the authoritative seal rolls back an
invisible review; a crash after the seal can leave an unused witness, never an
unverified claimable review. Aborting an old lease cannot release a newer fence.

### Current implementation limits

`workflow_learning/serving.rs` stages the complete teaching wave, rebinds and
compiles it, then takes a shared lock on the active PostgreSQL example row
through session publication. Quarantine and supersession cannot race through
that row lock. Authorization follows the host's request-snapshot semantics and
is checked again immediately before publication; this is not a global atomic
revocation transaction across PostgreSQL and Redis.

Descriptor publication atomically updates Redis descriptor and logical ledger;
same-revision competing allocations, stale revisions and skipped allocation
revisions are rejected. Metadata refresh may advance multiple revisions only
when its symbol-ledger bytes are identical to the committed ledger.
The local backend publishes one authoritative descriptor aggregate containing
the version, ledger and receipts. Reads consult it and rehydrate an older hot
session, including after cancellation between commit and hot-cache update. Fetch receipts
persist the source, declaration delta and published domain revision in the same
publication operation. Repeating a successful fetch returns that receipt after
fresh eligibility and access checks. Typed input slots and output contracts are
part of the receipt, with input positions rebound to the served source. Optional
fetch rejects cold or stale sessions and unmaterialized schema overlays; normal
context discovery must prepare those dependencies. Object-store ledger archives replicate only after
publication; archive-only multi-replica restoration is not a verified atomic
serving configuration. Redis Cluster multi-key support is not implemented.

`python3 -m unittest discover -s scripts/workflow_learning -p test_serving.py`
exercises a dependency-free reference state machine: complete closure checking,
scope privacy, stable allocation, no partial publication on each failure,
cross-writer conflict, scoped quarantine and receipt idempotence. These are
contract tests, not evidence that Rust compilation or storage integration works.

Rust integration acceptance additionally requires matrix-backed compilation
under different pre-existing symbol orders, dependency/semantic drift,
capability revocation at commit, crash injection between persistence operations,
and Shuttle coverage for concurrent exposure/commit. Example retrieval must not
emit outbound business requests. Evaluate usefulness with frozen held-out
intents and separate discovery-only and example-assisted arms.
