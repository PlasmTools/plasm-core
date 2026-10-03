# Python language conformance matrix

Nominal Boolean branch decoding has a focused type-bridge witness:
`cargo test -p plasm-agent-core --lib boolean_membership_branches_preserve_materialized_contracts`.
It checks domain retention and nullability for both branches of Boolean identity
and truthiness predicates. `nominal_literal_branch_constraints_preserve_nested_domains`
adds integer and string-carried nominal equality branches. The direct decoder
test `intersection_metadata_is_recursive_and_order_independent` checks constraint
permutations under arrays/records and temporal wire retention. Core
`value_contract::intersection` tests cover a finite algebra basis, recursive
conflict rejection and field presence. The live matrix witness
`python_nominal_boolean_refinement_selects_only_matching_effects` asserts actual
HTTP write recipients after filtering. These do not close the separate
named-callback admission or general conditional-effect composition obligations.

The [collection coverage contract](../../../../doc-site/docs/reference/collection-coverage-contract.md) specifies producer evidence, operator transfer, consumer demands and recovery separation. Its implementation inventory and CE-01–CE-07 obligations remain explicit; the design is not a claim of completed matrix coverage.

The codec ownership witnesses run with `cargo test -p plasm-core --lib collection_codec`.
They use an independent sequence oracle plus non-Clone payloads and pointer/lifetime
checks for ordered selection, concatenation and shared storage. They cover the
kernel part of CE-03/CE-04. Production graph/spill and consumers now use recorded
membership; these kernel tests alone do not count as live matrix completion.
The ordered-acquisition oracle is included in that command. Production query-index
scope, empty/duplicate/order preservation, cache disagreement and merge laws run
with `cargo test -p plasm-runtime --lib query_index`. These storage witnesses do
not alone close CE obligations for paginated result carriers, graph spill or DAG consumers.

The carrier witnesses are `plasm-runtime::execution::collection` (ordered nested
fanout, delivery windows and shared payloads), `plasm-agent-core::graph_rehydrate`
(recorded order, duplicate occurrences and missing identities), and
`plasm-agent-core::run_artifacts` (typed checkpoint storage and substitution rejection).
The existing `collection_boundaries` Python matrix remains the live producer-to-consumer
gate; it is not replaced by the codec property suite.


`python_coverage.json` is a checked ledger over the original matrix, not a second
feature universe. All 162 original rows require executable Python evidence.
Pending and partial entries fail the gate. Supplemental cases cannot substitute
for an original obligation.

This is original-obligation coverage, not a complete recursive language claim.
The [scoped composition contract](../../../../doc-site/docs/reference/python-scoped-composition.md)
contains the canonical SC-01 through SC-12 `plasm-law` evidence blocks beside the
normative prose. The matrix parses that document directly; the former separate
JSON law ledger has been removed. The [conformance design](../../../../doc-site/docs/reference/python-conformance.md)
adds BC-01 through BC-04 for lowering and value boundaries, with an explicit
constructor/contract scope and remaining test-system work. Status is derived from
registered evidence and explicit gaps, never a hand-edited covered label.

Inventory validation, generated properties and live execution are separate gates.
Original-row totals do not absorb composition or boundary gaps. Registered test
links are not evidence that those tests passed in a particular run.

Quantified predicate witnesses distinguish the fixture's `complete_lines` relation
(explicit exhaustive materialization evidence) from `lines` (unknown coverage).
`python_quantified_predicate_live_values` checks Boolean reductions, filtered and
nested generators, and empty arrays; `python_quantified_predicate_rejects_unproven_collection`
checks that both quantifiers reject unknown relation coverage. The constructor/position cross-product is exercised separately by
`python_predicate_value_position_closure`. Live relation-position, lazy branch and
iteration-expression tests cover composed predicates, skipped reads, captured
scalars, and zero-step, exact-bound and exhausted iteration. These checks cover
registered constructors and typed consumers, not arbitrary Python.

```sh
cargo test -p plasm-e2e --test plasm_language_matrix --test plasm_language_matrix_views
```

Use the pinned Monty worker (`PLASM_MONTY_BINARY`) and the repository's debug-test
stack setting (`RUST_MIN_STACK=16777216`). For inventory only, filter the first
suite to `python_coverage -- --nocapture`; that is not live execution evidence.

## Evidence contract

The runner compiles Python programs against fresh deterministic fixture HTTP
servers. It applies the original planning/live assertions directly. Supplemental
programs have independent fixture-derived value, order and effect-count assertions
in `python_expectations.rs`; there is no comparison compiler or snapshot blessing.
The old matrix program strings, synthesis module and duplicate live runner have
been removed. Row IDs and all feature obligations remain in the checked ledger.

The exact Python case inventory is checked in `python_coverage.json` and printed
by the coverage test; it covers all 162 original rows plus supplemental cases. The two
non-row obligations are exercised by `python_host_contract`: production computation
witnesses and actual HTTP wait/cancel behavior. The view suite also compiles only
Python and retains catalog validation, scope, computed-output and relation checks.
Stateful observation tests submit Python through both execution drivers and assert
request ordering, partial outcomes, cancellation and post-write observation.

