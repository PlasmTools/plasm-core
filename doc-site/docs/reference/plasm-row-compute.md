# Row compute (artifact plane)

Plasm programs operate on **rows**: JSON objects materialized from catalog queries, relation hops, and prior bindings. **Row compute** is postfix syntax that transforms those rows in the plan executor (in-memory), distinct from predicates on the **catalog plane** that compile to HTTP/CML.

See also [plasm-language-definition.md](plasm-language-definition.md) for full grammar and binding rules.

## Two planes

| Plane | Surface | When to use |
|-------|---------|-------------|
| **Catalog** | `e1{state="open"}` on a query/get | Reduce data at the API; predicates become query parameters or CML filters. |
| **Row** | `rows.filter{owner="alice"}` or `rows.filter(owner="alice")` or `rows.with{age: (now - updated_at)}` | Filter, derive columns, sort, group, or aggregate rows already fetched into the session artifact. |

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

## Derived columns (`.with`)

Add computed columns to each row while keeping the upstream **entity identity** (relation-dot continuation still works on the binding):

```text
stale = issues.with{age_days: (now - updated_at)}
boosted = items.with{boost: score * 2}
tagged = items.with{tag: owner + owner}
labeled = items.with{label: when(len(owner)>0, owner, title)}
```

**Surface:** `.with{col: expr, …}` or `.with(col: expr, …)` — comma-separated `name: expr` pairs inside the braces/parens. Column names are output labels (wire-style identifiers); expressions reference catalog **field wire names** on the current row.

**Expression language (v1):**

| Form | Meaning |
|------|---------|
| `field` / `parent.child` | Field path on the row |
| `null`, `true`, `false`, integer, float, `"text"` | Literals |
| `now` | Catalog-plane UTC clock (not a field lookup — a catalog field named `now` is shadowed) |
| `a + b`, `a - b`, `a * b`, `a / b` | Arithmetic (`+` also concatenates strings) |
| `len(field)` | String length |
| `when(lhs op rhs, then, else)` | Conditional; `op` is `=`, `!=`, `>`, `<`, `>=`, `<=` |

**Temporal subtraction:** `(now - updated_at)` or `(updated_at - created_at)` yields a non-negative **integer day count** when operands are temporal fields or `now`. Use these in filters or further `.with` columns (e.g. `when(now - updated_at > 14, 1, 0)`).

**Money:** `*` / `/` / `+` / `-` on money columns follow catalog money typing (same-currency rules; cross-currency arithmetic fails at runtime).

**Disambiguation:** `.with{` / `.with(` is row compute. Identifiers such as `.join(…)` or `.open(…)` without a leading row-compute verb are **not** postfix operators — they remain path/relation surface and fail row-compute lowering.

**Operator precedence (v1):** inside each expression, `*` and `/` bind tighter than `+` and `-` (e.g. `score * 2 + 1` is `(score * 2) + 1`). Parentheses override.

## Dedupe and distinct

Remove duplicate rows while preserving order (first occurrence wins):

```text
unique = items.dedupe(owner)
unique_by_pair = items.dedupe(owner, team)
all_distinct = items.distinct()
```

- **`.dedupe(field, …)`** — unique on the listed key columns (catalog wire names).
- **`.distinct()`** — unique on the **full row** (all visible columns).
- **`.distinct(field, …)`** — same as `.dedupe(field, …)` (alias sugar).

Both forms lower to the same Polars `unique_stable(…, First)` path. Like `.sort`, dedupe/distinct are **terminal** for relation-dot continuation on that label.

## Chaining order

Postfix applies left-to-right on the written expression (`a.limit(10).sort(x)` → sort after limit). Recommended SQL mental model:

```text
source → .filter{…} → .with{…} → .dedupe(…) → .group_by(…) → .sort(…) → .limit(n) → [fields] → <<TAG
```

`group_by` and `aggregate` change the row schema (terminal for relation-dot continuation on that label). After `group_by`, output columns are the **group keys** plus aggregate names (`n`, `total`, …). A chained `.sort(n, desc)` sorts on those aggregate columns — not on fields of the original catalog entity.

`filter`, `.with`, `sort`, and `limit` on a binding that still carries the catalog entity shape validate against that entity’s fields. `.with` **adds** columns but must not break entity identity (you cannot project away required identity fields inside `.with`).

## Relation-dot vs row compute

| Syntax | Meaning |
|--------|---------|
| `issues.labels` / `issues.r#` | Catalog **relation** hop (may fan out HTTP per source row when `issues` is plural). Use opaque **`r#`** or the relation **wire** from teaching exemplars — not a filter/param wire after `.` unless it is the declared relation. Scoped `query_scoped_bindings` map parent fields into capability params with **catalog typing** — see [Relation binding proofs](plasm-language-definition.md#typed-semantic-core-lean-oriented-sketch) (dry `plasm` on hole-IR alone is not enough for live `plasm_run`). |
| Paginated parent `issues = e1{…}` | **All API pages** are materialized into the binding by default (runtime page cap). Use `.limit(n)` or `.page_size(n)` on the read to bound; MCP `page(pgN)` on a later binding only pages **that** step — not a substitute for a full-repo histogram before `group_by`. |
| `issues.filter{owner="alice"}` | **Row compute** on materialized issue rows |
| `issues.with{age_days: (now - updated_at)}` | **Row compute** — derived column on each issue row |
| `issues => { … }` | **Derive map** over rows — not a relation hop |

See [plasm-language-definition.md](plasm-language-definition.md#binding-rhs-shapes-label). **`=>`** is only for derive maps and `for_each` on bindings; relation hops use `.r#`/wire, not `=>`.

## Not in v1

- OR/NOT in row filters; `.having{…}`; `.derive()` postfix.
- HTTP push-down of row filters (optimizer may add later without changing surface meaning).
- `rows{…}` as a row-local filter shorthand.
- Surface `join` / equi-join between bindings (row pipeline rejects join-from-surface).

## Execution engine

Row compute lowers fused [`ComputeOp`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/plasm_monad/payload/compute.rs) chains to a [`RowPlan`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/row_plan/plan.rs) IR in `plasm_core::row_plan`, then executes through a Polars-backed adapter in `plasm_runtime::row_compute`. Collect barriers (program return, paging, invoke-arg holes, render) are the only legal materialization points — render and derive remain outside the fused pipeline.

## Federation

Row compute field paths are validated against the **qualified entity** of the upstream surface or binding (same catalog as `e#` in the teaching table). Use the correct session `e#` when the same wire entity name appears in multiple catalogs.
