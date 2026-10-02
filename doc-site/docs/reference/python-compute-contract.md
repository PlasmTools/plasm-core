# Python compute contract

Status: normative language specification, adopted 2026-09-25. Implementation
conformance is tracked separately below. A specification requirement is not a
claim that the current implementation satisfies it.

## Ownership of semantics

Plasm has two language boundaries. The outer `Program.build` is a declarative,
statically admitted DAG builder (it is not executed as arbitrary Python).
The inner `@compute` body is Python implemented by the pinned Monty runtime and
checked by its upstream Python checker. Plasm MUST NOT maintain a second Python
expression type checker or expand an expression whitelist from task failures.

The supported inner language is the intersection of the pinned Monty execution
profile, its checker/typeshed, and explicitly documented Plasm policy. This is
not a promise of CPython compatibility or unrestricted third-party imports.
Supported statements and expressions are inherited from that profile; arbitrary
syntax exclusions require a policy rationale and a negative conformance case.

| Responsibility | Authority |
| --- | --- |
| Python binding, control flow, expressions, narrowing, comprehensions and function types | Upstream Monty checker |
| Rowset cardinality, completeness, order/identity laws, catalog ownership | Plasm |
| Available inputs, dependencies, host capability authority and effect ordering | Reviewed Plasm DAG |
| Primitive/domain, nested/union input and output validation | Recursive Plasm value contracts |
| Python execution, resource bounds, cancellation and crash isolation | Pinned Monty pool and Plasm policy |

## Boundary judgement

A compute node has a sealed contract `Compute<I, O, E, B>`: input contract I,
output contract O, allowed effects E and resource budget B. Admission requires:

1. The outer program resolves every input and capability in the current session.
2. Generated checker declarations faithfully represent I, O and E.
3. The pinned upstream checker accepts the function against those declarations.
4. Plasm validates the DAG edges, ownership, cardinality and collection proofs.
5. The review identity seals the source, contracts, capabilities, catalog pins,
   language profile and policy versions.

Runtime success means: validated input values satisfying I, execution confined
to E and B, and a returned value validated against O. Static acceptance does not
promise termination or absence of Python exceptions. Exhausted budgets and
exceptions are explicit failures and cannot produce successful output values.

## Exact wire values

Opaque JSON and blob values preserve recursive JSON structure, including unsigned
JSON integers through `u64::MAX`. Their carrier must not round these integers
through a float or discard ordinary blob objects. This does not widen the signed
64-bit `Integer` domain: an out-of-range JSON integer fails admission to that
domain. Temporal and money domains use their declared canonical encodings;
normalization is distinct from loss of value or field presence.

## Dependency closure

All accessible workflow values MUST be explicit node inputs. Pure compute has
no catalog objects, credentials, host callables, ambient mutable state or implicit
relation hydration. Standard-library facilities are governed by the pinned
profile and effects policy, not assumed pure because they are Python builtins.

The materialized source schema is authoritative. A catalog's larger entity
schema MUST NOT silently restore fields removed by projection. Whole declared
inputs are the correctness baseline; field pruning is only an optimization with
a separate proof. Dependency discovery MUST NOT require interpreting every
possible Python expression, alias, helper or comprehension.

Missing, null, unavailable and empty remain distinct. An absent observation must
not be manufactured as null or an empty collection. Partial/unknown collections
cannot be certified complete by compute. Row values are data, not entity handles;
constructing an id-shaped record does not acquire receiver authority.

## Contract fidelity

One recursive contract model drives teaching declarations, checker stubs,
materialization and return validation. Preserve primitive distinctions, nullable
values, arrays, records, union alternatives, discriminators, enum membership,
semantic value identity and catalog ownership. No Any-shaped replacement for an
unsupported contract, and no silent conversion of exact/domain values to a less
precise primitive.

Python structural typing is not sufficient evidence of domain or authority
compatibility. Constraints not expressible by upstream types MUST remain explicit
Plasm boundary validations. Contract representations and admitted Python
operations must agree; a typing alias alone cannot promise operations the runtime
representation does not implement.

