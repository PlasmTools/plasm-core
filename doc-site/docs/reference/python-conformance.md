# Python lowering: semantic contracts and conformance

The [collection coverage contract](collection-coverage-contract.md) specifies producer evidence, operator transfer, consumer demands and recovery separation. Its implementation inventory and CE-01–CE-07 obligations remain explicit; the design is not a claim of completed matrix coverage.

Status: test-system design adopted 2026-09-27. Literate evidence validation and
two finite Get argument-binding property families and the `read-value-v3` independent
reference algebra, typed corpus and shrinker are implemented. Finite collection-boundary
and nested-effect models add 36 and 15 live scenarios respectively. The full
constructor/contract product remains an open implementation obligation. No universal correctness claim follows from
the original 162-row matrix or from this document's inventory tests.

## Scope and authorities

Plasm has a declarative outer language, an asynchronous typed DAG runtime and a
pinned inner Python profile. Its semantic scope is their explicit composition,
not all Python syntax. The [language definition](plasm-language-definition.md),
[scoped laws](python-scoped-composition.md), [compute contract](python-compute-contract.md)
and [Monty profile](python-compute-profile.md) remain normative.

| Boundary | Semantic owner | Required evidence |
|---|---|---|
| Outer source to typed DAG | Plasm binding and constructor rules | Admission, inferred contracts, explicit captures and effects |
| Typed DAG to scheduled/serialized plan | Plasm lowering and replay | Contract preservation, effect ordering, tamper rejection |
| Rowsets to recursive materialized values | Plasm observation and value contracts | Presence, cardinality, coverage, identity separation and exact codecs |
| Inner Python | Pinned Monty checker/runtime and explicit Plasm policy | Profile acceptance/rejection plus execution witnesses |
| Compute/host output to DAG | Plasm recursive output and capability contracts | Validated values, authorized effects, budgets and receipts |

Monty cannot prove API result completeness or field availability. Conversely,
Plasm must not reimplement Python expression typing. The Monty profile inventory
is linked, not copied into an outer-language whitelist.

## Contract carried by a rowset expression

Use the judgment `Γ ⊢ e : Q`, with `Q = ⟨S, K, C, O, A, E⟩`:

- `S`: recursive materialized shape, including named domains, owner/catalog pin,
  union alternatives and field availability. Presence and nullability are separate.
- `K`: cardinality proofs, including emptiness and checked local/global bounds.
- `C`: completeness evidence for the requested collection and outstanding
  continuations. A bound or a fully hydrated entity is not membership evidence.
- `O`: ordering and multiplicity guarantees. A rowset is not implicitly a set.
- `A`: receiver authority and qualified identity, separate from value-shaped IDs.
- `E`: reads, writes, dependency order, resources and observable effect receipts.

This is specification notation, not a new user-visible Python type or a claim
that one production struct currently carries all six dimensions. The audit must
map each dimension to its actual producers/consumers and identify absent facts.

Distinguish `CatalogEntity`, an `ObservedRow` with an available field shape, and a
`MaterializedRecord`. A capability's `provides` contract, actual observation and
projection determine available values. Taking every field from the catalog is
not a materialization proof. An optional or nullable field that was not observed
must not silently become `None`. Unavailable, absent, explicit null and empty
collections remain distinct; a missing required observation can trigger an
explicit dependency or an attributed failure, not fabricated data.

## Constructor transfer-rule audit

The [producer/consumer audit](python-constructor-audit.md) records current owners
and representation gaps. This is the scope of the audit, not a declaration that
every product cell passes.
Each constructor needs premises, transfer equations, static/runtime obligations,
implementation owners and evidence. Lowering must preserve the judgment or reject;
it must not silently strengthen a premise.

