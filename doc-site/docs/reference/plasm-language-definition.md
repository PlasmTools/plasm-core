# Plasm language definition: Python DAG programs

The [collection coverage contract](collection-coverage-contract.md) specifies producer evidence, operator transfer, consumer demands and recovery separation. Its implementation inventory and CE-01–CE-07 obligations remain explicit; the design is not a claim of completed matrix coverage.

Plasm accepts Python source describing a typed, deferred execution graph. The host
parses source; it does not execute the outer Python module. The sole root is a
class directly derived from the supplied `Program`, with `build(self)` and an
explicit return. There is no alternative source grammar or parser retry.

The normative inner-Python boundary is [Python compute contract](python-compute-contract.md).
It assigns Python semantics to the pinned upstream checker and Plasm laws to the
DAG boundary. Its implementation ledger records current conformance gaps.
The [scoped composition contract](python-scoped-composition.md)
defines recursive capture, typing, cardinality, bounds and effects. Bounded maps
execute recursively with typed captures, nested arrays and ordered effects.
The law ledger separately records remaining conformance obligations.
The [literate conformance design](python-conformance.md) separates
constructor contracts, boundary laws, generated properties and live evidence.
Its law blocks are parsed directly by the matrix. Finite evidence covers observed
row embedding; general constructor/type/effect products remain explicit obligations.

The compact teaching card is
[`python-plasm-dag.txt`](../../../crates/plasm-core/src/prompt_render/assets/python-plasm-dag.txt).
It teaches supported idioms and complete examples. Domain declarations come from
CGS and incremental session exposure; their reference notation is not executable
Python.

```python
class Report(Program):
    @compute
    def line(self, row: Row) -> str:
        return f"{row.title}"

    def build(self):
        rows = e1.query().where(lambda row: row.owner == "alice")
        selected = rows.select("title")
        rendered = selected.map(lambda row: {"line": self.line(row)}, max_parents=256)
        return rendered
```

This example is compiled against the abstract language matrix with LangItem as e1.
In another session, e1 and its fields must match that session's declarations. Agents must not invent catalog calls from examples.

The four outer build statement constructors have a [closed rule registry](python-build-constructors.md).
The matrix checks its production inventory, live witnesses, rejection diagnostics
and documentation-erasure equivalence. This does not register every expression
or class/decorator rule. The seven catalog capability kinds have a separate
[typed dispatch registry](python-catalog-constructors.md), with
IL-checked live witnesses and rejecting programs.
[Relation navigation](python-relation-constructors.md) has a separate
recursive typed evidence gate for root and correlated uses.
[Scalar DAG constructors](python-scalar-constructors.md) register literal
text, captured formatting and field extraction separately from
[recursive literal operands](python-literal-constructors.md) and inner
compute expressions. [Program/class declarations](python-declaration-constructors.md)
have a closed admission inventory and full-module rejection tests.
Named projection lambdas have a [closed expression registry](python-projection-constructors.md) with recursive typed IL witnesses and diagnostic checks; operator/type products remain explicit coverage obligations.