Compute returns use the recursive Plasm value contract. Annotations admit
`bool`, `int`, `float`, `str`, session `vN`, `Row`, `Value[eN]`, record type fields
such as `Row.price`, `list[T]`, and `T | U` (including `None`). `Row` is the actual
input row contract, including field presence. A returned record may be an input
record or a matching Python dictionary. Arrays remain typed values; they do not implicitly become entity rowsets. Every output is
validated before publication and dependent effects. Arbitrary Python objects,
non-string dictionary keys, nonfinite numbers, cycles and out-of-range integers
are rejected. Each named domain resolves against its own loaded, pinned catalog,
including nested values returned from another catalog in a federated session.

## Admission and execution

Admission MUST finish for every compute node, including nested correlated bodies,
before a review/run token can be issued. A bad later compute must prevent earlier
writes from starting. Checker failure fails admission closed.

Static analysis and definition checking run synchronously in-process through monty-analysis.
The server schedules checking on a bounded CPU executor; this scheduling is asynchronous. It MUST NOT block
an async server on a nested runtime, start an improvised IPC service, invoke user
function bodies on fabricated data, or perform host calls. Definition-only
checking must strip or reject user-controlled definition-time execution (defaults,
decorators and evaluated annotations); declarations are host-generated.

At execution, only the reviewed source and contract can run. Stored/replayed
plans must re-establish admission under the pinned profile or carry a verifiable,
matching proof. A cache hit must bind all admission inputs; source-only caching
is insufficient. Pure compute permits only the five exact-money library bindings (`money_add`,
`money_sub`, `money_mul`, `money_div`, `money_compare`), bounded to 4096 suspensions; all other
undeclared host interactions fail. These bindings perform no IO or effects. Declared host reads
and writes retain the existing DAG approval, ordering, cancellation and completed-
write evidence contracts; a Python function name grants no host authority.

Checker source, AST and type-graph budgets are separate from execution limits.
Each check creates fresh in-memory state. Cancelling a scheduled check discards
its result; it cannot forcibly terminate a running CPU thread. Its concurrency
permit remains held until that thread finishes. Execution workers retain their
existing cancellation and isolation guarantees.

## Language profile and completion criterion

The initial upstream pin is `pydantic/monty` revision
`e007685fbb06494c13b9a7b3fede9f8e6a54a2be`, already used by monty-pool and the
packaged worker. Record the bundled checker/typeshed identity and Plasm contract
version alongside this revision. A profile upgrade changes the admission cache
and review identity.

Maintain an exhaustive inventory of the upstream profile's syntax families,
standard-library modules and host facilities. Each entry is classified as:

- supported, with checker and runtime evidence;
- upstream unsupported, with an upstream reference and a rejection witness;
- prohibited by Plasm policy, with the rule and a rejection witness;
- unverified implementation obligation (never advertised as supported).

Cross syntax families with primitive, optional, union, nested and collection
contracts, using representative equivalence classes and generated/property
cases rather than a claimed exhaustive enumeration of Python programs. Separate
laws cover authority, effects, completeness, replay and isolation. Differential
checks may use CPython only where the pinned profile promises equal semantics.

The [literate conformance design](python-conformance.md) defines the Plasm-side
contract product and boundary laws BC-01 through BC-04. Its machine-readable law
blocks are consumed by the matrix, with evidence and open gaps beside the prose.
This inventory and its initial generated Get family do not prove recursive codec
or materialization closure. Presence-aware observed arrays are implemented; the full
recursive type/presence product still needs independent evidence.

Existing Python matrix obligations remain the authority for preserved Plasm
semantics. New Python capabilities add obligations to that same coverage ledger.
AppWorld runs are integration evidence and cannot define general language rules.

Completion requires removal of the hand-written expression inference checker,
production admission integration across HTTP/MCP/NAPI/evaluator, recursive
contract round trips, pre-effect negative tests, actionable source-mapped
corrections, and reviewed coverage for every profile classification. A passing
CSV case alone cannot establish completion.

## Implementation ledger

### Entity-record annotation spelling

`Row[eN]` and `Value[eN]` resolve to the same catalog-pinned materialized record
contract, recursively in lists, unions and record-field annotations. Bare `Row`
retains its input-derived meaning. Neither spelling grants entity or mutation
authority. Teaching retains `Value[eN]` as its canonical explicit spelling.
Witnesses: `row_entity_annotation_matches_value_contract_recursively` and
`python_row_entity_annotation_runs_without_granting_authority`.

### Federated map record materialization