The first grammar-directed property family runs 35 Get identity spelling
comparisons and 30 invalid-call checks across root, projection, bound read, child
and grandchild contexts. These are canonical-plan/admission checks, not live
execution, general recursive closure or an independent reference interpreter.
Run with `cargo test -p plasm-e2e --test plasm_language_matrix registered_lowering_properties -- --nocapture`.
The full suite includes these checks automatically. The independent `read-value-v3`
algebra adds 141 live differential executions (41 closed and six observed trees over three source
cardinalities), with a type- and failure-stage-preserving shrinker. It covers the
explicit finite read/synthetic-value fragment in the
[constructor audit](../../../../doc-site/docs/reference/python-constructor-audit.md);
two separate finite families add 36 collection-boundary and 33 nested mutation-array
scenarios. They compare pagination coordinates, nested values and independent server
commit/receipt evidence, including uncertain outcomes. Mutation faults cover every
position in the declared nested sequences; system failures stop further scoped
admission, retain the typed cause, and preserve all earlier acknowledgements. The full type/presence/effect
product, resumed public continuations and generalized generation remain open. Oracle unit checks of negative presence/coverage partitions do not
count as live differential coverage. The literate BC-01/BC-03 blocks link this
family without erasing those gaps.

The ledger rejects missing/stale rows, changed feature tags, duplicate or dangling
case links, incomplete statuses, absent evidence and changed view/non-row
inventories. Tagged-union tests remain a separately checked supplemental suite.

## Explicit syntax replacements

- Python multiline strings replace heredoc delimiters; Python cases retain text,
  lazy rendering dependencies, reuse, cardinality and failure obligations.
- Typed `@compute` replaces Jinja rendering. Undefined fields and plural scalar
  extraction reject at admission; explicit Python indexing retains runtime errors.
- Qualified Python fields resolve the native binding-name collision. The Python
  test checks the explicitly qualified result.
- `Program.build` returns explicitly. The result-root obligations remain,
  without retaining historical last-binding coercion as a source-language feature.
- The native `singleton()` query hint does not truncate rows. Its `.take(5)` replacement
  retains those rows; Python grants scalar/receiver authority only to proven
  singletons. Dedicated take-one and empty-singleton rows prove that contract.
- Host `wait(oN)` and `cancel(oN)` are complete transport commands, not DAG source.
  Trailing native/Python statements, extra arguments and malformed handles reject.

## Extending semantics

Extend the original fixture/row first when necessary, add its linked Python case,
and run both whole suites before claiming coverage. Refactor frontend-specific
assertions into shared semantic assertions only when equivalent behavior is
explicitly proved. Do not omit original errors or live-result checks.

This gate proves the original matrix obligations applicable to Python. Compiler
unit tests, adversarial async/effect tests, teaching delivery, packaged-worker
smokes and deployment checks remain additional gates. It is not a live vendor or
LLM benchmark.

## Embedded collection proof extension

The `relation_from_parent_get` family now has an additional host-boundary gate:
`cargo test -p plasm-node --lib --features napi/dyn-symbols native_boundary_hydration_repeated_live`
with the pinned worker. Its abstract `hydration_boundary_matrix` fixture checks
explicit exhaustive collection metadata through Get, hydration, aggregate and
Monty compute at 2 MiB, plus empty/missing/unproven relations. Agent-core
`relation_coverage` tests protect partial/unknown parents, paging and truncation;
compiler `exhaustive_embed_rejects_missing_wildcard_suffix_before_cache_normalization`
protects malformed paths. These are additional obligations, not replacements for
the original obligation rows or evidence of vendor catalog completeness.

## View relation collection proof

`cargo test -p plasm-e2e --test plasm_language_matrix_views matrix_views_relation_collection_compute_coverage`
(with the pinned `PLASM_MONTY_BINARY`) checks Get- and Query-rooted complete view relations through
collection compute, directly and through filter/order/take/projection. It checks
exact values and order, empty view outputs and empty filtered outputs, complete
coverage and absence of continuations. The abstract views fixture is the input;
no vendor catalog is loaded.

Agent-core `relation_coverage` checks that view reference extraction preserves
Partial/Unknown upstream evidence, rejects missing parent/relationship evidence
and wrong target identities, preserves duplicate occurrences, and detects
truncation. It also exercises the unchanged collection-compute admission guard.
This is a cross-boundary witness for SC-07, not a claim of all view/transform
products or a completed AppWorld evaluation.

## Correlated composition extension

`cargo test -p plasm-agent-core --lib map_body::tests` extends the row-transform,
membership, recursive-value, render-to-write and effect-order obligations with
the abstract `python_dag_slice` fixture. A filtered, membership-selected,
deduplicated and sorted entity source feeds a bounded map; its typed record
output feeds Monty text compute and a captured HTTP write body. Empty parents
produce an empty string that reaches the write unchanged. Cancellation and bound
exhaustion prevent the downstream write; projected-away capture fields and
synthetic receiver authority reject before I/O. Existing scoped occurrence,
partial-child, commit-tampering and nested-value checks remain in this gate.

Map bodies are ordinary DAG nodes, including dependency and effect-order edges.
Their outputs remain synthetic records. Unions retain value provenance but do not grant entity receiver authority (RA-14). The recursive extension below now admits
nested maps and effects. Both original Python suites remain required.

## Conditional compute extension

