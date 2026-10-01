# Outer selection and arithmetic contracts

Outer root values, nested record/array elements, named projections and read/write
arguments share `PlasmDataValue::Expression` and `ValueOperation`. Its transfer
algebra uses `ValueContract::{join, arithmetic, data_value, field}`; it is separate from
Monty's inner Python semantics. The runtime evaluates that closed algebra over materialized
values, without coercing heterogeneous branches into one storage type.

## Conditional selection

`a if boolean_expression else b` has the union of the complete contracts of `a` and `b`.
Equal contracts collapse; nested unions flatten; null adds nullability. Different
domains remain distinct alternatives even when their storage types coincide.
Records, arrays, observed presence and catalog pins survive recursively. Selection
creates no entity authority. Only the selected branch is evaluated per row; both
branches remain declared planning dependencies. A missing field is an error when
evaluated, not a null value. Ordinary row comparisons retain three-valued null semantics: unknown selects
the else branch. Explicit None tests are total predicates, as specified below.

Native value variants distinguish arrays from literal strings. `"[1]"`
remains a string beside `[1]`; integers remain integers beside floating values.
Projection, first/last, distinct and downstream typed compute preserve variants.
Union numeric comparison/order is numeric, without rounding integers to floats;
incomparable ordered values fail rather than sorting their serialized text.

## Arithmetic transfer table

The table applies to storage kinds. Named profiles such as digit IDs and Unix
timestamps retain their constraints on selection; arithmetic on their string or
integer storage does not manufacture a new profile-conforming value.

| Operands | Operators | Result |
|---|---|---|
| integer, integer | `+ - *` | integer, checked i64 |
| numeric, numeric | `/` | finite number |
| numeric, numeric with a number operand | `+ - *` | finite number |
| string, string | `+` | string |
| money, money | `money_add(a,b)`, `money_sub(a,b)` | money; currencies must match, including absence |
| money, integer or decimal string | `money_mul(value,factor)`, `money_div(value,factor)` | money; retain currency; reject floats |
| internal Date, Date | `-` | whole elapsed days, truncated toward zero |
| all other pairs | any | admission error |

Arithmetic distributes over union variants: **every** possible operand pair must
be admitted. Nullable operands propagate null. Computed results have no source
domain pin: bounds, enumeration membership, formatting profiles and other catalog
constraints require validation against an explicitly chosen destination domain.
Money retains its currency in the runtime value, not an unproved input domain.
Money amounts use the existing bounded Decimal engine and its precision; this is
not arbitrary precision rational arithmetic. Zero division, overflow, nonfinite
results and currency mismatches fail before dependent effects.

`len` of a string, array or record returns an integer, propagating null; string
length counts Unicode scalar values. It does not stringify arbitrary operands.

```python
class Values(Program):
    @compute
    def retain(self, row: Row) -> Row.value:
        return row.value

    def build(self):
        rows = e1.query()
        choice = rows.select(value=lambda r: r.count if r.flag == True else r.text)
        increased = rows.select(value=lambda r: r.count + 1)
        return self.retain(choice), self.retain(increased)
```

Here `choice.value` retains the two domain alternatives; `increased.value` is a
generic integer. To assert a destination constraint, return a served `vN` from a
typed compute; its return validator proves that constraint before effects.

## Executable evidence and limits

BC-03 property `outer_values`: 6 branch kinds × 6 branch kinds × both decisions,
seven arithmetic/laziness cases, four negative-admission cases, four mixed-column
composition cases, two structural record/array cases and nine arithmetic/type-failure effect gates. Expectations are
fixture-authored. Core tests enumerate eight operand kinds × eight kinds × four
operators × two nullability states, and verify domain-preserving joins and union
distribution. Runtime tests cover exact integers, nested JSON versus strings,
currency and numeric failures. These are finite products, not a claim of arbitrary
constructor closure or new temporal-profile/duration algebra.

## Recursive consumption

Immutable aliases reuse the same DAG node. Records (including `{}`), arrays,
scalar literals, arithmetic, `len`, comparisons, boolean combinations and choices
compose recursively. Projection lambdas bind their row cursor; root expressions
use one explicit unit row. Arguments consume the resulting typed values; they do
not reinterpret arithmetic as string syntax. A singleton field requires singleton
proof, with a runtime empty check. Constructing a record confers no receiver
identity or effect authority. Expression depth is bounded at 64.