A scoped map may return nested records and arrays whose value domains belong to
other catalogs loaded in the enclosing execute session. Materialization borrows
that session's catalog registry for observation projection and recursive validation;
the parent's dispatch-scoped catalog is not the complete value-domain registry.
Domain pins, primitive types and missing-catalog failures remain enforced.
Witness: `map_record_preserves_foreign_nested_value_domains` at the map output
materialization boundary, including wrong-pin and invalid-value rejection.

### Open ownership gap: pure outer expressions

Normatively, pure Python expressions in `build` and row lambdas have Monty
semantics, including operand-selecting `and`/`or`, truthiness, short-circuiting
and scalar method composition. Plasm seals captures and checks consumer contracts;
it must not substitute Boolean algebra for Python operand selection.

Pure outer expressions now seal their captures and consume synchronous
`monty_analysis::analyze` through the linked library. The separate scalar evaluator
and datetime method/result inference have been removed. Monty supplies result-type
evidence and executes the resulting typed compute node; there is no analysis
worker protocol. Plasm still constructs lazy DAG scopes when evaluating an operand
requires a relation read or other graph dependency.

This integration is not yet a conformance-completion claim. The language matrix
must close remaining inferred-domain, nullable/nested-expression and lazy-scope
obligations before a new evaluation baseline is frozen. No `Any` or sample-value
evaluation is an acceptable substitute for resolved static contracts.

Inference declarations must carry a source-scoped mapping to the exact pinned
Plasm contracts. Existing checker stubs represent some constrained domains by
their primitive carrier; those stubs alone cannot certify inferred domain outputs.
Decoding must reject unresolved or unrepresentable contracts, preserve missing
separately from null, and never infer receiver authority from a Python type.
The structured graph is type evidence; it does not replace runtime-profile
admission or authorize host effects.


Root values, record/array elements, projections and read/write arguments now use
shared recursive value lowering. Numeric/boolean/null bindings, immutable aliases,
empty records, inline singleton receivers, arithmetic, length, boolean conditions
and recursive container operands are implemented in that common path.
Monty supplies true/false branch evidence through its synchronous analysis API.
Outer scopes and row-preserving filters map that evidence onto immutable captured
ports. The checked `Refine` IL operation intersects contracts without granting
new domains or authority. Both branch bodies remain reviewed, only the selected
scoped body executes, and facts do not escape merges.

The closure obligation is substitution: binding an admitted value expression to
an immutable local, or embedding it in a record/array, preserves its value contract
and dependencies. Consumers retain cardinality, expected-type, completeness and
authority checks. A scalar catalog argument requires both a scalar-cell contract
and a static or bounded singleton proof: a per-row scalar compute over plural
input is still plural and rejects before execution. The `value_recursive_*` matrix crosses these contexts; the
[outer value contract](python-outer-value-contract.md) specifies lazy branches and
retained arithmetic/domain laws.

Value-boundary closure is implemented by the `value_closure_*` live matrix.
Structural records and constants require no nominal entity owner. Their compute
nodes still pin the session catalog context, complete recursive input schema and
Monty profile; dry planning never executes Python, even over constant inputs.
Nested field access preserves recursive types and selected-field provenance.
Absent fields fail distinctly from present `None`; records grant no entity authority.

Scalar/value input annotations are accepted but **not taught**. They require one
unambiguous value column. If that column matches the annotation, the existing
bounded per-row scheduler supplies its value to Monty. Otherwise `list[T]` may
collect one column of `T`. Matching an array-valued cell takes precedence over
collection adaptation. Multiple columns or incompatible types are rejected.
Concrete adapted arguments are validated before invocation, and outputs before
any dependent effect. Completeness, row/byte budgets and cancellation remain the
same as canonical row compute. Serialized mode, source schema and catalog pins
are independently revalidated; no authored loop or implicit entity authority is
introduced. Scalar extraction remains an explicit cell operation in the sealed IL
and in the shared dry/live materializer. Object-valued primitive cells (including
JSON and blob values) remain one value; their payload keys do not become row
columns. Direct and projected blob round-trips are covered by
`scalar_blob_roundtrip_preserves_value_boundary`, and all fixture domains by
`scalar_compute_preserves_all_fixture_domains`. Shared teaching continues to show only `Row`/`Value[eN]` and their
collection forms, guarded by `scalar_compute_adaptation_is_not_taught`.