`text_conditional_membership` verifies fixture output from Python conditional
rendering through the original live matrix. Supplementary `conditional` tests in
agent-core cover both-branch dependency admission, mixed scalar branch types,
nullable membership rejection, lazy execution, and standard CSV quoting for
commas, quotes, CR/LF, Unicode and empty values. Correlated output reaches a
write unchanged. These witnesses now run through upstream admission; they no
longer define a Plasm-owned Python expression subset.

## Normative upstream Python boundary

The [Python compute contract](../../../../doc-site/docs/reference/python-compute-contract.md)
is the normative language boundary. Its implementation ledger must be maintained
alongside this matrix. The local inference checker has been removed. Production
compilation asynchronously admits all compute nodes through the pinned pool,
including nested map bodies; execution readmits stored artifacts before IO.

The [profile ledger](../../../../doc-site/docs/reference/python-compute-profile.md)
classifies syntax, modules and effects. Supplementary gates are:

- `python_compute::upstream`: nullable/union narrowing and syntax-family witnesses;
- `python_compute::schema`: recursive schema materialization and projected fields;
- `python_pool`: typed codec, worker isolation, cancellation and admission without invocation;
- `python_compute_feasibility`: primitive text forms, enum contracts, local copies;
- `map_body::tests::python_lowering::upstream_admission_rejects_later_compute_before_any_write_and_on_replay`:
  invalid later nodes and mismatched profile artifacts cause zero backend calls.

These supplement the original matrix obligations; they do not replace
rowset, cardinality, identity, completeness or effect-order witnesses. General
non-string return contracts and explicitly unverified profile classifications
remain separate obligations, not silent support claims.


## Live composition gaps discovered 2026-09-25

The four authored AppWorld probes documented in the parent repository at
`scripts/appworld/python-development/BROADENING.md` expose additional composition
obligations. The 162 original rows remain green; that does **not** imply arbitrary
combinations of their operators work. The following inventory separates implemented slices from remaining obligations:

- G1 implemented for captured catalog rows and narrowing projections: current-row
  typed compute inside map retains source fields alongside its scalar output.
  `assembly_row_compute` is a Python live witness; five assembly tests in the
  map-body suite additionally cover recursive values, failures and replay.
- G2 implemented for direct named singleton-field captures in projection:
  `assembly_singleton_capture` asserts typed object derivation and captured values.
  Recursive values and empty/plural captures have additional map-body tests.
- G3 implemented for named proven singleton receivers. The captured receiver and
  source row are distinct dependencies; stateful tests cover failures and cancellation.
  Bind a Get before fanout; an inline receiver-building expression is not admitted.
- G4 implemented: bounded nested scopes preserve typed ancestor inputs, computed
  write arguments, effect ordering/receipts and full occurrence addresses.
  `scoped_nested_records` and `scoped_nested_effects` are live matrix witnesses;
  `map_body::tests::nested` adds empty, three-level, compute, failure, cancellation
  and replay-tampering coverage. This does not close every SC law.
- G5: exhaustive collection evidence through relation + union + membership + map
  + compute composition, including unknown/partial/missing/empty cases. Vendor
  declarations need independently verified API evidence before promotion.

Before implementing each extension, add an abstract fixture and executable
semantic obligation to the corresponding original family; preserve live values,
coverage, ownership, ordering, cancellation and partial-write evidence. The live
programs and error receipts are reproductions, not passing matrix witnesses.

`scoped_flat_map_format` covers nested rowset composition with captured Python
formatting in the original live matrix. The fanout observation-boundary tests
retain partial failures, cancellation, write/read barriers and invocation receipts.

Simple Get argument binding accepts either `get(value)` or `get(identity=value)`.
Keyword witnesses are linked to the existing boolean and bound-identity obligations
(including empty singleton failure) in `python_coverage.json`. The identity admission
and review-seal test checks equivalent plans for both spellings across literal and
bound identities, and rejects missing, duplicate, unknown and invalid arguments.
Compound named-key and nullary Get contracts remain distinct.

## Constructor and recursive-value gates

Production deferred row dispatch uses `PythonRowOperation`; the literate
[constructor registry](../../../../doc-site/docs/reference/python-row-constructors.md)
requires exactly one rule and positive/negative evidence per registered operation.
Its negative programs execute during the matrix run. Build statements and catalog
calls have their own gates below. Outer scalar DAG nodes are also registered;
recursive literal operands have a separate gate below.

`recursive_value_boundary` adds 80 fixed-depth domain/presence cases through
fixture HTTP and real Monty. The family compares exact nested results and Python
observations, rejects absent demanded fields, and preserves observed absence.
Union generation and arbitrary recursive products remain separate gaps.


