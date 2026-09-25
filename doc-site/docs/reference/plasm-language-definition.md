# Plasm language definition: Python DAG programs

Plasm accepts Python source describing a typed, deferred execution graph. The host
parses source; it does not execute the outer Python module. The sole root is a
class directly derived from the supplied `Program`, with `build(self)` and an
explicit return. There is no alternative source grammar or parser retry.

The canonical executable teaching contract is
[`python-plasm-dag.txt`](../../../crates/plasm-core/src/prompt_render/assets/python-plasm-dag.txt).
It defines the supplied interface with signatures, semantic comments and complete
examples. Domain declarations come from CGS and incremental session exposure.

```python
class Report(Program):
    @compute
    def line(self, row: Row) -> str:
        return f"{row.title}"

    def build(self):
        rows = e1.query().where(lambda row: row.owner == "alice")
        selected = rows.select("title")
        rendered = self.line(selected)
        return rendered
```

This example is compiled against the abstract language matrix with LangItem as e1.
In another session, e1 and its fields must match that session's declarations. Agents must not invent catalog calls from examples.

## Types and source operations

The session supplies e# entities, r# relations, m# methods and v# semantic value
aliases. Entity/method/relation symbols remain append-only. Value aliases preserve
catalog ownership and named domain identity, even when primitive representations
match. Classes and aliases on the teaching card are references, not code to copy,
import or instantiate.

Use declared `.get(...)`, `.query(...)`, `.search(...)` and m# signatures. Simple
Get takes the identity positionally; compound Get takes its exact named keys;
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
control transport paging; use `take` to bound the requested rows.

A singleton proof permits scalar field extraction; plural traversal uses
`flat_map`. A projected id-shaped object does not acquire receiver authority.

## Effects, iteration and rendering

Mutations participate in the reviewed parent DAG even when their results are not
returned. Dependent reads observe writes. Fanout preserves the captured row and
its scope. Bounded `iterate` re-observes after each effect; bound exhaustion without
the stopping predicate is an error. Failures and cancellation do not roll back
completed external writes.

`@compute` methods receive materialized `Value[eN]`, `Row`, or lists of those
values. A scalar annotation executes per row; a list annotation executes once for
the collection. Compute is a checked expression subset in bounded Monty and
currently returns str. The outer class is never instantiated in the worker.
Python multiline, raw and formatted string forms replace tagged heredocs and
source-level Jinja rendering. A formatting expression becomes a reviewed render
node through its declared compute dependency.

## Corrections and delivery

Syntax failures report Python byte positions. Admission/type and plan failures
retain their stage in the shared needs_fix envelope. Corrections refer to Python
calls and current domain declarations. Recovered syntax is never evidence that a
prefix executed. Reject replay retains Python indentation and literal whitespace.
Repair in the same logical session and preserve selection criteria and evidence
of completed writes.

The first exposure supplies the library reference and domain card. Extensions
send new aliases and complete replacement declarations for changed entities.
Unchanged declarations are not resent. Profile/catalog changes cannot silently
reinterpret existing symbols. Unsupported declarations must have an explicit
availability record; they cannot be silently dropped.

## Validation authority

The original rowset, effect, coverage and identity laws remain in force.
`plasm_language_matrix` pairs Python programs with native semantic fixtures and
executes both against shared assertions. Historical syntax retained for that
oracle is not an accepted production frontend. See
[the semantic coverage ledger](research/python-dag-slice/LOWERING.md).