| Constructor family | Transfer obligations | Existing law links |
|---|---|---|
| Program, local binding, return | Resolve names, seal dependencies; explicit result roots; retain unreturned effects | BC-03, BC-04, SC-01, SC-11 |
| Get, query, search, relation | Capability output shape; identity versus selection; independent field/membership evidence | BC-01, SC-02, SC-07 |
| Filter, sort, distinct, take | Preserve available types and lawful authority; define order/multiplicity and coverage of the requested result | SC-02, SC-04, SC-07 |
| Projection, aliases, derivation | Produce the actual projected shape; preserve domains; do not invent fields or receiver authority | BC-01, SC-02 |
| Union, membership | Compatible row shape; lawful multiplicity; combine dependency coverage without promotion | SC-04, SC-07 |
| Aggregate, group | Defined empty behavior and result types; synthetic authority; sufficient input coverage | BC-01, SC-02, SC-07 |
| Map, flat-map, captures | Recursive transfer with scoped ports; shape versus flattening; local/global bounds and full addresses | SC-01 through SC-10 |
| Mutator, acknowledgement | Declared observed return type versus receipt; ordered effects; read-after-write freshness | BC-04, SC-08, SC-09, SC-12 |
| Bounded iterate | Re-observe after effects; stop/bound failure; retained receipts and identity | SC-05, SC-06, SC-08, SC-09 |
| Record/array assembly | Rowset-to-value rule; exact recursive shape; complete materialization and value/authority separation | BC-01, SC-02, SC-03, SC-07 |
| Compute, rendering, host calls | Monty profile; faithful stubs/codecs; explicit inputs and capability authority; validated returns | BC-02, BC-04, SC-04, SC-12 |