The production build-statement inventory is now gated by
`python-build-constructors.md`: four constructors, positive live matrix witnesses,
and thirteen rejecting programs with specific diagnostics. The registry tests
reject omissions, duplicate rules, missing negatives and witnesses whose AST does
not contain the constructor. `root_build_statements` retains an unreturned write
and verifies its completed receipt alongside exact returned data; `parallel`
retains ordered tuple-return evidence. The seven catalog capability kinds are registered in
[the catalog constructor contract](../../../../doc-site/docs/reference/python-catalog-constructors.md).
The gate checks resolved typed IL against existing live witnesses and exact
negative diagnostics; a query witness cannot discharge search.
The [relation constructor gate](../../../../doc-site/docs/reference/python-relation-constructors.md)
checks root and correlated navigation with one/many targets, scoped binding proofs
and continuation-authority rejections.
The [scalar DAG gate](../../../../doc-site/docs/reference/python-scalar-constructors.md)
registers literal text, captured formatting and singleton field extraction. It
shares inventory/evidence validation with the catalog and literal gates.
The [literal operand gate](../../../../doc-site/docs/reference/python-literal-constructors.md)
registers seven shared constructors. `operand_recursive_literals` compares exact
nested live values and typed IL, including integer precision and null preservation.
The [declaration gate](../../../../doc-site/docs/reference/python-declaration-constructors.md)
registers root Program, documentation, build and compute declarations with 38
full-module rejections and live witnesses for four compute-input annotations.
Class documentation erasure preserves the plan. Remaining contextual rules and
semantic products still need explicit evidence.

Named projection expression registration is checked against the production inventory
and recursive typed IL; see [projection rules](../../../../doc-site/docs/reference/python-projection-constructors.md). Existing live projection rows remain the semantic witnesses. Operator/type products and captured-input projection are not certified by registration.

Outer predicate rules link existing boolean, scalar, containment and membership
live cases to typed filter evidence: [predicate constructors](../../../../doc-site/docs/reference/python-predicate-constructors.md). These links do not certify every nullable/type/completeness product.

[Aggregate descriptors](../../../../doc-site/docs/reference/python-aggregate-constructors.md)
require all seven functions in typed aggregate/group IL plus argument-diagnostic
rejections. The separate reduction_algebra property covers 48 finite nullable-integer, empty, order, grouping and alias products; broader domains and compositions remain open.

### Constructed value records

`record_value_*` cases exercise outer dictionary construction through compile,
review and live execution: singleton fields, independent dependencies, nested
records/arrays, exact primitive literals, empty collections, scalar bindings,
record reuse, projection, compute consumption and empty singleton failure.
The invalid-semantics suite rejects plural scalar extraction, unavailable fields
and attempts to use constructed records as entity receivers. These obligations
are additional Python value-surface coverage, not historical syntax aliases.

### Shared recursive outer values

**Open gap — Monty expression ownership:** the cases in this section do not prove
Python-equivalent scalar semantics. Outer `and`/`or` currently require booleans,
and scalar method calls may enter rowset dispatch. The nullable string expression
`"needle" in (row.text or "").lower()` must be admitted with Monty semantics in
all value-consuming positions. The direct Monty profile witness
`profile_syntax_families_check_and_execute` now includes operand selection,
nullable coalescing, chained calls and skipped exceptions; it does not close the
outer-lowering obligation. Migration must cross expression families with consuming
positions and retain domain, cardinality, field-presence and effect checks.

The host-side `python_compute::analysis_tests::structured_analysis_*` witnesses exercise
non-executing in-process upstream inference, including nullable
method composition and independent nominal/presence/null evidence. These are
analysis-boundary tests, not outer matrix coverage: no consuming-position
obligation is discharged until that position compiles and executes through the
new elaborator. The outer compiler still uses its current lowering path.


`value_recursive_*` exercises literal/scalar bindings, aliases, nested arithmetic,
records/arrays, conditional/boolean expressions, length, membership, inline
singleton fields, named projections, read arguments and inline versus bound write
arguments. Empty records are exercised both at the root and in bounded maps.
`python_recursive_values_keep_lazy_effect_and_authority_boundaries` rejects hidden
lazy calls, plural extraction, invalid comparison kinds
and forged receivers. `outer_values` now checks incompatible destination types, overflow and zero division before
writes in three consuming contexts: projection, record field and direct argument.
The projection registry inspects shared recursive value IL, not `ComputeOp::With`.

The separate value-boundary closure witnesses below cover scalar compute
annotations, structural compute inputs and nested field access.

### Value-boundary closure

`python_value_closure_matrix` executes 21 `value_closure_*` witnesses through
production admission, dry planning and live Hermit/Monty: scalar and array inputs,
per-row and collection adaptation (including empty collections), structural and
empty records, constant/bound formatting, and nested fields in projections and
read/write arguments. `python_value_closure_rejects_ambiguous_and_forged_inputs`
rejects ambiguous/type-incompatible inputs, missing fields, forged receivers and
modified mode/version/catalog/owner seals. The original flat-map scalar rejection
remains a cardinality boundary. Core field tests distinguish absence from null
and preserve record-union types; catalog-label flow tests retain nested provenance.
Scalar adaptation is deliberately absent from agent teaching.

`scalar_compute_preserves_all_fixture_domains` also round-trips every field of
`python_value_contract` through both direct field extraction and one-column
projection, preserving named domains, nullability, opaque references, money
metadata and nested arrays. The primitive inventory remains exhaustive in
`primitive_reductions`. `scalar_blob_roundtrip_preserves_value_boundary` isolates
direct versus projected blob input. Scalar extraction is an explicit IL derive
kind, preserved across plan/comp conversion and shared dry/live materialization;
an object-valued cell is never expanded based on its payload shape.

### Typed datetime and retired temporal syntax

