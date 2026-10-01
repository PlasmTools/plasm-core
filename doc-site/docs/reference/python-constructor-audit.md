# Python constructor contracts: implementation audit

Audit date: 2026-09-27. This records producers, consumers and unresolved facts;
it is not certification that all constructors preserve the six-dimensional
judgment in [Python conformance](python-conformance.md). Paths below are relative
to `crates/`. The finite oracle deliberately does not import these implementations.

## Representation map

| Fact | Producer / representation | Consumer | Audit finding |
|---|---|---|---|
| Catalog type and qualified owner | `plasm-agent-core/src/plasm_dag/binding_contract.rs`; `ValidatedSurfaceNode` in `plasm_plan/types.rs` | Python field admission, compute declarations, runtime dispatch | Catalog identity is available; it is not evidence that every catalog field was observed. |
| Projected / observed shape | Surface `projection`; compute `SyntheticResultSchema`; optional capture schema | `map_body_schema.rs::row_fields` | Synthetic schemas and presence-aware observed records are explicit. Catalog fields describe allowed values, not guaranteed presence; explicit projections require their selected fields. Broader shape/coverage products remain unproved. |
| Cardinality | Binding contracts, map bounds, scoped output contracts | Scalar admission, map execution, materialization | Static singleton/plural and runtime bound checks have separate owners; neither proves membership completeness. |
| Membership coverage | `plasm_plan_run/coverage_fold.rs`, `compute_eval/relation_coverage.rs` | `compute_eval/input_rows.rs`, `map_body.rs` | Relation completeness requires an explicit membership witness. Missing dependency evidence remains Unknown. Exact limits do not automatically establish completeness. |
| Order and multiplicity | Derive nodes and `plasm-core/src/row_union.rs` | `compute_eval/eval.rs::derive_node_rows` | RA-14 defines first-wins set union. The compact teaching incorrectly said duplicates survived; corrected to match the normative law and existing matrix. |
| Receiver authority | Binding contract, `ValidatedCaptureNode.entity_authority`, materialized row identity sidecar | Relation/mutator admission and dispatch | Value-shaped IDs must not create authority. Identity augmentation at the compute boundary must be checked independently of recursive value shape. |
| Effects and dependencies | Surface effect class; nested plans and capture ports; `plan_schedule.rs` | `plasm_plan_run/map_body.rs`, iteration, operation receipts | A late materialization error can follow completed writes. Coverage rejection alone does not establish zero IO or adequate failure receipts. |

## Constructor transfer and boundary review

| Constructor | Required transfer | Current owner / unresolved work |
|---|---|---|
| Read, relation, mutation result | Derive available shape from the observation contract; keep membership and hydration proofs distinct | `plasm_dag/python/reads.rs`, `writes.rs`, binding contracts and relation coverage. Observed records preserve absent fields separately from null. Read and nested-create arrays have finite witnesses; other result/effect products remain open. |
| Filter, sort, take | Preserve available field types and lawful authority; transfer order/cardinality/coverage separately | Derive kernel and coverage fold. Read-value oracle compares complete inputs; the separate 36-case collection family exercises paging. Unknown/unavailable and resumed continuation products remain open. |
| Projection and union | Project actual columns; preserve domain types; compatible union with RA-14 first-wins multiplicity | `python/projection.rs`, row contract/union. `PublicRowSchema::project` omits declared optional fields and rejects missing required columns. The observed-shape oracle includes absence; full projection/union/presence products remain open. |
| Record map and nested capture | Produce a closed record shape; recursive arrays require complete inputs; parent captures remain scoped | `python/body.rs`, `fanout.rs`, `map_body_schema.rs`, runtime `map_body.rs`. Synthetic and presence-aware observed arrays have differential witnesses; broader recursive products remain open. |
| Flat-map | Concatenate lawful row results, preserving their authority and combined coverage | Nested plan/runtime map. Not represented in reference v1; flattening and entity authority cannot be inferred from record-map evidence. |
| Aggregate / group | Explicit empty behavior, result schema, complete input and synthetic authority | The reduction-v1 independent model covers 48 finite complete-input products; broader type/coverage/group products remain open. |
| Compute / render | Exact input/output contracts, Monty-owned Python semantics, explicit dependencies | `python_compute`, compute input materialization and `ValueContract`. Typed scalar, domain, record, array and union outputs are implemented with finite `typed_returns` witnesses; see [typed return contract](python-return-contract.md). Arbitrary constructor/type/effect products remain open. |
| Iterate / effects | Bounded causal execution, fresh reads, explicit stop failure and retained effect prefix | Scheduler and `compute_eval/iterate_until.rs`. Independent effect-trace model, cancellation and replay products remain open. |

`compute_eval/input_rows.rs::materialized_input_row_from_mat` checks collection
coverage, copies inline rows and augments identity/ambient slots through
`row_json.rs`. `ValueContract` distinguishes closed records from `ObservedRecord`: required
fields must exist, while observed optional fields may be absent. Extra fields are
rejected. A nullable field permits explicit null; nullability alone does not
permit absence. The producer and consumer must agree on the same
observed shape without leaking authority metadata into ordinary values.