Outer dictionary construction uses the same recursive value contracts as scoped
record construction. It produces one record over an explicit unit source, with
captured values as DAG inputs. This preserves empty embedded collections without
silently suppressing the record. Singleton extraction retains its runtime empty
check. Bound scalars remain scalar values; bound constructed records remain
records. Constructed values have no receiver authority. The language matrix's
`record_value_*` cases cover execution, reuse, projection and compute consumption;
compile-negative cases cover plural extraction, unavailable fields and authority.


Whole-row operands with an explicit projection now assemble only those declared
columns, separately from receiver identity metadata. Missing projected fields
remain errors; explicit nulls and empty arrays retain their values. The
`read-value-v3` matrix family covers projected and observed read arrays.
Presence-aware records preserve absent keys independently of nullable values.
Profile v5 seals this boundary contract. Async occurrence snapshots retain
acknowledged operations across later failures; generalized failure-prefix
differential evidence remains open in BC-04.

| Obligation | Current evidence / remaining work |
| --- | --- |
| Synchronous in-process checking and stubs | Monty analysis tests verify type evidence, source spans, request isolation and no execution; workers execute reviewed code only |
| Typed output boundary | Recursive return contracts, owning-catalog validation, typed downstream inputs and rejection before dependent effects; [matrix evidence](python-return-contract.md) |
| Replace local expression checker | Expression and annotation types come from Monty; host references are translated to sealed declarations. Callback locals and expression captures use the upstream semantic index. Outer DAG lowering remains structural; see the boundary ledger below. |
| Asynchronous pre-review admission | Implemented through shared `compile_program`; HTTP/MCP/NAPI/evaluator and REPL migrated; cross-entry validation covers host admission, evaluator/REPL and language matrices |
| Generated recursive stubs from authoritative source schemas | Implemented from recursive value contracts; checker, materializer and codec tests are mandatory |
| Profile identity in reviewed plan and replay | Required typed-v11 profile field; replay readmits all nested nodes before IO |
| Whole-input dependencies without heuristic field pruning | Whole declared schemas replace used-field inference; projection omission is checked |
| Profile inventory and complete conformance classification | [Profile ledger](python-compute-profile.md); syntax families and every bundled module classified with policy/admission witnesses |
| Existing rowset/identity/effect laws | Retained; original matrix remains mandatory |

## Maintenance rule

Every change to the accepted language, upstream pin, boundary representation,
capability policy or correction behavior MUST update this specification (or its
linked profile ledger), teaching and the relevant matrix witnesses in the same
change. No task-driven one-off inference cases. Keep normative requirements and
verified implementation status distinct. A cutover removes the old admission
path rather than leaving an unadvertised fallback.


## Typed input assembly

A bounded map may apply a declared per-row `@compute` method to its captured row.
Its input is the exact projected recursive row contract; its text output is paired
with that occurrence's other output fields. A collection helper still consumes
an explicitly traversed many relation. No implicit singleton/list conversion is
allowed. Synthetic output records do not acquire receiver authority.

A named projection may capture a field from a previously bound proven singleton:
`rows.select("original", replacement=lambda row: rendered)`. This forms a
typed object derivation with explicit source and captured-input dependencies.
Captured values retain their recursive value contracts, including domain identity
and nullability. Plural captures reject before IO; an empty bounded singleton
fails at materialization. Lexical shadowing follows upstream binding; reserved
host symbols cannot be rebound by the row lambda.
The assembled rows preserve source order/count and combine dependency coverage.
No environment is implicitly made available to Python, and no code runs during
planning. The map-body assembly tests exercise these laws, including pre-effect rejection
and replay validation. The original language matrix contains independent Python
`assembly_row_compute` and `assembly_singleton_capture` witnesses.

## Captured write receivers

A flat-map catalog operation may target a previously named entity singleton:
`items.flat_map(lambda item: target.mN(value=item.value))`. The source occurrence
and the receiver are separate typed inputs. `target` must retain entity identity
and a static or bounded singleton proof; the method must belong to its catalog
and entity. Projection cannot restore removed identity, and synthetic records
cannot acquire receiver authority. Bind receiver reads before the flat-map;
inline receiver-building expressions are not admitted in this one-operation body.

The reviewed DAG seals both source and receiver dependencies. Each occurrence
supplies its own payload while the captured receiver identity remains fixed.
Empty source sets invoke no writes; an empty receiver with a nonempty source
fails before a write is dispatched. Existing effect ordering, cancellation,
partial-failure outcomes and post-write observation laws apply unchanged.