The `datetime` matrix uses Python source through production admission, dry witness,
and live Monty execution. It covers constructors, import aliases, leap-day and
negative-duration arithmetic, offset comparisons, formatting, typed compute
returns, captured and nested quantifiers, catalog field projection, mutation wire
encoding, and bounded iteration with temporal stop predicates.

Core `temporal_value` tests cover kind separation, explicit Unix units, invalid
calendar values, naive instants, range and precision rejection, and unrepresentable
RFC3339 offsets. `temporal_input` witnesses recursive arrays/records and remapped
union transport fields. Pool tests cover typed nested materialization and explicit
clock configuration. Prompt tests require date/datetime declarations without an
evaluation clock row or `str | int` erasure.

Historical relative temporal aliases are retired, not missing Python coverage:
`temporal` tests require rejection of `now`, `today`, `7d ago`, `now-1h`, and numeric
strings used as inferred timestamps. Their replacement is datetime computation.
The pinned interpreter's unsupported IANA zone rules and datetime fold semantics
remain outside the admitted temporal contract; full CPython is not claimed.

`primitive_reductions::temporal_ordering_contract_end_to_end` exercises typed
`order_by`, projection, global/grouped extrema and positional reductions feeding
Monty. `unorderable_reductions_reject_before_execution` locks static domain rejection.
Core `value_order` properties check offset invariance and numeric transitivity;
runtime `row_compute` properties check sort/extrema agreement and borrowed row
round-trips. These witnesses run on the direct value engine, with no Polars path.
`primitive_reductions::temporal_equivalence_contract_end_to_end` covers grouping
and distinct over direct and nested temporal keys. Core equality properties lock
hash congruence, offset equivalence, presence and ambiguous temporal unions.
`top_k::laws` compares the bounded heap with stable full sort/take over generated
nulls, ties, direction and bounds, plus typed temporal and malformed-value cases.
The live `projection_alias_topk_uses_source_contract` witness and host projection
pushdown law test ensure alias names resolve back to catalog source fields.
Arithmetic capability tests cross every numeric/money/non-arithmetic union pair;
runtime tests retain declared results on empty inputs. Naive/aware mixed live
cross-products remain a separate temporal fixture expansion. Dynamic
keyword/higher-order construction also lacks a fold-preservation guarantee under
the pinned interpreter. See `docs/research/python-datetime-validation.md` in the
parent repository for measured commands and limitations.

### Null predicates and branch-local type facts

Python None tests retain Python semantics through Monty analysis and execution.
The recursive predicate-position product includes null tests across root,
record, projection, map, filter, iteration and mutation-argument admission.

The datetime suite crosses nullable computed input with both present/null runtime
values, short-circuit conjunction/disjunction, negation, both conditional arms,
chained comparisons, temporal comparison/arithmetic/properties/methods, recursive
records/arrays and scoped consumers. Its iteration witness covers a nullable
deadline, zero writes when absent, and two writes followed by re-observation when
present. Negative witnesses reject unguarded access, wrong-branch proofs,
disjunctive over-refinement and proof leakage after a merge.

Core `Refine` coverage locks recursive/domain/presence preservation and runtime
rejection of a false refinement. `monty-analysis` branch tests cover both
successors, negation, conjunction, `isinstance` and bounded expression selection.
Plasm maps that evidence to captured references rather than implementing a second
Python fact algebra.

Separate null-test filters transfer their validated predicate facts to downstream
row schemas. Live witnesses split `where` and `.year` projection, exercise both
present and null rows, and check retained projected values. Negative witnesses
reject null-selecting filters and disjunctions that do not establish non-nullness.
Temporal quantifier captures reuse one port for repeated references to a parent
field, preserving Monty's own branch narrowing. Immutable aliases share branch
facts through their canonical binding identity.


## Catalog identity and datetime response boundary

- `python_union_preserves_catalog_rows_in_correlated_reads`: union followed by
  correlated catalog argument capture, including projected values; abstract
  `python_dag_slice` fixture and real occurrence execution.
- `datetime_naive_response_is_preserved_through_projection_and_compute`: HTTP
  timezone-free datetime -> typed projection -> Monty datetime arithmetic,
  preserving microseconds and `tzinfo is None` (`python_value_contract`).
- `response_temporal_contract_distinguishes_missing_null_and_invalid`: decoder
  rejects malformed and wrong-awareness present values; missing fields stay
  missing and null stays null.
- `naive_datetime_wire_round_trips_without_inventing_or_discarding_timezone`:
  wire/profile law, rejects timezone erasure and precision loss.

These obligations cover the semantic boundaries independently of AppWorld task IDs.
- `python_union_catalog_rows_preserve_effect_arguments`: same-owner union retains
  row argument correspondence and deduplicates effects.
- `python_union_preserves_common_entity_receiver_authority`: both arms must carry the same catalog/entity authority; projection, union, relation and effect execution retain it. Cross-catalog unions remain terminal.
- `python_compute_infers_structural_return_at_public_admission`: typed inputs and a single return expression infer record output; multi-statement inference remains rejected.
- `dag_compute_hidden_cell_defers_result_and_preserves_effect_receipt`: bounded
  delivery never advertises hidden cells as inline values or leaks them through
  metadata; completed effects remain observable.

- `hydrate_propagates_response_contract_errors` and the existing unavailable-detail
  regression distinguish invalid data from unavailable reads.