Deferred row methods use a closed production dispatch registry. The
[constructor rule gate](python-row-constructors.md) requires an exact rule inventory,
positive matrix witnesses and executable negative admission examples. Registration
also covers the four [build statement constructors](python-build-constructors.md):
documentation, binding, discarded effects and return. Their witnesses include
retained write receipts and ordered tuple returns; negative programs check the
statement premises and terminal-return rule. Closed gates also cover
[catalog calls](python-catalog-constructors.md),
[relation navigation](python-relation-constructors.md), and
[outer scalar DAG nodes](python-scalar-constructors.md), and
[recursive literal operands](python-literal-constructors.md), and
[program/class declarations](python-declaration-constructors.md), and
[named projection expressions](python-projection-constructors.md), and
[outer predicates](python-predicate-constructors.md), and
[aggregate descriptors](python-aggregate-constructors.md). Remaining
contextual expression rules and semantic products need their own evidence. These gates do not
claim to enumerate every lowering branch. The [reconciled coverage boundary](python-constructor-audit.md#reconciled-coverage-boundary) records the remaining contextual seams and gate-strength differences. Host calls and pure compute have different
capability policies and must not be merged into an implicit IO escape.

## Test architecture

1. **Original obligation matrix:** retain every original row, feature, view and
   Python witness. Those assertions remain independent of new composition checks.
2. **Literate laws:** `plasm-law` blocks beside normative prose link original rows
   to executable cases and typed property families. Tests parse these documents
   directly. No separately maintained JSON law/status copy exists.
3. **Constructor contracts:** audit each rule's preconditions and inferred result
   at admission, IL validation, scheduling, materialization and replay. These
   checks establish well-formedness; they do not prove execution equivalence.
4. **Independent semantic oracle:** implement a small finite Plasm algebra over
   fixture observations and effect traces. It must not call production lowering,
   schema inference or materialization to calculate expected results. Monty owns
   the inner computation; the oracle does not evaluate Python source.
5. **Typed composition generator:** derive valid and invalid programs from rule
   premises. Generate whole expression trees, not only adjacent operation pairs.
   Shrink with the typing derivation and failing obligation intact. Retain source,
   contracts, fixture data, profile/catalog pins, seed/depth and failure stage.
6. **Differential/metamorphic oracles:** compare reference execution with lowered
   execution, equivalent call spelling, lawful factoring, alpha-renaming, and
   execution before/after rewrites. Assert that the intended transformation or
   context was actually exercised. Equality of two equally broken executions is
   not an independent oracle; retain concrete expected values/effect assertions.

Compare values, recursive shapes, order, multiplicity, authority, coverage and
effect traces. For independent concurrent work compare the specified causal
order, not incidental wall-clock order. Failures are observations too: compare
stage, address, completed/failed/uncertain effects and uninvoked dependents.

### Finite dimensions and exclusions

| Dimension | Required representative partitions |
|---|---|
| Value | Every Plasm primitive/domain; optional; tagged/untagged union; nested record/array |
| Presence | Present value; explicit null; absent field; unavailable observation |
| Cardinality | Zero; one; many; exact bound; bound plus one |
| Collection | Complete; empty complete; partial; unknown; each continuation channel |
| Shape | Full observation; partial observation; projected; aliased; synthetic; heterogeneous branch |
| Authority | Entity; projected entity where lawful; synthetic ID lookalike; compound; federated homograph |
| Context | Root; child; grandchild; siblings; captures; record and array embedding; replay |
| Effects | Read; write; read-after-write; discarded result; failure/cancellation before and after a prefix |

Exhaust small finite products. For larger products report planned size, generated
size, exclusions, seed and maximum depth separately. Pairwise coverage is a
sampling strategy, not proof of recursive closure. The deterministic `read-value-v3`
corpus has 41 closed-shape trees and six observed-shape trees over three source cardinalities (141 live comparisons).
Its shrinker preserves output type, strictly reduces tree size and retains failure
stage. The separate effect model covers a finite nested-create product; random sampling
and the full type/effect product remain open. See
the [implemented scope and exclusions](python-constructor-audit.md#finite-reference-algebra).

### Reporting and gates

Report independently: original-row coverage; law checks with evidence and remaining
gaps; finite generated cases; runtime pass/fail/skip results; open contradictions;
Monty profile classification. Do not collapse these into a single percentage.

A registered witness is not a passing test result. Each law requires positive live,
negative admission, runtime evidence and metamorphic roles. Evidence may establish
only part of a check: its `gap` remains explicit. Status is derived, never edited.
Even a law with all finite obligations discharged is not a theorem about every
program. A regression that expects rejection of a normatively valid program is a
counterexample, not positive conformance.

CI runs inventory checks AND their live/property suites. Invalid links, duplicate
evidence, missing evidence roles and erased gaps without evidence fail inventory.
A failing law reproduction must not be silently skipped or reclassified as an
unsupported language feature. Each declared exclusion needs a normative reason.

## Boundary laws and executable evidence

### BC-01 — Faithful rowset-to-value materialization

For `Γ ⊢ e : ⟨S,K,C,O,A,E⟩`, embedding its result into `list[Record(S)]`
requires complete materialization of the requested collection with no outstanding
continuation. Runtime values must inhabit `S`, retain `O` and preserve domain
contracts. The containing value receives no receiver authority. Projection narrows
`S`; it cannot restore absent fields from the catalog. Neither cardinality bounds
nor individual entity hydration discharge the collection premise.

For an explicit projection `F`, whole-row embedding obeys
`embed(project_F(R)) = [ { f: row[f] | f in F } | row in R ]`. It preserves
order and multiplicity and requires every selected field to be observed. A null
is copied as null; an absent selected field fails. Structural identity, ambient
receiver slots and storage metadata outside `F` are not value fields. Receiver
authority remains on the source sidecar and is neither granted to the record nor
removed from the independently admitted receiver operation.

Unprojected observations use `ObservedRecord(S,P)`, where `P` lists fields that
may be absent. Presence is independent of nullability: absent keys stay absent;
explicit null remains null and must satisfy the field's value contract. Catalog
requiredness does not prove a field was observed by this read. Constructed records
have guaranteed keys; explicit projection demands each selected field. Sorting or
filtering on an absent field fails. Set equality distinguishes absent from null.
Nested observed records retain these rules and carry no receiver authority.
Monty missing-attribute lookup returns undefined, producing `AttributeError`;
a pure compute may handle that exception without hydrating or invoking host IO.

The v3 corpus covers unprojected read arrays with absent versus null fields.
Unknown relation membership still fails the collection guard. A finite nested mutation-array product is covered;
full presence/coverage products require additional evidence; completed writes
must remain visible if subsequent materialization fails (BC-04/SC-09).
A bare relation in a record field (`{"children": p.r6}`) also rejects as a
scalar field, while a call-suffixed relation reaches runtime. This is a separate
SC-04 admission counterexample, not a reason to teach an artificial suffix.

```plasm-law
{
  "id": "BC-01",
  "extends": ["lang_limit_projection", "lang_relation_many_from_plural_query", "lang_union_rowset"],
  "checks": [
    {"role": "positive_live", "claim": "Recursive synthetic record arrays retain their declared values and shape", "evidence": [{"kind": "matrix", "id": "scoped_nested_records"}], "gap": "The full observed-shape product still requires witnesses."},
    {"role": "negative_admission", "claim": "Impossible field, domain, authority and shape claims reject", "evidence": [], "gap": "Register boundary-specific negative witnesses across all value families."},
    {"role": "runtime_evidence", "claim": "Missing, null, unavailable and incomplete collections remain distinct", "evidence": [{"kind": "property", "id": "read_value_algebra"}, {"kind": "property", "id": "recursive_value_boundary"}, {"kind": "property", "id": "collection_boundaries"}, {"kind": "property", "id": "mutation_array_effects"}], "gap": "Finite live evidence covers read and mutation arrays, observed presence, and indexed/offset/cursor paging into compute and record arrays. Unknown/unavailable collections, continuation resumption and the full shape/effect product remain open."},
    {"role": "metamorphic", "claim": "Equivalent field projection and embedding preserve materialized contracts", "evidence": [], "gap": "Extend the finite independent materialization oracle with projection/embedding rewrite comparisons and the full observed-shape product."}
  ]
}
```

### BC-02 — Checker, codec and value-contract agreement

For sealed `Compute<I,O,E,B>`, generated stubs, codecs and validation must denote
the same recursive boundary values. Upstream acceptance cannot authorize an
unavailable Plasm field or effect. Runtime success means validated `I`, execution
within `E,B`, and validated `O`; it does not follow from type checking alone.
Typed scalar, domain, record, array and union returns are implemented under the
[typed return contract](python-return-contract.md), with finite `typed_returns`
and structural/effect-gate witnesses. Arbitrary constructor products remain open.

```plasm-law
{
  "id": "BC-02",
  "extends": ["lang_render_projected_shape", "lang_render_value_error_at_execution"],
  "checks": [
    {"role": "positive_live", "claim": "Admitted typed Python renders projected fields and recursive inputs", "evidence": [{"kind": "matrix", "id": "type_projected_integer"}, {"kind": "matrix", "id": "text_conditional_membership"}], "gap": "These cases cover only selected values; the profile ledger and codec suites remain separate required gates."},
    {"role": "negative_admission", "claim": "Generated declarations reject unavailable fields and undeclared authority", "evidence": [], "gap": "Register profile and recursive-boundary rejection witnesses here."},
    {"role": "runtime_evidence", "claim": "Codec validation and resource failures preserve exact value contracts", "evidence": [{"kind": "property", "id": "recursive_value_boundary"}], "gap": "The finite recursive input family covers fixture domains and observed presence through real Monty. Union generation, broader recursive products, resource faults and general output validation remain separate obligations."},
    {"role": "metamorphic", "claim": "Checker declaration and runtime encoding denote the same values", "evidence": [], "gap": "Generate boundary values independently of production stub and codec implementations."}
  ]
}
```

### BC-03 — Lowering preserves semantic judgments

Every admitted constructor preserves its transfer rule through DAG, scheduled
plan and replay. Equivalent surface forms preserve their semantic plan when the
rule states canonical equivalence; more general rewrites preserve observations
even if graph structure differs. Changes to identity, values, bounds, catalog
pins or effect order must not be erased by normalization.

The initial finite property family checks Get identity spelling across seven
literal identities and five contexts (root, projection, bound read, child and
grandchild), plus six invalid forms in those contexts. Its 35 equivalence and 30
rejection checks do not cover all constructors, bound-value types or live effects.

```plasm-law
{
  "id": "BC-03",
  "extends": ["lang_get_by_id", "lang_bound_get_field"],
  "checks": [
    {"role": "positive_live", "claim": "Identity spellings and registered build statement witnesses execute through typed DAG lowering", "evidence": [{"kind": "matrix", "id": "get"}, {"kind": "matrix", "id": "bound_get_field"}, {"kind": "matrix", "id": "bound_get_field_keyword"}, {"kind": "matrix", "id": "root_build_statements"}, {"kind": "matrix", "id": "parallel"}], "gap": "Live spelling witnesses cover simple identity; constructor-wide lowering equivalence remains open."},
    {"role": "negative_admission", "claim": "Invalid Get binding rejects in each generated context", "evidence": [{"kind": "property", "id": "invalid_get_binding"}], "gap": "This generated family covers Get binding only, not all invalid constructor premises."},
    {"role": "runtime_evidence", "claim": "Lowered and reference execution agree on values, order, coverage and effects", "evidence": [{"kind": "property", "id": "read_value_algebra"}, {"kind": "property", "id": "reduction_algebra"}, {"kind": "property", "id": "primitive_reductions"}, {"kind": "property", "id": "typed_returns"}, {"kind": "property", "id": "outer_values"}, {"kind": "property", "id": "recursive_value_boundary"}, {"kind": "property", "id": "collection_boundaries"}, {"kind": "property", "id": "mutation_array_effects"}], "gap": "Finite read/value, reduction, pagination and nested-create models cover their declared products. Authority, other effects/types and arbitrary constructor products remain open."},
    {"role": "metamorphic", "claim": "Get positional and keyword identity produce equivalent plans across contexts", "evidence": [{"kind": "property", "id": "get_identity_spelling"}], "gap": "This finite family does not establish arbitrary rewrite or recursive operation equivalence."}
  ]
}
```

### BC-04 — Admission and failure preserve effect accountability

All statically decidable contract violations, including invalid later/empty
bodies, reject before review and backend IO. A data-dependent coverage, shape or
budget failure is not evidence that no write occurred. Dependencies must schedule
known prerequisites before effects that require them. Runtime failures preserve
completed and uncertain effects and prevent unauthorized dependent work; no
implicit rollback or blind replay is allowed.

```plasm-law
{
  "id": "BC-04",
  "extends": ["lang_program_return_consecutive_writes", "lang_iterate_bound_exhausted"],
  "checks": [
    {"role": "positive_live", "claim": "Nested and discarded effects participate in ordinary DAG execution", "evidence": [{"kind": "matrix", "id": "scoped_nested_effects"}, {"kind": "matrix", "id": "root_build_statements"}], "gap": "Other effect forms and mixed read/write bodies remain separate obligations."},
    {"role": "negative_admission", "claim": "Invalid later and empty bodies cause zero IO", "evidence": [], "gap": "Link existing host admission/replay assertions through a typed executable registration; test names alone are not evidence."},
    {"role": "runtime_evidence", "claim": "Late shape/completeness failure preserves already completed writes", "evidence": [{"kind": "property", "id": "mutation_array_effects"}], "gap": "The independent nested-create model covers rejection, response loss after commit and invalid successful responses. Cancellation has a separate host regression; other mutators, transports, crash recovery and generalized failure-prefix generation remain open."},
    {"role": "metamorphic", "claim": "Equivalent lawful schedules preserve causal effects and failure prefixes", "evidence": [], "gap": "Implement an independent effect-trace oracle with partial-order comparison."}
  ]
}
```

## Recursive value/presence family

`recursive_value_boundary` generates 80 cases from the 20 fields of the abstract
`python_value_contract` fixture. Each field is nested under a constructed record
and array, or under a record containing an observed row array. Both routes run
through Python admission, dry validation, live fixture HTTP and real Monty.

The product covers present values for every field; absence for all 19 nonidentity
fields; and explicit null for the nullable integer. Get identity is excluded from
absence because the requested identity is independently authoritative. Constructed
field reads must fail on absence; observed records must retain absence and expose
`AttributeError`. Expectations include exact nested values, complete coverage,
empty operation ledgers, Python observations and catalog/domain pins in the plan.
No production validator, stub generator or codec computes expected values.

The leaves include Boolean, integer beyond IEEE exact range, number, digit ID,
UUID, date/time domains, enum/multi-enum, exact money, opaque entity reference,
JSON with u64, blob and recursive arrays. This fixed-depth product is not a
recursive generator for arbitrary union branches or all optional-field subsets.
It does not add a second Python evaluator or extend Monty's admitted profile.

## Finite collection and effect models

`collection_boundaries` exhausts three strategies (indexed, offset, cursor), four
cardinalities (0, 1, one fixed backend page, page plus one), and three contexts
(returned page, Python compute, nested record array): 36 live executions. Backend
page coordinates remain stable. Collection consumers demand complete acquisition;
explicit `take` defines a bounded expression. Empty pages are complete only when
the pagination driver proves exhaustion. Outstanding continuations cannot become
ordinary arrays. This family does not exercise resumed public page handles or
unknown/unavailable observations.

`mutation_array_effects` uses an independent state/effect model for nested create
fanout over 0, 1 and 3 parents. Fifteen scenarios cover success and first/last child
rejection, response loss after commit, and malformed successful responses (the
first/last position coincides for singleton inputs). The model specifies
best-effort inner fanout followed by failure of incomplete array embedding; later
outer occurrences must not execute. Tests compare server attempts/commits,
ordered operands, exact nested values, leaf acknowledgements and occurrence
serialization after plan round-trip. A 503 after commit models a lost successful
receipt; it is not a socket-disconnect test.

Async occurrences carry `mutation_dispatches` separately from semantic
acknowledgements. `unresolved` means transport dispatch began without a successful
response; `response_received` means successful transport receipt, even if decoding
or Python continuation later fails. Neither proves domain state change or rollback.
Pre-transport failures mint no dispatch. Completed host receipts survive Python
cancellation/resume failures. This evidence currently instruments HTTP/GraphQL
mutation dispatch, not arbitrary local/credential/EVM effects or durable crash recovery.

## Implementation sequence

First audit the six contract dimensions and constructor producers/consumers; use
BC-01 to resolve observed-shape versus catalog-shape representation explicitly.
Then implement an independent finite rowset/value oracle and recursive codec
laws. Add generated compositions and shrinkers before widening the supported
surface. Finally run original, boundary, scope, replay and packaged-worker suites
together; use AppWorld as subsequent integration/teaching evidence.

The literate inventory, Get spelling families and finite read/value differential
family are implemented. The producer/consumer audit identifies representation
gaps; observed read arrays and finite mutation-array/pagination products are covered.
Other effects, continuation resumption and the full presence/coverage product remain open. Expand the independent
model by transfer rule and contract dimension, preserving explicit exclusions,
before treating broader runtime success as conformance.

## Primary-source precedent

JAX separates primitive abstract evaluation from transformation rules and checks
Jaxpr type/effect consistency. Its operation records and tests compare reference,
eager, compiled and batched execution, with explicitly sampled Cartesian products.
See [primitive rules](https://docs.jax.dev/en/latest/601/jax-primitives.html),
[test comparison and sampling helpers](https://github.com/jax-ml/jax/blob/027d9f3392fc5c770c3c923b9874b10d247a650d/jax/_src/test_util.py)
and [vmap comparisons](https://github.com/jax-ml/jax/blob/027d9f3392fc5c770c3c923b9874b10d247a650d/tests/lax_vmap_test.py).

DaCe separates frontend replacements, graph validation and transformation
applicability. Its tests compare compiled/reference outputs, reject invalid nested
connectors and assert that a transformation actually changed the graph. See
[replacement registry](https://github.com/spcl/dace/blob/ad85878d1a391e4f7ac277b8313b8c8b787612ea/dace/frontend/common/op_repository.py),
[comparison helper](https://github.com/spcl/dace/blob/ad85878d1a391e4f7ac277b8313b8c8b787612ea/tests/numpy/common.py),
[nested validation](https://github.com/spcl/dace/blob/ad85878d1a391e4f7ac277b8313b8c8b787612ea/tests/sdfg/validation/nested_sdfg_test.py)
and [fusion tests](https://github.com/spcl/dace/blob/ad85878d1a391e4f7ac277b8313b8c8b787612ea/tests/transformations/map_fusion_vertical_test.py).

These practices motivate the design; neither inspected system provides an
exhaustive semantic coverage denominator. DaCe's documented Python callbacks are
not adopted: Plasm retains its explicit Monty and reviewed-DAG boundaries.

## Finite reduction algebra

`reduction_algebra` compares an independent std/serde_json model against live
Python compilation, dry validation and HTTP-backed execution. `reduction-v1`
contains six score sequences: empty, one integer, one null, all null, mixed
null/negative/positive values, and repeated integers including zero. The full
product with ascending/descending identity order, global/grouped reduction and
direct/aliased score fields has 48 executions, each applying all seven functions.
Groups use alternating non-lexicographic text keys to expose first-seen ordering.

The model defines count as row cardinality; numeric functions ignore null values.
Sum returns zero for no numeric operands; average/min/max return null. First/last
preserve the selected endpoint, including null. Global empty input produces one
row; grouped empty input produces none. Assertions compare exact public rows,
order, complete coverage, absence of continuations and absence of effects.
Expected values never call production aggregation, schema inference or codecs.

This finite family does not cover missing fields, partial/unknown collections,
nullable/multiple group keys, noninteger domains, precision/overflow extremes,
nested reductions or full inferred result-type equivalence. These remain gaps;
48 executions are not a claim of general reduction closure.

## Primitive value-preserving reductions

`primitive_reductions` checks all 14 CGS FieldType variants across 20 fixture
fields/domains, forward/reverse/empty inputs: 60 live programs. First/last preserve
the input domain, recursive shape and constraints; they are nullable on empty
global input. Count is integer. Tests verify sealed Monty input contracts, exact
returned values and a real typed Python consumer. JSON u64, leading-zero IDs,
exact money, enums, temporal profiles and recursive arrays are included. Numeric
reductions across all domains, arbitrary group keys and precision limits remain
separate obligations; no operation is made lawful merely by being enumerated.

`typed_returns` exercises all 14 CGS primitives through named-domain output and a
second compute, for 60 live cases. Structural, presence, federated-domain and
effect-gate witnesses are detailed in the [typed return contract](python-return-contract.md).

`outer_values` is the finite [outer value transfer](python-outer-value-contract.md)
property: branch products, arithmetic domain erasure, mixed-column composition and
failure-before-effect checks. It complements constructor admission rather than
using a passing example as evidence for all operand types.