Outer filter predicates have [closed boolean and comparison inventories](python-predicate-constructors.md), with typed IL witnesses and membership-boundary rejections.
Predicate and value expressions share recursive lowering across records,
projections, scoped maps, filters, iteration conditions, and typed call inputs.
`any`/`all` require complete collections. Filters, quantifiers and iteration stop conditions apply Python truth testing to scalar predicate values; the resulting IL predicate is Boolean. Lazy
operands that introduce reads become conditional scoped subgraphs: both branches
are reviewed, but only the selected branch executes. Their value merge enforces
exactly one result and preserves joined types without granting receiver authority.
Explicit `is None` / `is not None` tests are total Boolean predicates. Non-null
proofs for immutable references remain local to dominated lazy branches and
refine generated compute inputs through a checked value operation. Proofs do
not escape merges; missing fields are still errors. See the
[outer value contract](python-outer-value-contract.md#null-tests-and-branch-refinement).


Aggregate descriptors have a [closed seven-function registry](python-aggregate-constructors.md), typed IL witnesses and argument-diagnostic checks.

## Types and source operations

The session supplies e# entities, r# relations, m# methods and v# semantic value
aliases. Entity/method/relation symbols remain append-only. Value aliases preserve
catalog ownership and named domain identity, even when primitive representations
match. Classes and aliases on the teaching card are references, not code to copy,
import or instantiate.

Use declared `.get(...)`, `.query(...)`, `.search(...)` and m# signatures. Simple
Get accepts one identity positionally or as `identity=...`; both spellings lower
to the same typed identity and reviewed plan. Supplying it twice, omitting it, or
using an unknown keyword is an admission error. Compound Get takes its exact named keys;
pathless singleton Get takes no arguments. Scope, backend selection, controls,
arguments and payload remain distinct catalog input lanes. Optional omission is
not permission to pass None. Nested records, arrays, enum members, temporal
profiles, exact money and entity references retain their native contracts.

Tagged input unions retain their catalog discriminator and branch-specific fields.
The card serves root alternatives as `@overload` signatures with `Literal` tags;
nested alternatives are `TypedDict` types joined with `|`, including inside arrays.
Supply exactly one variant in ordinary Python keyword arguments or dictionaries.
The compiler rejects mixed branches, unknown tags and missing required fields,
and applies the same primitive/domain validation and coercion laws as other inputs.
Scope and argument fields remain present alongside the selected payload branch.
Transport field remapping is applied after validating the logical branch.

`Rows[T]` is a deferred rowset. Filtering, projection, sorting, limits, distinct,
union, grouping and aggregation build nodes. They do not run Python iteration.
`where` supports typed comparisons, `and`/`or`/`not`, literal list/tuple
membership, one-column rowset membership and substring tests. `select` aliases
may name fields or supply typed scalar lambdas for arithmetic, `len` and
conditional expressions. Attach `page_size` directly to a query/search call to
control transport paging; use `take` to bound the requested rows. Collection
consumers (compute and record-array inputs) acquire their complete source expression;
a presentation page must not truncate it. Fixed backend page coordinates remain stable.

A singleton proof permits scalar field extraction; plural traversal uses
`flat_map`. A projected id-shaped object does not acquire receiver authority.

Scoped record fields may embed a bound rowset (`{"items": rows}`) as a complete
observed array. The binding is captured explicitly; absence is retained and no
receiver authority is granted to the embedded values.

## Effects, iteration and rendering

Mutations participate in the reviewed parent DAG even when their results are not
returned. Dependent reads observe writes. Fanout preserves the captured row and
its scope. Bounded `iterate` re-observes after each effect; bound exhaustion without
the stopping predicate is an error. Failures and cancellation do not roll back
completed external writes.

`@compute` methods receive materialized `Value[eN]`, `Row`, or lists of those
values. A scalar annotation executes per row; a list annotation executes once for
the collection. The normative compute language follows the pinned Monty profile
and upstream checker, subject to explicit Plasm boundary policy. The current
implementation uses upstream admission. Compute returns preserve primitive,
nominal `vN`, record, array and union contracts; validation occurs before dependent
effects. The outer class is never instantiated in the worker.
Python multiline, raw and formatted string forms replace tagged heredocs and
source-level Jinja rendering. A formatting expression becomes a reviewed render
node through its declared compute dependency.

## Constructed value records

An outer dictionary constructs one typed value record, including when returned
directly from `build`. Nested dictionaries and arrays retain their recursive
value contracts; referenced fields retain CGS domains and nullability. For example,
`return {"id": item.id, "title": item.title}` declares dependencies on those
singleton fields. A plural scalar field is rejected; an empty bounded singleton
fails at execution. Embedding an explicit rowset produces an array, including `[]`
for an empty rowset, without removing the enclosing record.

Constructed records may be bound, projected, embedded in another record or passed
to `@compute` as `Row`. They are values: copying identity fields grants no entity,
relation or mutation authority. Their dependencies remain part of the reviewed DAG.

## Corrections and delivery

Syntax failures report Python byte positions. Admission/type and plan failures
retain their stage in the shared needs_fix envelope. Corrections refer to Python
calls and current domain declarations. Recovered syntax is never evidence that a
prefix executed. Reject replay retains Python indentation and literal whitespace.
Repair in the same logical session and preserve selection criteria and evidence
of completed writes.

The first exposure supplies the semantic card and domain card. Extensions
send new aliases and complete replacement declarations for changed entities.
Unchanged declarations are not resent. Profile/catalog changes cannot silently
reinterpret existing symbols. Unsupported declarations must have an explicit
availability record; they cannot be silently dropped.

## Validation authority

The original rowset, effect, coverage and identity laws remain in force.
`plasm_language_matrix` compiles Python programs against abstract semantic
fixtures and checks planning and live behavior directly. Historical source is
not a comparison oracle. See the
[semantic coverage ledger](../../../crates/plasm-e2e/tests/plasm_language_matrix/PYTHON-COVERAGE.md).
The original obligations and additional scoped composition laws have separate
coverage totals; both are required before claiming complete scoped semantics.

### Rowset body composition

`flat_map` uses the scoped DAG runtime and can nest with typed ancestor captures.
Captured f-strings lower to compute nodes; ordinary supported Python methods,
conversions and format specifications retain explicit dependencies. The optional
`max_parents` lowers the 65,536 default; all maps share a 65,536 occurrence budget
and a depth ceiling of 16. See [the scoped composition contract](python-scoped-composition.md).

### Compact teaching contract

Agent-facing teaching is a semantic delta over Python, not a library header. Serve
one compact card for DAG construction, rowset/cardinality laws, compute and effects;
add exact typed domain signatures incrementally. Preserve domains, constraints,
nullability and union/recursive shapes. Do not repeat value descriptions on inputs
that share them or expose internal acquisition hashes. Prerequisite guidance uses
response-local acquisition labels and grouped, explicit input/output bindings.
Domain teaching uses compact, non-executable signatures: `eN.method(...)` for root calls and `row.method(...)` for receiver calls under an entity heading. Distinct CGS comments remain; identical comments may be grouped with explicit method owners. Those labels are explanatory, not executable session symbols. Full specifications
and implementation details belong in reference documentation.

Value symbols retain nominal ownership in structured metadata; cards omit registry keys and runtime `provides`/materialization bookkeeping. `@profile` abbreviates a sole profile constraint; compound constraints remain JSON. Semicolons separate consecutive uncommented fields. Get accepts declared CGS invocation inputs as keyword arguments alongside identity. Producer fields create ordinary typed DAG dependencies; required inputs must be supplied on the call. Deployment-owned authentication is not a program input unless CGS declares it. Internal hydration inherits captured execution bindings. Unbounded arrays use `list[T]`; bounded arrays retain both endpoints with `*` for an unbounded endpoint. These forms are shared production MCP/session teaching, not harness rewrites.


Typed `@compute` results preserve Plasm domain and recursive contracts; see [the return contract](python-return-contract.md).

### Recursive compute input adaptation

Structural and literal values can feed typed compute without nominal catalog
ownership. Nested fields preserve their contracts and provenance. For reasonable
agent-authored programs, a scalar annotation accepts one unambiguous typed value
column through the bounded runtime loop; a matching array cell remains one value,
while `list[T]` can collect a column of `T`. Inputs and outputs validate at the
boundary; this grants no entity/effect authority. This is an untaught admission
convenience: shared prompts and cards retain canonical row-based compute.
Normative rules and executable evidence: [compute contract](python-compute-contract.md).

### View relation completeness

A composed view combines the coverage of its executed inputs. Traversing its
`view_embed` relation preserves that evidence only when every source parent and
every declared relation occurrence is retained. Present empty relations are
complete when their upstream evidence is complete; missing relations are not
empty evidence. Truncation remains partial, and partial or unknown inputs never
become complete through extraction or field hydration. Whole-collection compute
continues to require complete coverage with no outstanding continuation.

The abstract view matrix exercises this boundary directly and through
filter/order/take/projection; see the view relation collection proof in the
Python coverage ledger.

## Python temporal values

Use `from datetime import date, datetime, time, timedelta, timezone` or
`import datetime as dt`; aliases are resolved statically. These imports are
allowed at module scope and inside `@compute`. They grant no filesystem,
network, or arbitrary module access.

Calendar-date domains expose `date`. RFC3339 and Unix timestamp domains expose
`datetime`, retaining their declared wire units separately. Records, arrays,
unions, projections, and compute boundaries preserve temporal types rather than
reducing them to `str | int`. Synthetic temporal returns retain kind and
components; named catalog returns encode their declared transport format.

```python
from datetime import datetime, timedelta, timezone

class Recent(Program):
    def build(self):
        end = datetime.now(timezone.utc)
        start = end - timedelta(days=7)
        return e1.query().where(
            lambda row: row.recorded_at is not None and start <= row.recorded_at < end
        )
```

Temporal expressions lower to ordinary typed Monty compute nodes with explicit
input dependencies. API arguments consume typed values directly; `.isoformat()`
is for explicit text formatting. The pinned Monty implementation is the execution
ceiling, not a promise of full CPython: use `timezone.utc`, not `datetime.UTC`;
`zoneinfo` and IANA timezone rules are not admitted.

Date-only and instant domains are distinct. Instant encoding requires an aware
datetime; no implicit UTC is assigned to a naive authored value. Integer units
come from the catalog. Invalid dates, unsupported precision, and lossy wire
conversions fail rather than truncate. Python supports microseconds; Unix seconds
and milliseconds require exactly representable values.

The former relative-string language (`now`, `today`, `7d ago`, `now-1h`),
natural-language parsing, timestamp-magnitude inference, and predicate syntax
rewrites are retired. Explicit wire scalars remain transport data, not another
computation language. CGS temporal profiles and API codecs remain authoritative.
Clock configuration belongs to the runtime/evaluation host and is not taught in
domain cards.

The pinned interpreter does not preserve `datetime.fold`; explicit `fold=` calls
are excluded from the compute profile. Folded datetime component records are
rejected. Dynamic keyword construction and higher-order datetime constructors
are not certified for fold preservation.

Generator targets are lexical bindings: they may shadow outer values, and the
outer binding is restored when the generator scope ends. Temporal quantifier
captures follow the same rule as ordinary scalar quantifiers.

### Response values and catalog-preserving composition

`iso8601_naive_datetime` describes a timezone-free ISO datetime and exposes Python
`datetime` with `tzinfo is None`. RFC3339 and Unix formats remain aware instants;
`iso8601_date` remains a calendar date. Encoding never invents or discards a timezone.
Use `date.today()` / `datetime.now()` for relative dates rather than guessed years.
A present response value that violates its declared type fails decoding with the
field identified. It must never become a successful null; absent and nullable values
retain their distinct meanings.

Captured arguments resolve against their declared output schema, including catalog
value domains. Union remains terminal under RA-14 and does not grant entity
receiver authority. Ordinary field
selection preserves existing identity under RA-10; constructed records do not.
Correlated scalar captures consume their typed schema independently of entity authority.
Search results are candidates; effects must use the intended observed identity.
Correction does not license replacing a selection predicate with a different one.

CML `datetime_format` requires an explicit `wire` input encoding. It formats naive
datetimes without timezone conversion and aware/Unix instants in UTC. Hydration
propagates response-contract decode errors; unavailable detail reads are distinct
from malformed present values.

### Ordered mutation occurrences

Writes follow program order. Within a mutating row application or scoped map,
occurrences dispatch serially in source order. The first failed occurrence stops
the enclosing operation: successful prefix effects remain committed, the failed
occurrence retains its typed evidence, and the remaining suffix is undispatched.
Preflighting the full input does not authorize dispatch past a failure. Concurrent
reads may drain already admitted work. A failure never implies rollback.
Receipts describe that execution; later programs may read or write in the same
session without a reconciliation gate. A new execution neither resumes nor rolls
back the failed execution. Choose subsequent effects from the task and observed
evidence, including completed and unresolved dispatches.