Choice and boolean operands may reference declared dependencies, but cannot
introduce calls that would be hoisted out of a lazy branch. Such calls fail
admission explicitly; bind intended unconditional dependencies before the choice.
Only the selected value branch is evaluated. This is not conditional DAG effect
execution.

`value_recursive_*` matrix cases exercise roots, bindings, nested records/arrays,
projections, read arguments, inline and bound write arguments, empty scoped
records, membership and lazy errors. The arithmetic/type product above and its
pre-effect gates continue to apply. The separate `value_closure_*` witnesses
cover value annotations and structural compute contexts under the
[compute boundary contract](python-compute-contract.md).

## Structural field selection

Field access recurses through typed records and observed records. A record union
must declare the requested field in every variant; the result joins those field
contracts. Optional presence is checked when the value is read and is not
converted to nullability. A missing field and a present `None` remain distinct.
The same selector works in root values, projections and capability arguments.
Its dependency traversal retains the selected field's provenance, including
sensitive-domain policy, without conferring receiver authority.

The `value_closure_*` live matrix and
`structural_field_selection_preserves_presence_and_union_types` lock this rule.

## Python branch evidence

Monty owns Python predicate semantics and control-flow narrowing. Plasm does not
reinterpret None tests, Boolean composition, comparisons or `isinstance` using
its own fact algebra. The synchronous `monty-analysis::analyze_branches` API
observes typed subjects on both successors of a predicate without executing it.

Plasm maps those subjects to immutable captured DAG references. Lazy branches
retain evidence only inside the selected scope; it cannot escape a merge or prove
anything about a later, independently evaluated call. A selecting row filter
applies true-branch evidence to its successor schema. The schema is re-derived
from the reviewed predicate source and input contracts during plan validation.

The internal `Refine` value operation intersects evidence with the original
contract and checks the materialized value. It preserves domain identity, temporal
wire representation, recursive shape and field presence. It cannot grant receiver
authority or turn an unobserved field into a present field. This is an IL operation,
not taught Python syntax. Unsupported or unresolved analysis remains an explicit
admission error.

Deferred dependencies are identified before lowering lazy successors. Each
successor is lowered with its selected branch evidence, including when the lazy
expression occurs inside a container. Materializing a Boolean port retains the
original predicate as the static evidence source, while runtime evaluates it
once. Logical capture provenance connects scalar bindings to their internal
storage ports without exposing those ports to Python.

Executable witnesses include `branch_refinement_is_shared_across_value_domains_and_nested_fields`,
`datetime_nullable_guards_cover_scoped_and_recursive_consumers`, and
`datetime_nullable_guards_reject_unproved_or_leaked_facts`. These cover filter
successors, lazy branches, nested records and rejection of leaked evidence.

### Exact-money Python library

The arithmetic functions above and `money_compare(left, right)` are pure host bindings available in outer expressions and
`@compute` bodies. Python executes calls and argument evaluation; Plasm's existing
money kernel owns decimal arithmetic. Positional and keyword arguments are equivalent.
Arithmetic results have the Money storage contract and currency, not the input's domain pin.
Comparison returns -1/0/1 and accepts Money or an exact integer/decimal string
on the right; currency conflicts fail.
The normal destination validator applies before effects. Ordinary Python operators
on the mapping carrier do not implement money arithmetic. No Decimal import or
arithmetic dunder dispatch is implied. A compute turn allows at most 4096 host
suspensions; memory, execution-time and output budgets still apply. The bindings
cannot read catalogs, hydrate rows, or perform effects. Invalid arguments or
arithmetic failures raise `ValueError` in Monty; uncaught exceptions fail the
compute. A caught exception does not bypass destination validation.

Materialized contracts are a sound abstraction of the inferred type graph. Literal
and truth-value exclusions that have no Plasm representation retain their enclosing
positive type (for example `str & ~Literal["skip"]` becomes `str`). This cannot
strengthen a proof or admit an unsafe access. Representable union and nullability
refinements remain intact; unresolved positive types remain admission errors.
