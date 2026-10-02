# Typed Python return boundary

`@compute` returns a Plasm value. Monty owns expression semantics; Plasm resolves
annotations, seals recursive output contracts and validates values before exposing
them to the DAG. Returning data never grants entity or effect authority.

| Annotation | Contract |
|---|---|
| `bool`, `int`, `float`, `str`, `None` | Builtin scalar/null; int is signed 64-bit, float finite |
| `vN` | Exact session domain, constraints, catalog identity and hash |
| `Row` | Actual input row shape; in a multi-input return, the inferred returned record shape, including observed presence |
| `Value[eN]` | Declared entity data fields, without methods or traversal authority |
| `T.field` | Field contract of a record type, e.g. `Row.price` |
| `list[T]` | Recursive array value |
| `T | U` | Union; `T | None` preserves T with nullability |

A `Program` compute method may omit its return annotation. Monty infers through
the complete body against its typed inputs, preserving field domains and named
record fields. In a multi-input return, `Row` refers to the inferred returned
record, not the dependency packet or whichever input was declared first. The
authored annotation remains checked; `list[Row]` cannot return scalars. For example:

```python
class Summary(Program):
    @compute
    def describe(self, rows: list[Row]):
        return [{"key": row.id, "title": row.title} for row in rows]

    def build(self):
        return self.describe(e1.query())
```

Bare `dict` and `list[dict]` require an inferred structural field contract; they
do not authorize untyped output. A nested `def` in `build` is a scoped
callback, not a compute method; declare `@compute` on the class and call `self.method`.

These forms compose recursively. A record can be returned as an input record or
constructed as a matching Python dictionary. A field absent from an observed
record stays absent; it is not manufactured as `None`. Python objects outside this
value algebra, tuples, sets and dictionaries with non-string keys are rejected.
JSON domains admit JSON integers through u64 without float conversion; the named
integer domain remains i64. Cycles/deep graphs, nonfinite numbers and oversized
outputs fail within bounded decoding. Domains resolve independently against their
own pinned catalogs in a federated session; catalog graphs are not merged.

Compute results are consumed directly. Scalars are Python scalar values, arrays
are array values, and records expose their declared fields. A per-row compute
retains its input cardinality; a collection compute emits one value. `content`
is an ordinary record field name, never a reserved accessor. Arrays do not acquire
entity identity. Completeness, cancellation and reviewed effect rules are unchanged.

## Consuming Python

```python
class Adjust(Program):
    @compute
    def increment(self, row: Row) -> Row.count:
        return row.count + 1

    def build(self):
        source = e1.get("000123")
        result = self.increment(source.select("count"))
        return result
```

`Row.count` retains the source's named integer domain. A served `vN` can explicitly
select a destination domain; its constraints are checked on the returned value.
For writes, pass the singleton scalar `result` directly to the served mutation
parameter, or select a declared field from a record result. An invalid return
fails before any dependent mutation is dispatched. Previously completed,
unrelated effects retain their existing failure semantics.

A scalar parameter requires both a scalar-cell contract and a proven singleton.
A per-row scalar compute over plural input rejects at admission; use a bounded
`map` for effects on each selected row. Corrections must not teach a magic
`.content` accessor or the retired path-expression/heredoc surface.
`compute_values_have_no_reserved_content_accessor` and
`plural_compute_values_cannot_fill_scalar_parameters` lock these boundaries.

## Executable evidence

The BC-03 `typed_returns` property shares the exhaustive CGS FieldType inventory
with `primitive_reductions`: 20 domain fields × forward/reverse/empty inputs = 60
live programs. Each passes a nullable named-domain result through two Monty nodes,
checks exact values and output contracts, complete coverage and absence of effects.
The count case also rejects a forged output-domain contract during dry validation.

`typed_return_structures_and_effect_gate` additionally covers record pass-through,
dictionary construction, arrays of records, unions, nested arrays/records,
observed absent versus null fields, cross-catalog constrained domains and local
annotations, unknown annotations, static return mismatches, valid mutation
arguments and invalid output rejection before mutation IO. Codec unit tests cover
lossless large integers, unsupported representations, nonfinite values and budgets.

The pinned [Monty profile](python-compute-profile.md) seals this boundary. Earlier text-only reviewed
computes are not silently replayed under these rules. Text rendering remains the
ordinary `str` case.

## Outer expressions

Outer conditional unions and arithmetic now follow the
[outer value transfer contract](python-outer-value-contract.md). Selection preserves
complete branch domains; arithmetic produces storage types without unproved
constraints. First/last reductions retain their input contracts. Arbitrary
constructor/type/effect products remain explicit matrix obligations.