- `datetime_layout_requires_an_explicit_encoding_and_preserves_naive_calendar_time`
  and the aware formatter round-trip test cover the CML transport boundary.

### Paginated row coverage / hydration (RA-19)

The runtime witness family `execution::query_stream::tests` uses the abstract
`plasm_pagination_matrix` fixture and real query decoding, pagination and hydration.
It checks exact list/detail request counts across a 20-row page plus a final row:
complete rows require zero detail GETs; explicit summaries and missing observed
fields still hydrate; a fresh complete list replaces stale post-write observations.
Run `cargo test -p plasm-runtime --lib query_stream::tests`. This locks a runtime
law independent of Python spelling or any production catalog.

### Direct compute results

`typed_returns::compute_values_have_no_reserved_content_accessor` checks dry/live
composition of scalar, array, null and record results. `content` is an ordinary
method, local, record field and argument name; strings have no such attribute.
The historical render-content obligations now use direct typed compute values.
Host regression `python_content_is_an_ordinary_record_field` additionally rejects
serialized output-contract tampering before execution. Contract version 9 seals
the checked output type; there is no compatibility accessor.

### Catalog-label flow across Python values

RA-9 uses the shared information-flow policy, not spelling-based `value_ref`
restrictions. Python value inference does not grant label clearance. The agent-core
fixture-backed witness
`python_expressions_preserve_arbitrary_catalog_labels_to_sinks` compiles direct
fields, transformed strings, nested records/arrays and mutation fanout, then checks
both arbitrary source labels against an active sink policy. Inactive-policy
admission is checked separately. This is a plan-policy witness; it does not claim
additional live matrix execution coverage.

### Open re-evaluation gate after upstream expression integration

The 2026-09-29 full run is not a green baseline: language matrix 79 passed,
28 failed, 1 ignored; views 14 passed, 2 failed. All five typed-return tests pass,
including direct/projected blob cells and serialized IL round trips. Test-function
counts are not counts of independent defects; registry witnesses repeat shared
language obligations.

Close these groups before freezing a new evaluation baseline:

- **Domain and nullable operators:** Money arithmetic and nullable comparisons,
  membership and addition, exercised by `outer_value_transfer_boundary`,
  `python_iteration_admission_and_review_seal`, `python_value_closure_matrix`
  and predicate/projection registry witnesses. Preserve exact domain contracts;
  reconcile null semantics with the language law rather than weakening types.
- **Temporal inference and narrowing:** datetime equality currently yields
  unresolved type evidence; chained non-null guards lose comparison admission.
  Witnesses: `datetime_outer_expression_matrix` and
  `datetime_nullable_guards_preserve_narrowing`.
- **Scoped row shape and view authority:** singleton lambda capture and chained
  synthetic row operations are repaired; filtered view provenance is preserved
  by both lowering and plan validation. The cross-filter refinement witness now
  passes using Monty evidence mapped to captured row fields, including recursive
  records and union selection. No datetime-specific admission exception is used.
- **Evidence migration:** constructor inventories and equivalent-plan assertions
  still encode the removed local evaluator; some negative tests reject ordinary
  Python truthiness or `str(123)`. The view assertion has been migrated from the
  removed synthetic `content` column to the scalar result cell. Update these only against the current language
  contract, retaining runtime-value, cardinality, authority and effect witnesses.

After conformance closes, rebuild the deployed adapter/worker artifacts and run
production-teaching integration smokes before the AppWorld ladder. A successful
`cargo check` of the Node/evaluator crates does not refresh the loaded NAPI binary.

### Exact-money library cutover (2026-09-29)

Python money arithmetic is explicit: `money_add`, `money_sub`, `money_mul`,
`money_div`. The latter two take integer or exact decimal-string factors.
`typed_returns::exact_money_library_outer_and_authored_compute` covers inferred
outer expressions and authored compute through compilation and live execution,
checking native Money output and currency. Agent-core
`exact_money_functions_have_static_contracts` and
`exact_money_functions_execute_in_monty` cover static contracts, exact fractions,
nested calls, keywords, currency mismatch, zero division, malformed decimals and
float rejection. The scalar carrier remains a mapping; operators are not money
operations. This does not close the previously listed unrelated conformance gaps.

Focused verification after the library change: Python agent-core regressions
89 passed / 1 ignored; typed-return matrix 6 passed; complete view matrix 16
passed. At that checkpoint the nullable cross-filter witness remained red; the
branch-evidence change below closes it. These focused
results do not replace the full language-matrix baseline or establish eval readiness.


### General branch evidence (2026-09-29)

The extension exposes `analyze_branches`: typed lexical bindings, a predicate and
observed subjects produce true/false type roots from the upstream checker.
Filter schema transfer and lazy scoped values share this API. The old predicate
pattern matchers and `RequireNonNull` operation are removed; `Refine` intersects
evidence with the original contract and validates the unchanged runtime value.

Measured witnesses: extension analysis tests 16 passed; the original scoped and
recursive nullable witness passed all 20 programs, and its negative witness passed
all 8 rejected programs. The new live matrix witness covers nullable integer,
nullable string, `isinstance` union selection and nested record fields. These
results do not close the unrelated temporal inference and matrix migration
obligations above or establish AppWorld evaluation readiness.