The `fanout_captured_receiver` Python matrix case and captured-receiver
observation-boundary tests are the implementation witnesses. They must pass
before this boundary is reported as verified.

## Source conformance authority

Python conformance tests assert the typed IL, fixture values, row order, coverage,
effects and errors directly. Historical path-expression source is not an execution
oracle. Preserve semantic-obligation IDs when replacing their source programs.
The language and view matrix runners accept Python only; remaining parser bridges
and other tests are tracked separately until removal is complete.

Scalar `where` comparisons may use a previously bound scalar cell or proven
singleton field as their right operand. These are explicit typed DAG inputs,
shared with write/iteration operand validation. Plural captures, unavailable
projected fields and rebinding of reserved host symbols are rejected before IO.
Ordinary lexical shadowing is resolved by the upstream semantic index.

## Collection acquisition and failed host continuations

A collection input proof demands the complete source expression. Planner demand
propagates through record derivation and map parents; an explicit `take` stops it.
Transport page coordinates retain their catalog-defined width, and presentation
paging must not truncate a collection acquired for compute or array embedding.
An empty page is terminal only with pagination-driver evidence.

Async host occurrences retain successful IO acknowledgements before Python resumes.
Cancellation or a failed continuation does not erase them. Separate mutation
dispatch evidence distinguishes an unresolved dispatched request from a successful
transport response; neither is a domain state-change proof. See
[BC-01 and BC-04](python-conformance.md) for the finite executable families and
remaining exclusions.

## Registered row constructors and recursive evidence

Deferred row method admission is dispatched through a closed typed registry,
with [literate rules and executable witnesses](python-row-constructors.md).
`recursive_value_boundary` compares independently specified nested fixture values
and Python observations for 80 domain/presence cases. Bound rowsets in record fields
use explicit collection capture ports, preserving observed presence and recursive
domain contracts. Typed returns now use the same recursive contracts (see
[return boundary](python-return-contract.md)). The finite corpus is evidence for
its declared products, not exhaustive coverage of every constructor combination.


Outer build statements now have a closed production dispatch inventory and
[executable constructor rules](python-build-constructors.md), separate from the
inner Monty statement model. This registers existing semantics; it adds no Python
syntax or new teaching vocabulary. The seven catalog capability kinds also have
[closed dispatch and IL-checked witnesses](python-catalog-constructors.md).
Relation navigation has its own [recursive typed evidence gate](python-relation-constructors.md).
The [outer scalar DAG constructors](python-scalar-constructors.md) are registered
separately from [recursive literal operands](python-literal-constructors.md).
[Class/decorator admission](python-declaration-constructors.md) has its own gate.
Remaining contextual rules and annotation/domain products still need evidence.

## Typed datetime boundary (v8)

Temporal CGS profiles materialize as Python `date` (date-only) or `datetime`
(RFC3339/Unix seconds/Unix milliseconds). `time`, `timedelta`, and `timezone`
are also typed compute values. Recursive records, arrays and unions retain these
types. Monty owns arithmetic, parsing, formatting and module/member availability.
Imports and aliases are retained for checking and execution. No filesystem,
environment or network capability is installed by an import.

Temporal outputs preserve components, offset and optional name in a tagged internal
representation. Catalog argument coercion encodes the declared wire profile; it
rejects naive instants and precision loss. Clock configuration belongs to runtime
and evaluation policy, not teaching. Named-zone rules and datetime fold semantics
are not supported by the pinned interpreter.

Evidence: `plasm_language_matrix/datetime.rs`, `python_pool/tests.rs` datetime
codec/clock witnesses, and `temporal_value` boundary tests.


### Naive datetime and response admission

The catalog profile `iso8601_naive_datetime` supplies native Python `datetime`
values without timezone information. It is distinct from RFC3339/Unix instants;
Monty owns arithmetic and naive/aware comparison rules. Transport rejects timezone
invention/erasure and precision loss. Invalid present response values are errors,
not optional nulls. Matrix witnesses are listed in `PYTHON-COVERAGE.md` under
catalog identity and datetime response boundary.

Static definition admission also invokes Monty's compile-only `MontyRun::new`
to retain upstream runtime-syntax validation before review. It never calls
`run` or `start`; Python execution remains in the worker. The analysis library
itself remains independent of the interpreter.

### Cross-node branch evidence

