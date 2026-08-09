# Row compute (artifact plane)

Plasm programs operate on **rows**: JSON objects materialized from catalog queries, relation hops, and prior bindings. **Row compute** is postfix syntax that transforms those rows in the plan executor (in-memory), distinct from predicates on the **catalog plane** that compile to HTTP/CML.

See also [plasm-language-definition.md](plasm-language-definition.md) for full grammar and binding rules.

## Two planes

| Plane | Surface | When to use |
|-------|---------|-------------|
| **Catalog** | `e1{state="open"}` on a query/get | Reduce data at the API; predicates become query parameters or CML filters. |
| **Row** | `rows.filter{owner="alice"}` or `rows.filter(owner="alice")` | Filter, sort, group, or aggregate rows already fetched into the session artifact. |

Use catalog filters when the API supports them and you want fewer round-trips. Use row filters when refining a binding, combining results from multiple steps, or when the field is not a query parameter.

## Row filter

**Equivalent forms:**

```text
open = items.filter{owner="alice"}
open = items.filter(owner="alice")
```

- Comma-separated clauses are **implicit AND**.
- Comparison operators match entity brace queries: `=`, `!=`, `>`, `<`, `>=`, `<=`, `~` (contains).
- v1: flat comparisons only (no OR/NOT/relation exists).

**Disambiguation:** In postfix/program RHS, `.filter` immediately followed by `{` or `(` is row compute. In path expressions, `.filter` followed by an identifier (no `{`/`(`) remains a **relation** name.

**Invalid:** `rows{owner="alice"}` on a binding label — use `rows.filter{…}`.

## group_by and aggregate

**Canonical:**

```text
by_owner = LangItem.group_by(owner, n=count)
by_team = LangItem.group_by(owner, team, n=count, total=sum(score))
```

**Sugar:** `LangItem.group_by(owner)` → `group_by(owner, count=count)`.

**Output shape:** One JSON field per group key (wire/dotted name), plus aggregate columns (`n`, `total`, …). Not a single generic `"key"` column.

**aggregate** without a key applies functions over all rows: `all = items.aggregate(n=count)`.

## Chaining order

Postfix applies left-to-right on the written expression (`a.limit(10).sort(x)` → sort after limit). Recommended SQL mental model:

```text
source → .filter{…} → .group_by(…) → .sort(…) → .limit(n) → [fields] → <<TAG
```

`group_by` and `aggregate` change the row schema (terminal for relation-dot continuation on that label). After `group_by`, output columns are the **group keys** plus aggregate names (`n`, `total`, …). A chained `.sort(n, desc)` sorts on those aggregate columns — not on fields of the original catalog entity.

`filter`, `sort`, and `limit` on a binding that still carries the catalog entity shape validate against that entity’s fields.

## Relation-dot vs row compute

| Syntax | Meaning |
|--------|---------|
| `issues.labels` / `issues.r#` | Catalog **relation** hop (may fan out HTTP per source row when `issues` is plural). Use opaque **`r#`** or the relation **wire** from teaching exemplars — not a filter/param wire after `.` unless it is the declared relation. Scoped `query_scoped_bindings` map parent fields into capability params with **catalog typing** — see [Relation binding proofs](plasm-language-definition.md#typed-semantic-core-lean-oriented-sketch) (dry `plasm` on hole-IR alone is not enough for live `plasm_run`). |
| Paginated parent `issues = e1{…}` | **All API pages** are materialized into the binding by default (runtime page cap). Use `.limit(n)` or `.page_size(n)` on the read to bound; MCP `page(pgN)` on a later binding only pages **that** step — not a substitute for a full-repo histogram before `group_by`. |
| `issues.filter{owner="alice"}` | **Row compute** on materialized issue rows |
| `issues => { … }` | **Derive map** over rows — not a relation hop |

See [plasm-language-definition.md](plasm-language-definition.md#binding-rhs-shapes-label). **`=>`** is only for derive maps and `for_each` on bindings; relation hops use `.r#`/wire, not `=>`.

## Not in v1

- OR/NOT in row filters; `.having{…}`; `.derive()` postfix.
- HTTP push-down of row filters (optimizer may add later without changing surface meaning).
- `rows{…}` as a row-local filter shorthand.

## Federation

Row compute field paths are validated against the **qualified entity** of the upstream surface or binding (same catalog as `e#` in the teaching table). Use the correct session `e#` when the same wire entity name appears in multiple catalogs.