The current representation distributes facts among nodes, schemas, row codecs,
coverage and identity sidecars. A future contract refactor must identify which
facts are immutable plan evidence and which are runtime observation evidence;
putting a catalog schema into a larger struct would not itself solve this gap.
The deferred row-method dispatch now uses a closed production registry checked against
[executable constructor rules](python-row-constructors.md). The four build statement
constructors also use [closed dispatch and executable rules](python-build-constructors.md).
The seven catalog capability kinds use [typed dispatch and IL-checked witnesses](python-catalog-constructors.md).
Relation navigation also has a [recursive typed evidence gate](python-relation-constructors.md).
The [outer scalar DAG constructors](python-scalar-constructors.md) also have a closed gate.
[Recursive literal operands](python-literal-constructors.md) have a shared
production classifier for primitive, write and scoped-record consumers.
[Program/class admission](python-declaration-constructors.md) also uses a closed
declaration inventory, full-module negatives and four compute-annotation witnesses.
[Named projection expressions](python-projection-constructors.md) have a closed recursive gate.
[Outer predicates](python-predicate-constructors.md) register live semantic obligations; Monty owns boolean and comparison syntax.
[Aggregate descriptors](python-aggregate-constructors.md) register all seven functions with typed reduction witnesses.
Other contextual expression forms and semantic products still need explicit evidence.

## Reconciled coverage boundary

The registered families close dispatch inventories, not the language's
semantic product. The existing `plasm-law` evidence blocks in
[Python conformance](python-conformance.md) remain the authoritative evidence and
gap ledger; this table identifies production seams to discharge those gaps.
There is no new aggregate coverage percentage or duplicate status ledger.

| Remaining seam | Production owner | Evidence already available | Next obligation |
|---|---|---|---|
| Predicates and closed membership | `python/reads.rs`, `membership.rs` | Original predicate/membership matrix and finite equality-filter oracle | Semantic families now have live typed filter witnesses and capture/effect/scalar-cell rejection checks; no production Python operator inventory is maintained. Full operator/type, RHS allowlist, nullability and completeness products remain open. |
| Named projection expressions | `python/projection.rs`, `assembly.rs` | Projection matrix and read-value projection oracle | Field, scalar literal, arithmetic, `len` and conditional expressions now have typed IL witnesses and diagnostic checks. BC-03 `outer_values` adds finite branch/operator/type products and mixed-column composition; captured-input alternatives remain separate obligations. |
| Aggregate descriptors | `python/reductions.rs` | Registered aggregate/group/distinct row methods and matrix cases | Seven-function descriptor inventory and arity/keyword diagnostic negatives are checked. The independent reduction-v1 model covers 48 empty/null/order/group/alias products over nullable integers. Full types, missing fields, partial coverage and nested reduction products remain open. |
| Scoped dynamic operands | `python/body.rs`, `writes.rs`, `inputs.rs` | Recursive literal registry, recursive value family and nested-create model | Separate field, captured collection, singleton and acknowledgement contracts; generate presence/union/coverage/effect combinations. Literal registration does not register these dynamic alternatives. |
| Compute application and interpolation | `python/text.rs`, `interpolation.rs` | Declaration/scalar gates, recursive Monty input witnesses and compute profile | Argument adaptation and owner qualification across source kinds; recursive output contracts have `typed_returns` and structural/effect-gate witnesses; resource-failure products remain separately scoped. |
| Iteration callbacks | `python/iteration.rs`, `fanout.rs` | Registered iterate method and existing stop/exhaustion matrix | Seed/step/stop contextual premises, independent re-observation and retained-effect-prefix traces, cancellation/replay products. |
| Cross-family authority | Binding contracts, relation/capture nodes, runtime identity sidecars | Relation gate and existing authority rejections | Generate value-preserving transformations that must not regain receiver authority; check inferred contracts as well as final values. |

Two restrictions are visible in current admission, not newly diagnosed runtime
bugs: projection arithmetic admits only `+`, `-`, `*`, `/`, and its conditional
test admits one comparison. Closed membership uses a syntactic method allowlist
(`get`, `query`, `select`, `where`, `take`, `order_by`, `union`, `distinct`,
`flat_map`); it does not admit every otherwise valid read operation. Reconcile
these contextual restrictions with normative premises before expanding syntax.
Monty's inner Python profile is a separate boundary from these outer DAG forms.

The row-method gate now uses the shared constructor evidence validator: compiled
IL witnesses distinguish compute operations, scoped record versus row outputs,
iteration payloads and explicit read page budgets. Each negative requires its
intended diagnostic. Registry witnesses share entity selection with the live
matrix, including cursor-based iteration. These checks prove occurrence, not
all payload premises or arbitrary composition semantics.

Next priority: extend independent models across reductions, authority,
presence/coverage and effects; reconcile the remaining dynamic operand and
compute/iteration contextual premises. Each step must preserve original matrix obligations
and leave undischarged `plasm-law` gaps explicit. Adding an enum alone closes no
semantic gap.

## Finite reference algebra