Final focused validation for this change:

- `cargo test -p monty-analysis --test analysis`: 16 passed.
- `cargo test -p plasm-core --lib refinement`: 3 passed.
- `cargo test -p plasm-agent-core --lib python`: 92 passed, 1 manual viewer ignored.
- `cargo test -p plasm-e2e --test plasm_language_matrix_views`: 16 passed.
- Language matrix filters `branch_refinement`,
  `datetime_nullable_guards_cover_scoped_and_recursive_consumers`, and
  `datetime_nullable_guards_reject_unproved_or_leaked_facts`: 4 passed. They
  exercise 32 accepted live programs and 10 rejected programs, including lazy
  compute scopes, both union successors, Boolean short-circuiting and lazy values
  nested inside arrays.

The lazy path identifies deferred dependencies before lowering successors;
there is no speculative unguarded admission. Boolean materialization retains the
original predicate for static evidence without repeating runtime evaluation.
Literal exclusions project to a sound enclosing contract rather than causing a
spurious admission failure. No AppWorld evaluation was run for this change.

### Monty expression ownership

The scalar, predicate, projection and literal ledgers bind semantic families to
live witnesses and invalid programs. They no longer depend on production enums
that pretend to enumerate Python syntax or require Python expressions to lower to
the retired predicate/scalar IL. DAG constructors still have structural gates.

Additional witnesses cover chained nullable comparisons, inherited factory
return types, truthy quantifiers, and exact money comparisons. Optional fixture
fields are explicitly guarded or coalesced in arithmetic/ordering witnesses;
mixed equality and Python truthiness are not negative language cases.

## Input-boundary regression witnesses (2026-09-29)

- `datetime_query_consumers_encode_named_temporal_domains`: actual HTTP query decoding for date, aware timestamp, naive timestamp and epoch milliseconds. Internal temporal markers cannot cross the boundary.
- `temporal_input_encodes_each_parameter_lane_recursively` (plasm-core): scope, selection and controls recurse through record arrays and preserve null. Existing invocation tests cover records, arrays and remapped unions.
- `python_membership_captures_projected_fanout_rows`: union, non-null predicate, bounded search fanout, projected rowset capture and negative membership, in single-catalog and federated sessions. This passing witness does not by itself diagnose the AppWorld captured-input failure.
- `complete_collection_demand_crosses_scoped_outputs`: page-index, offset and cursor transports crossed with empty/nonempty results, direct/nested scoped returns, captured collections and bounded child reads. Checks exact rows, coverage and HTTP page sequences; explicit takes remain bounded.
- `rowset_identity_scope_returns_only_admitted_ports` (plasm-core): operation-free rowset bodies may return declared parent/capture ports; undeclared outer names and synthetic-record promotion fail. The captured-fanout pagination witness exercises this through Python admission and live execution.


#### Write recovery contracts

- Opaque service errors never authorize a retry or classify an effect: `cargo test -p plasm-e2e --test write_contract` varies error text independently of committed state and requires a fresh, complete identity observation.
- Already-satisfied targets and repeated target identities: declared existence `skip_write` performs authoritative reads with zero write dispatches (`declared_state_preflight_skips_already_satisfied_targets`).
- Execution-local failure: `python_source_failed_write_stops_later_effects_without_replay` checks stop-at-failure and independent dispatch of a subsequent execution in the same session, through both runtime and Python-host paths.
- Cancellation preserves committed writes and stops the execution suffix: `python_source_cancellation_keeps_committed_writes_and_stops_later_effects`.
- Agent boundary: `packages/plasm-agent/scripts/test-write-reconciliation.ts` verifies visible diagnostics/receipts and new reads/writes after partial failures, lost responses and terminal failures. Identical program text in a new execution is not inferred replay.
- No session-wide write gate or cross-execution deduplication. Catalog-declared within-execution preconditions retain their existing semantics; service messages never supply execution authority.


### Embedded membership normalization boundary

`hydration_boundary_matrix` includes an exhaustive `Folder.wire_notes` relation
with `payload.items[*].id` on the wire and `Note.note_id` in the semantic row.
The agent-core `membership_evidence_*` regressions cover decoder → semantic
capture → typed storage, nested Python filters on cold/warm execution, and hot /
spilled entity walking. They distinguish unproven membership from exhaustive
empty membership, conserve duplicate occurrences, reject malformed branches,
and preserve upstream uncertainty / detect dropped occurrences. Runtime cache
regressions cover exhaustive replacement and unproven overwrite. These are
abstract fixture witnesses, not production-catalog assertions.

The `observation_boundary_matrix` bounded-iteration witness also requires exhaustive nested membership and duplicate occurrences to survive zero-step and post-write exits through both execution drivers (`python_iteration_stateful_contract_both_drivers`). Snapshot compaction separately checks compound identities with identical display strings.

### Ordered mutation failure frontier

Mutation occurrences follow source order. The first failure stops dispatch of the
remaining suffix; preflight is not dispatch admission. Completed prefix effects
remain evidence, and a failed dispatch does not prove rollback. Read concurrency
is independent of this law.