A scoped lambda receives one row, and chains of synthetic rowset operations remain
DAG operations. Row-preserving filters retain view provenance through plan
validation. Their output contracts are refined using Monty's branch evidence,
recomputed from the reviewed source and captured input contracts. The same API
supplies lazy outer-branch evidence; Plasm contains no independent predicate fact
algebra. Evidence intersects existing contracts and preserves field presence,
domain identity and temporal wire representation.

Materialized contracts are a sound abstraction of the inferred type graph. Literal
and truth-value exclusions that have no Plasm representation retain their enclosing
positive type (for example `str & ~Literal["skip"]` becomes `str`). This cannot
strengthen a proof or admit an unsafe access. Representable union and nullability
refinements remain intact; unresolved positive types remain admission errors.

Positive intersections may contain both a sealed nominal value domain and its
literal carrier constraint. Decoding meets those constraints while retaining the
sealed domain identity and catalog pin; it does not require a single positive
node. Constraint conjunction is symmetric: neither operand is the original value.
It uses `ValueContract::intersect_constraints`, separately from the directional
`refined_by` operation that cannot grant authority from branch evidence. Domain
pins and temporal wire representations are retained recursively through arrays
and record schemas; required presence wins over optional presence.
Distinct positive nominal domains or temporal encodings are rejected rather than
silently choosing one. Materialized record schemas remain closed: a field absent
from either schema must be absent in the intersection; requiring that field makes
the non-null intersection empty. Optional fields outside the common schema are removed.
Overlaps involving specialized carriers without a representable common contract
remain explicit errors; a different shape alone must not turn them into `Never`.
The inference witness
`boolean_membership_branches_preserve_materialized_contracts` exercises both
branches of Boolean identity and truthiness tests for plain and nominal, nullable
and nonnullable fields. This is type-bridge evidence, not a live effect-selection
witness or a claim of named-callback support. The independent live witness
`python_nominal_boolean_refinement_selects_only_matching_effects` records HTTP
write recipients after Boolean filtering and asserts that planning dispatches no
writes. `intersection_finite_scalar_algebra` checks idempotence, commutativity and
associativity over a finite scalar/null/union/domain basis; independent scalar and
record value-set witnesses check denotation and field presence. Graph-decoder witnesses
separately check recursively nested metadata and actual Monty literal branches.

### Standard descriptors and exact comparison

Monty owns classmethod/staticmethod typing, including inherited `Self` return
contracts. Its typeshed profile declares executable factories; missing CPython
factories (such as `datetime.fromtimestamp`) fail at admission. Chained comparisons
reuse Monty comparison narrowing for the next operand, without leaking facts
outside the chain. No Plasm comparison recognizer duplicates this analysis.

`money_compare(left, right) -> int` returns -1, 0 or 1. The left operand is Money;
the right is Money, an integer or an exact decimal string in major units. The
existing money kernel checks currencies. Floats and malformed decimal strings
fail; callers express predicates with ordinary Python comparisons to zero.

Expression coverage uses live semantic witnesses and independent literate
obligations, not a closed inventory of Plasm recognizers for Python operators.
DAG constructors retain their structural inventory and review-boundary checks.

Analysis rejects upstream error/fatal diagnostics. Advisory lints (for example,
a constant truthy condition) do not redefine Python admission. The analysis API
keeps type inspection on the same stub-injected source; human-facing checker
runs still retain their warning diagnostics.

Refinement checks observed fields without closing the graph row: later hydration
may add unrelated fields. It still checks required presence, known field types,
and nullability. Materialized outputs retain strict closed-record validation;
refinement neither exposes additional fields to Python nor grants authority.
Nonzero datetime `fold` remains outside Monty's preserved value model. This is
expressed as `Literal[0]` in its datetime signatures, not a reserved keyword ban
on unrelated Python calls.

### Typed record indexing

Input records support attribute access and string-key subscripting. Literal keys preserve exact field contracts; dynamic string keys return the union of declared field contracts. Absent keys raise `KeyError`; present nullable values remain `None`. Records do not thereby acquire dictionary methods, iteration, or entity authority. The worker protocol explicitly marks record access; ordinary host classes remain unsubscriptable.


## Python authority boundary ledger (2026-10-02)