`plasm-e2e/tests/plasm_language_matrix/reference_algebra/model.rs` defines the
independent algebra using only `std` and `serde_json`. It never calls production
schema inference, lowering, materialization or Python evaluation for its expected
results. Fixture observations are inputs to both executions, not expected-output
snapshots copied from the runtime.

`read-value-v3` declares 41 closed-shape trees plus six observed-shape trees,
run over 0, 1 and 3 complete source rows: 141 live differential comparisons. Source rows include duplicate projected
values, explicit null, a negative integer, newline and Unicode text. Constructors
are source, projection, equality filter, order, take, first-wins set union,
synthetic record map and nested record maps with captured parent fields. The
corpus includes two unary layers, union combinations, nine parent/child choices
a deeper nested child composition, and four direct projected-array embeddings. This exhausts that declared corpus, not
all trees up to a depth bound and not the Cartesian product of all constructors.

The runner emits a Python `Program`, compiles and validates its dry witness, then
executes against an isolated HTTP server driven by the abstract language matrix
catalog. It compares exact recursive values, row order and multiplicity, complete
coverage without continuation, and absence of effect acknowledgements. It does
not strip unexpected fields to make results agree. Each comparison gets a fresh
execution context; the server task is aborted when its owner is dropped.

On failure the shrinker removes constructors or shrinks children while preserving
the output type, strictly decreasing tree size and retaining the failure class
(admission, dry, runtime, values, coverage or effects). It stops at a local minimum
or 32 accepted descents and prints corpus version, source cardinality, case index,
original and reduced trees, and reduced Python source. This is deterministic;
there is no random seed or automatic blessing of counterexamples. Catalog/profile
pins remain those of the checked-out test harness; portable persisted failure
bundles are still an obligation.

Reference-model unit tests additionally exercise all 18 combinations of three
cardinalities, three coverage states and continuation presence at materialization;
missing versus null; wrong scalar type; nested empty arrays; parent captures;
bound overflow; and type-preserving shrink steps. These are **oracle tests**, not
live differential evidence for those negative partitions.

Explicit exclusions:

- Types beyond text, integer, nullable fields and recursive records/arrays; domain
  pins, all primitives and unions still need independent value generators.
- Partial/unknown HTTP collections, each continuation channel, unavailable fields,
  and incomplete-input take semantics. The model rejects its unsupported take case.
- Unavailable-field metadata and the full observed-field/coverage product.
  Six observed-shape cases cover absent versus explicit-null scalar fields and
  direct nested read arrays; they do not cover every catalog type or mutation.
- Entity authority, relations, flat-map, aggregates, grouping, compute, mutations,
  failure effect prefixes, concurrency, cancellation and replay.
- Random generation, all invalid constructor premises, full inferred-plan contract
  comparison, durable reproduction bundles and shrinker minimality proofs.

Whole-row operands now apply their declared projection strictly before value
assembly, while identity-bearing field/receiver inputs remain separate. Surface
and relation record schema extraction also respects explicit projections. A
missing projected field fails; it is never synthesized as null. The v2 corpus
covers direct projected arrays with empty, singleton, many and duplicate values.

The separate recursive value family adds nested field/presence cases; broader
presence/coverage products remain open. The finite effect model below covers nested creates. Keep these exclusions visible in the
literate laws until executable evidence discharges each obligation.

## Finite collection/effect extension

The `collection_boundaries` family adds 36 live indexed/offset/cursor products
across empty, singleton, exact-page and page-plus-one inputs at page, compute and
nested-array consumers. `mutation_array_effects` adds 15 nested-create scenarios
against an independent commit/acknowledgement model, including rejection, lost
successful response and invalid response decoding. These are separate finite
models, not new constructors of the shrinking read-value algebra above.

Read budgets now follow collection input proofs and map parents. Complete consumers
must not receive a presentation-page truncation; explicit limits still stop demand
propagation. Pagination consults its driver before declaring an empty page unproven.
HTTP/GraphQL mutation dispatch evidence and host acknowledgements survive failed
Python continuations in async occurrence snapshots. Other effect transports,
resumed public continuations, durable crash recovery and arbitrary constructor
products are not discharged by these families.

## Finite reduction extension

The `reduction_algebra` property adds the independently specified `reduction-v1`
model: 48 live products over six nullable-integer sequences, both identity orders,
global/grouped reduction and direct/aliased fields. Every execution uses all seven
descriptors; groups retain first-seen order. The model imports no production
aggregation code. See [the scoped laws and exclusions](python-conformance.md#finite-reduction-algebra).

Primitive endpoint coverage now spans all 14 CGS FieldType variants: 20 fixture
fields x forward/reverse/empty inputs = 60 programs, including typed Monty
consumption and exact domain/recursive contract checks. This caught and repaired
first/last numeric contract erasure and borrowed money-struct serialization.
The [outer value contract](python-outer-value-contract.md) now defines arithmetic
legality, nullability, domain erasure and checked runtime failures. Its finite
operator/type matrix and BC-03 `outer_values` property are executable evidence;
new domain-specific dimensional or duration algebras require separate laws.
