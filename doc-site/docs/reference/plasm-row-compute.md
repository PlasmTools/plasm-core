# Row compute in Python Plasm

The outer Python class declares a typed DAG. Catalog reads acquire rows; row methods transform the materialized result. `@compute` declares a bounded rendering node with explicit input dependencies.

See the [language definition](plasm-language-definition.md) for the supported interface and the semantic matrix for executable coverage.

| Operation | Python interface | Meaning |
|---|---|---|
| Catalog selection | `eN.query(owner="alice")` | Arguments use declared CGS input lanes |
| Row filtering | `rows.where(lambda row: row.score >= 10)` | Refine already acquired rows |
| Projection | `rows.select("id", "title")` | Retain typed columns |
| Ordering and bounds | `rows.order_by("score", descending=True).take(10)` | Order, then bound output |
| Aggregation | `rows.aggregate(total=agg.sum("score"))` | One synthetic typed row |
| Grouping | `rows.group_by("owner", count=agg.count())` | One row per group |
| Deduplication | `rows.distinct("id")` | Explicit duplicate removal |
| Relation fanout | `rows.flat_map(lambda row: row.rN)` | Declared relation per receiver |
| Mutation fanout | `rows.flat_map(lambda row: row.mN(...))` | Reviewed effect per receiver |
| Rendering | `self.report(rows)` | Materialize and invoke a declared `@compute` method |

Symbols and signatures come from the current session. These fragments illustrate operations; a submitted program must contain a complete `Program` subclass.

A rowset is not a Python list. The compiler accepts only the taught outer operations; ordinary Python loops and arbitrary calls do not declare reviewed nodes. A singleton field can supply a scalar argument. An unbounded plural field cannot silently become one scalar. `take(1)` fails on empty singleton use and does not establish that the chosen row is the intended identity.

Rendering accepts Python string literals, including multiline and raw strings. Runtime formatting belongs in a typed `@compute` method. `Value[eN]` or `Row` means per-row rendering; `list[Value[eN]]` or `list[Row]` means one collection render. Nested structures, arrays, enum constraints, exact money and reference types remain typed.

Writes execute even when unreturned. Subsequent dependent reads must re-observe changed state. Failure or cancellation does not imply rollback. Bounded iteration declares an explicit `max_steps` and fails if its stop condition remains false at exhaustion.

The original source syntax survives only as an internal semantic oracle in parity tests. It is not accepted by agent-facing hosts.