Python parsing, annotation resolution, assignment compatibility and lexical binding
are upstream facts, including within expressions that capture DAG values. The host
must not reconstruct comprehension/lambda shadowing or maintain a primitive/type
annotation grammar. `monty-analysis::function_locals` and `external_names` export
semantic-index facts. These lexical APIs explicitly do **not** assert type admission;
compute definitions pass checker and Monty compiler admission at each call site,
where projected input contracts are known. Class extraction cannot check a body
against a nominal entity alone: relations and derived fields require the actual
input schema.

Catalog type references (`Row[eN]`, `Value[eN]`, `vN` and record type-field
projections) are host-owned evidence. Plasm translates these references into sealed
upstream declarations. All remaining annotation syntax is passed to the checker.
Incomplete annotations obtain concrete evidence from whole-function inference;
upstream assignment checking verifies the annotated constraint. Unknown/Any is never
published as a materialized value contract.

Python dictionaries have a separate recursive `Dictionary { key, value }` contract.
Only string keys materialize. Unlike fixed records they do not guarantee any key or
attribute. Empty dictionaries are valid when their key/value contracts are known;
mutated containers use upstream inference, not return-literal inspection. Dictionaries
survive serialization and subsequent typed compute without gaining entity authority.

Compute argument assignment follows Python typing, but runtime input validation
retains the actual contract (for example, Boolean stays Boolean even when Python
allows it as an `int` argument). Independent arguments are packed without a join or
zip, with the existing total row and byte budgets. Returned values are validated
before publication. Profile `typed-v11` identifies these boundary semantics.

Purity is enforced at the host interaction boundary, not by a spelling blacklist.
Local identifiers such as `open` grant no capability. Module support comes from
Monty. The compute pool exposes only its declared input values and exact-money
functions; undeclared host/OS interactions fail. Print output is discarded. Each
invocation owns isolated execution state and successful workers are reset.

Callback calls now obtain argument binding from the upstream Python call binder.
Reachable blocks, branch successors, early returns and implicit `None` come from
`monty-analysis::function_flow`, backed by the semantic index. Plasm consumes these
facts to construct reviewed scopes; it does not reconstruct callback control flow.
Return annotations are checked before consumer truth conversion. Definition-time
default captures and positional-only parameters follow the same binding path for
named callbacks and projection lambdas.

The root Program ABI and available DAG constructors remain host interfaces:
async/decorated execution, unbounded DAG loops and variadic entity-receiver tuples
have no constructor. These are capability/representation limitations, not claims
that the corresponding Python syntax is invalid. Materialized Python computations
continue to run through Monty's full compute admission.

### Final admission audit: callable and root flow

Explicit compute-call arguments are bound by `monty-analysis::bind_arguments`.
Positional-only and keyword-only parameters, duplicate keywords, missing arguments
and default omission are interpreted by the upstream call binder. Plasm preserves
argument evaluation order, then normalizes the bound signature to required typed
materialization ports. The checks in `PreparedCompute` describe this internal port
ABI; they no longer constrain the public compute signature.

Static Program declarations have no runtime class closure. Method defaults must
therefore be scalar constant metadata; they cannot resolve a name from a later build
scope, defer a definition-time exception, or create shared mutable state. Computed
and mutable defaults require a definition-time execution representation and are
rejected even when the caller supplies an explicit argument. Variadic compute inputs still lack a typed packet-port representation. Compute
invocations remain isolated; no persistent mutable default state is exposed.

The host binds the build receiver through the same upstream binder and consumes
`function_flow` for its reachable statements and return roots. Statement-level branching
returns still lack a DAG return constructor; a conditional return expression stays
intact in the upstream flow projection. Callback value calls also use upstream
binding, followed by the one-row-port and singleton requirements of MapBody.

The remaining AST matches are the following host interfaces, not a second Python
parser or type checker:

| Site | Owned evidence / responsibility |
| --- | --- |
| `python/admission.rs` | Static Program declaration, host receiver and compute marker; no executable class state |
| `python/build_statements.rs`, `statements.rs` | Construct immutable DAG bindings, scoped callbacks and ordered effects |
| `python/*operations.rs`, `projection.rs`, `reductions.rs` | Catalog and rowset API signatures; field, identity, cardinality and resource bounds |
| `python_compute/returns.rs` | Translate catalog-owned annotation references into upstream declarations |
| `python_compute.rs`, `multiple.rs` | Validate normalized materialization ports and checker-produced value contracts |
| `python_program_diagnostic.rs` | Ruff parser recovery for an explicitly unexecuted, untyped prefix sketch |
| AST templates in expression/callback lowering | Construct dependency substitutions; upstream checking still admits resulting Python |