Verified witnesses: `ordered_effects_leave_the_failed_occurrence_suffix_undispatched`
checks each failure position; `oph_mutating_fanout_preserves_effects_on_system_failure`
checks the executable fanout against an abstract stateful transport;
`python_fanout_partial_failure_keeps_row_outcomes_and_actual_state` checks Python
scoped maps. `python_nested_fanout_failure_stops_inner_and_outer_suffixes`
checks a completed inner write followed by failure, with both suffixes undispatched,
through direct and Python-host execution. These execution witnesses do not establish host recovery or suffix
resumption; that requires a separate end-to-end continuation witness.

### Python truth consumers and exception repair

`predicate_truth_*` cover string, operand-selecting conjunction, empty string, null, iteration stop and relation quantification through live matrix execution. Pool tests preserve typed user-code exceptions as program repair while lifecycle failures remain runtime failures. Effect evidence still dominates recovery after writes; diagnostic prose grants no authority.

`repair_runtime_projection` and `repair_runtime_compute` execute real Monty failures
through the matrix and assert `FailureCause::Program`, `RepairProgram`, the stable
`python_exception` code, and absence of effect evidence separately from private
diagnostic text. `predicate_truth_refinement` verifies that truth conversion retains
the non-null proof consumed by a subsequent projection.

## Prefix and record-access regression slice

`prefix_and_record_access_regressions` executes serial prefix composition, zero rows,
zero count, empty scalar extraction failure, and nested string-key record access.
`plan_read_bounds` independently checks generated prefix composition, sibling demand
and selection/order barriers. The runtime zero-limit test verifies schema preservation;
record inference/codec tests preserve nominal types, exact integers and absence versus null.
These are supplemental witnesses registered in `python_coverage.json`, not replacement
claims for the original obligations or arbitrary Python mapping support.

### Federated scoped record materialization

`cargo test -p plasm-agent-core --lib map_record_preserves_foreign_nested_value_domains`
locks the runtime map-output boundary for nested arrays of records carrying a
foreign catalog's nominal scalar domain. The enclosing session registry is borrowed;
parent dispatch scoping must not remove value-domain resolution. The witness also
rejects absent catalogs, mismatched catalog pins, and invalid primitive values.
This boundary regression does not claim a new full AppWorld task pass.

### Entity-record annotation equivalence

`row_entity_annotation_matches_value_contract_recursively` checks `Row[eN]` and
`Value[eN]` produce identical contracts and Monty definitions for records, lists,
nullable unions and nested lists; unknown symbols remain rejected.
`python_row_entity_annotation_runs_without_granting_authority` exercises a Python
Program through compilation and execution, and rejects relation traversal from
its computed records. Canonical teaching remains unchanged.

### Scoped callback laws

The [callback contract](../../../../doc-site/docs/reference/python-callback-contract.md)
defines declaration, lexical capture, occurrence, sequencing, branching, return and
authority obligations. `callbacks` runs the registered callback value witnesses,
independent HTTP effect dispatch checks (including fail-stop and empty input),
admission negatives and existing lambda regressions. The production build
constructor inventory includes callback declaration and rejects missing rule evidence.
Unsupported forms and conservative refinement boundaries remain explicit in the
contract; these witnesses do not claim arbitrary Python function support.


Boundary cutover witnesses (2026-10-02):
- `python_compute_dictionary_and_multiple_inputs_live`: mutated dictionaries,
  independent collections, the same source at two cardinalities, downstream dictionary
  indexing, local reassignment, positional-only typed callback parameters, nested
  comprehension shadowing, serialized artifact roundtrip and native execution.
- `python_nominal_boolean_refinement_selects_only_matching_effects`: both filtered
  rows and conditional effect expressions, with independently observed HTTP writes.
- Upstream scope-index, annotation, whole-body return and closed-host-interaction
  tests live beside `python_compute`; these do not establish complete outer-builder
  language coverage.

Compute-input inference (2026-10-03): `compute_inferred_callsite_inputs` runs a
query collection and a proven-singleton Get through unannotated `@compute`
parameters. `python_compute_infers_materialized_inputs_at_each_call` also checks
independent named inputs, scalar fields, explicit cardinality mismatch and the
pure-compute write boundary before I/O.

Callback authority boundary: `callbacks_admit_lexical_bindings_at_rowset_consumers`
covers upstream call binding, positional-only/keyword-only defaults, return
annotations before truth conversion, dictionary constructor contextual typing,
projection-lambda defaults and unreachable bodies. `callbacks_live_conditional_effects`
witnesses implicit returns and dead branches with independent HTTP write logs.
`monty-analysis::callable_evidence_tests` locks the underlying binder and reachability
exports without any catalog or host effects.

- Callable boundary: `map_body::tests::python_lowering::python_compute_uses_upstream_call_binding`
  runs reordered keyword arguments, positional-only/keyword-only signatures, closed defaults,
  and build defaults; rejects duplicate/missing arguments, positional-only keywords and
  defaults that could otherwise capture a build-local binding. Upstream binder tests live
  in `monty-analysis::argument_binding_tests`.

Program declaration coverage now includes ordinary helper methods through
`root_build_statements` and the declaration registry. Helper arguments retain
independent dependencies; recursion remains rejected. Structural mapping inputs,
set capture and ordered helper effects have abstract-fixture runtime witnesses in
`map_body::tests::python_lowering` (no production catalog coupling).