HTTP execute, MCP planning, Node and the evaluator route through `compile_program`.
The internal native-language matrix oracle is not a production admission alternative.
This audit establishes these specific ownership paths, not support for every Python
statement as a DAG constructor. In particular async execution, runtime class state,
variadic row receivers and unbounded effect loops remain unrepresented.

Authored return checking shares the final worker admission declarations. Inferred
nominal evidence is not reused as an incompatible return-construction ABI. A record
may be returned unchanged or constructed as a dictionary; the complete authored
annotation is checked for both representations so mutable containers remain
invariant and Python `Literal` constraints are retained. Runtime refinement
validation still precedes every dependent effect. List expansion stays inside a
whole Monty expression rather than being lowered as a standalone starred operand.

Lexical callback row ports retain singleton materialization when captured by a
nested scope. This role is distinct from an ordinary singleton entity rowset,
which still materializes as a collection unless explicitly consumed as a row.

### Audit verification

Final focused verification on 2026-10-02:

- `cargo test --locked -p plasm-agent-core --lib python_`: 118 passed, one ignored.
- `cargo test --locked --manifest-path plasm-oss/vendor/monty/Cargo.toml -p monty-analysis`: four unit, 20 integration and one documentation test passed.
- Language-matrix filters `typed_return_structures_and_effect_gate`,
  `outer_value_transfer_boundary`,
  `datetime_nullable_guards_cover_scoped_and_recursive_consumers`,
  `nullable_projection_callback_branch_ports`,
  `python_predicate_value_position_closure`, `callback`, and `registry`: 27 passed.
- Workspace formatting and parent/OSS/Monty whitespace checks passed.

The preceding broad matrix run (excluding its duplicate composite property runner)
passed 128 tests and exposed four failures. The final focused run covers all four:
nested row-port capture, conditional root expression flow, retired annotation
error wording, and preserved money-format metadata. The complete workspace suite
and a fresh broad matrix were not rerun after these final corrections. No model
or AppWorld evaluation was launched for this audit.


### Reasonable program extensions (2026-10-02)

- Structural `dict` and `list[dict]` inputs use a mapping view of the actual closed
  record contract. Monty checks annotation compatibility and the body against
  generated TypedDict fields. This does not admit untyped output, grant entity
  authority, or erase missing/null distinctions. Independent compute inputs use
  the same adaptation without joining collections.
- Undecorated Program methods called with `self.method(...)` that reference Plasm
  symbols or `self` elaborate through the existing scoped callback flow. Methods
  with typed inputs and without those references execute as one pure Monty compute;
  local mutable values stay inside Python and only the typed return crosses the
  materialization boundary. Monty binds arguments and reports locals and
  control flow; Plasm supplies independent DAG dependencies and ordered effects
  for workflow methods.
  Methods execute once per invocation. Recursive expansion and caller-local
  capture are rejected; purity restrictions in value contexts remain in force.
- `hasattr` is declared in Monty's bundled typeshed to match its existing runtime
  implementation. It is not a Plasm-side builtin special case.
- Sets have an explicit recursive boundary contract and restore as Python sets;
  their wire arrays do not become Python lists. Equality and hashing ignore
  element order. Domain output encoding traverses sets and mapping records.
  The reviewed boundary profile is version 11.

Abstract-fixture witnesses: `python_reasonable_structural_input_and_set_capture`,
`python_program_helpers_preserve_independent_ports_and_effects`,
`supported_attribute_presence_builtin_is_checked_upstream`, and
`sets_preserve_order_independent_equality_and_hashes`.

Verification for these extensions:

- Agent-core `python_`: 121 passed, one manual viewer test ignored.
- Final structural-input/set-capture witness after value-column precedence fix:
  passed, including independent inputs and dictionary-valued column passthrough.
- Core value tests: 74 passed. Broad core run: 1,077 passed, with the teaching-size
  guard initially failing; after compacting teaching, all 142 prompt-render tests
  passed. The card remains 4,491 bytes, below its unchanged 4,500-byte budget.
- Final declaration matrix filter: three passed, including live helper execution
  and complete-module negative examples.
- Workspace formatting and OSS/Monty whitespace checks passed.

No AppWorld/model run or dependency change was made for this extension. These are
compiler/runtime fixture results, not measured task-success improvements.
