# Plasm language definition

This document is the **canonical specification** of the user-facing Plasm surface language: path expressions, multi-line programs (bindings, postfix transforms, roots), structured values and heredocs, row-to-text templates, and the **CGS load-time rules** for structured capability inputs. It aligns with the reference implementation in **`plasm_core::expr_parser`** and with program lowering to **`PlasmComp`** ([`compile_plasm_program`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/plasm_dag.rs)).

For API authoring (YAML catalogs, transport), use the OSS documentation **Connect an API → Reference**, or in the Plasm monorepo the skill file `plasm-oss/skills/plasm-authoring/reference.md` (repository root–relative).

**Surface invariants (PLP-*):** executable conformance is locked by `cargo test -p plasm-e2e --test plasm_language_matrix` against `fixtures/schemas/plasm_language_matrix` (not a separate published page).

---

## Monadic execution contract (`PlasmComp`)

Plasm programs compile to a single canonical artifact: **`PlasmComp`** (`plasm_core::plasm_monad`). There is no separate “Plan DAG” user language — the comp **is** the execution contract.

### Bind semantics

Each surface binding `label = rhs` is **`bind`** over the prior comp:

```plasm
issues = e1{status="open"}
labels = issues.r2
summary = labels[title, color]
summary
```

Operators (formal):

```text
pure  : T -> Plasm<T>
bind  : Plasm<A> -> (A -> Plasm<B>) -> Plasm<B>
```

Parallel final roots (`a, b`) are an **applicative product** at return — not nested `bind`.

### Wire shape (MCP / HTTP `_meta.plasm`)

Greenfield consumers read **`comp`** only:

```json
{
  "comp": {
    "version": 1,
    "steps": { "issues": { "kind": "invoke", "operation": "…" } },
    "bind": { "topo": ["issues", "labels"], "deps": {}, "primary": {}, "holes": {} },
    "return": { "kind": "step", "step": "summary" }
  },
  "plan_ux_reflection": { … },
  "run_ref": "pc1"
}
```

`bind.topo` is the phased runner order. **`bind.primary`** witnesses the monadic predecessor for each step. Plan commit ids hash the semantic subset: `version`, `steps`, `bind`, `return`.

### Typed step payloads (`PlasmStepPayload`)

Each entry in `comp.steps` is a tagged serde object (`kind` discriminant). Wire and runner share one schema — no untyped JSON step blobs:

| `kind` | Role | Key fields |
|--------|------|------------|
| `invoke` | Read / action / view surface | `plan_kind`, `qualified_entity`, `ir` **xor** `ir_template`, `projection`, `predicates`, `page_size`, `approval` |
| `pure` | Literal / artifact data | `data` (`PlasmDataValue`) |
| `map` | Row compute (filter, sort, group, derived columns, …) | `compute` (`ComputeTemplate`) |
| `derive` | Per-row map over a source | `derive` (`DeriveTemplate`: `source`, `item_binding`, `inputs`, `value`) |
| `flat_map_relation` | Relation fanout (`>>=`) | `relation` (`PlanRelationTraversal`: `source`, `relation`, `target`, `ir`, `binding_proofs`, `materialize`) |
| `flat_map_effect` | `for_each` side effects | `source`, `item_binding`, `effect_template`, `projection`, `predicates`, `approval` |

Input wiring for templates uses **`bind.holes`** (prior step outputs); row sources for map/derive/relation/effect use **`bind.primary`**. TypeScript mirror: [`apps/plan-dag/src/comp-types.ts`](https://github.com/PlasmTools/plasm-core/blob/main/apps/plan-dag/src/comp-types.ts).

**Evidence chain:** when `PLASM_EVIDENCE_CHAIN=1`, dry-run `plan_commit_id` is recorded in a hash-chained bundle through live execution; see [plasm-evidence-bundles.md](plasm-evidence-bundles.md).

### Monad laws (testable)

- Left identity: `pure(a).bind(f) == f(a)`
- Right identity: `m.bind(pure) == m`
- Associativity: `m.bind(f).bind(g) == m.bind(a -> f(a).bind(g))`

Equality is same typed result and observable semantics — not byte-identical trace logs.

Writes / `for_each` / side effects are **bind-ordered** and not freely reassociable. See `comp_equivalent` / `EffectBarrier` in `plasm_core::plasm_monad`. Co-submitted write or action nodes in one program execute **sequentially in program order** even when they share no data dependency edge (only read-effect nodes in the same bind layer may run in parallel). For example, `branch_create` followed by `file_create` in one program is safe without an intermediate read-back.

Implementation: [`plasm_monad/`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/plasm_monad/), compile via [`compile_plasm_program`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/plasm_dag.rs).

### Program return semantics

Multi-line programs lower through a fixed staging pipeline:

`source` → `collect_program_statement_lines` → `expand_flattened_program_statements` → `validate_program_statement_order` → `compile_plasm_dag_to_plan_inner` → `PlasmComp.return` → live materialization.

| Symbol | Meaning |
|--------|---------|
| `BINDING_LINE` | `label = rhs` — introduces a named step in the comp graph |
| `ROOTS_LINE` | `root` or `r1, r2, …` — names the value(s) of the whole program |
| `coerced_default_return` | optional metadata when staging synthesized a `ROOTS_LINE` |
| `comp.return` | `{ "kind": "step", "step": "…" }` or parallel product — canonical return witness on the wire. **`kind: "parallel"`** means multiple return roots (applicative product), not concurrent step execution. |

**Three disjoint staging tiers** (see also [Program default-return coercion](#program-default-return-coercion-two-sugars-only) below):

| Tier | Precondition | Semantic law |
|------|--------------|--------------|
| **1 — Strict ML `let` block** | At least one non-binding statement | **R1:** the **last** non-`BINDING_LINE` is the program value (`let b₁ = e₁ in … in RETURN`). Staging must not rewrite it. |
| **2 — Flat single-line sugar** | One logical line, `line_has_flattened_program_shape`, no heredoc | Coercion applies **only** within `split_flattened_program_line` parts (first-binding append / bare-label replace). Never at program scope when a `ROOTS_LINE` already exists. |
| **3 — Binding-only omission** | Every statement is `BINDING_LINE`; no `ROOTS_LINE` | **R3:** append the **last** binding label as `ROOTS_LINE` — ML reading `let b₁ = e₁ in let b₂ = e₂ in … in bₙ`. |

**Execution witness (R4):** dry-run `comp.return.step` (or parallel `steps`) must equal the staged `ROOTS_LINE` label(s). Live execute materializes rows from that root only.

| Program | Tier | `comp.return` |
|---------|------|---------------|
| `hits = e4(…)` | 3 | `hits` |
| `hits = e4(…)\nlabels = hits.labels` | 3 | **`labels`** |
| `filtered = …\nsorted = filtered.sort(…)[id,title]` | 3 | **`sorted`** |
| `…\nlimited[id,body]` after bindings | 1 | `limited` projection |
| `repo = e1\nlabels = e2\nlabels` | 1 | `labels` (explicit bare root) |
| `issues = e1{…} labels = issues.r# labels` (one line) | 2 | `issues` |

Conformance: `cargo test -p plasm-e2e --test plasm_language_matrix program_return`.

---

## Programs and typed holes (`PlasmInputRef`)

Inside **multi-line Plasm programs** (bindings + roots), method and predicate RHS positions accept **typed references** to prior bindings, not only concrete literals. The compiler represents these as `plasm_core::value::PlasmInputRef` inside the in-memory `Value` tree (serializes to plan `__plasm_hole` objects). **HTTP one-line execute** keeps concrete-only parsing unless the host opts into the same program context.

**Holes are deferral, not proof:** `PlasmInputRef` and plan `__plasm_hole__` validation placeholders tell the type checker to wait until row materialization. They do **not** certify that a parent field’s wire type matches a scoped-query capability parameter. After holes are filled, the **instantiated** expression must type-check like a concrete surface line (see **Relation binding proofs** and invariant 7b below).

- **Whole binding / node output:** `body=report` means “argument `body` receives the value produced by the program node bound to `report`” — it is **not** reparsed as a string literal after macro-style substitution.
- **Field paths:** `body=report.content` means “`content` on the materialized output of `report`” when `report` names an in-scope program node. Path segments use **wire field names** (`issue.number`); same RT rules as compound ctor keys.
- **`for_each` row:** inside `source => Effect(…)`, the row cursor is `_`; use `_.id`, `_.field`, etc. for per-row holes (same hole kind as the plan template contract).

**EntityRef slots and identity in brace predicates:** A field typed as **`entity_ref`** (EntityRef) toward another entity expects **identity** syntax for that target — `e#(id)` in parens, not a bare scalar. **Referential transparency:** any spelling that denotes that identity is valid in **every value position** (top-level get, brace predicate, method arg, nested compound slot) — session symbolic compound `e3(owner=…, repo=…)`, wire compound `Repository(owner=…, repo=…)`, **binding** from a prior assignment (`repo` after `repo=e3(…)`), scalar key fields, or `anchor.<relation>`. Inline and decomposed forms must parse to the same IR when equivalent.

**Teaching table fill-ins (not runtime values):** `$` is a **teaching placeholder** only. Rare **teaching exemplar literals** in the TSV (e.g. `pikachu` for Pokemon, `example-name` when `id_field` is `name`, `user@example.com` for email ids) parse as strings but are **samples** — substitute the real id from context before execute; do not copy them as wire constants except when reproducing the taught shape. **Scalar predicate values** in programs must be **quoted strings** (`state = "EVA"`, not `state = EVA`) — quoting is required only inside `{…}` predicates. A keyed **identity GET** on the entity's identity field (`e#(id=<id>)`) is a get-by-id, not a filter: the value is an id and an unquoted bare word (`e_mon(id=eevee)`) is accepted (a bad id surfaces as a live 404, not a compile error). When a binding yields a **typed row** for that same target entity, the type checker may **narrow** to identity using the catalog’s key fields (`key_vars` / id) when those scalars appear at the **top level** of the row value.

**MCP handles (not program syntax):** `logical_session_ref` and `run_ref` are **session/run tokens** for tool continuity — copy verbatim on MCP calls; they are not Plasm expressions. HTTP execute uses the query param `plan_commit_ref=pcN` for the same commit token.

**Display ≠ parse:** plan dry-run and teaching wire glosses may show catalog wire names (`render_expr_surface` / `wire_surface_for_teaching_session` — parse wire-first, render from typed IR). MCP/HTTP **programs** use opaque `e#` / `m#` / `r#` plus **catalog wire names** for fields, params, filters, and projections on ingress; [`parse_plasm_surface_line_program`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/plasm_plan_run/parse.rs) feeds the author's surface directly to the parser with in-grammar [`SymbolMap`] resolution — no textual pre-expansion (see [Session symbols are lexical tokens, not a string pre-expansion](#session-symbols-are-lexical-tokens-not-a-string-pre-expansion)). Legacy `p#` tokens are **rejected** at parse.

---

## Referential transparency

An expression that denotes a value denotes the **same value** wherever that type is expected:

| Surface | Example | Role |
|---------|---------|------|
| Session symbols | `e3`, `m14`, `r2` | teaching table + program tokens |
| Wire names | `Repository`, `owner`, `title` | Catalog truth; fields, params, filters, projections |
| Compound ctor | `e3(owner=o, repo=n)` or `Repository(owner=o, repo=n)` | Multi-key entity identity |
| Binding (label ref) | `repo`, `issue.number`, `body.content` (program context) | Prior binding / field path |

**Substitution laws (testable):**

1. **Inline ≡ decomposed:** `e1{repo=e3(…), …}` ≡ `repo=e3(…); e1{repo=repo, …}` when `repo` is typed as that entity.
2. **Symbolic ≡ wire:** after in-grammar `e#`/`m#`/`r#` resolution and wire-name field/param keys, wire render of the parsed IR and direct wire authoring normalize to the same `Value::Object` at compound ctor positions.
3. **Position independence:** if `e3(owner=x, repo=y)` parses as a get binding, the same surface must parse in brace predicates and method args.

Implementation: unified entity constructor head resolution in [`entity_ref_parse.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/entity_ref_parse.rs).

---

## Session symbols are lexical tokens, not a string pre-expansion

**Principle.** The opaque session symbols `e#` (entity), `m#` (method), `r#` (relation hop), and `v#` (value-domain metadata, never in code) are **lexical tokens** of the Plasm surface for that session. **Catalog wire names** are the canonical tokens for fields, capability params, query/search filters, and projection/postfix keys — resolved at the exact grammar position where they occur, scoped under the receiver `e#` / invoke `m#`. Symbol resolution is a property of the **parse over the source the agent wrote**, not a textual rewrite that happens before the parser runs. Legacy `p#` tokens are **rejected** at parse.

**Ingress (implemented).** [`parse_plasm_surface_line_program`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/plasm_plan_run/parse.rs) trims the author's line and calls [`parse_with_cgs_layers_program`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/mod.rs) with the session [`SymbolMap`] — **no** `expand_path_symbols` / `expand_expr_for_teaching_session` string pre-pass. The path parser ([`expr_parser/mod.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/mod.rs), [`value.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/value.rs), [`entity_ref_parse.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/entity_ref_parse.rs)) resolves opaque tokens in-grammar via `resolve_ident` / `normalize_method_symbol_label`. DAG postfix field lists resolve per token via [`resolve_wire_field_token`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/plasm_plan_run/parse.rs) at lower time.

**Display-only (never parse ingress):**

| Mechanism | Role |
|-----------|------|
| `wire_surface_for_parse` / `wire_surface_from_teaching_session_line` | Parse session surface → `render_expr_wire` for REPL / dry-run wire summaries |
| `collapse_tokens_for_feedback` | Wire → opaque for symbolic-mode parse-error hints |

**Diagnostic rule.** Parse/type errors carry offsets into **the source the agent wrote** (symbolic surface), never into a rewritten intermediate string.

**Positions the in-grammar resolver honors uniformly** (same spirit as [Referential transparency](#referential-transparency)):

| Position | Surface | Symbol role |
|----------|---------|-------------|
| Get head | `e#(id)` / `e#(owner=value)` | entity; positional and id-field shadow sugar are equivalent — `e#` resolves identically |
| Dotted call | `e#(…).m#(param=value, …, ..)` | wire keys are **capability-scoped** params of the resolved `(receiver entity, method)` capability |

**Same-entity scope EntityRef inference (type-level):** on dotted invoke/create, when a **scope** parameter `P` is typed `EntityRef(T)` and the receiver is already entity `T`, `P` may be **omitted** from authored `(…)` if the receiver identity normalizes to a legal `EntityRef(T)` payload (compound keys from `e#(owner=…, repo=…)`, etc.). Explicit `P=…` in input always wins. Path-template scope keys already injected from the receiver stay omitted as before. Cross-entity scope (e.g. `Issue` receiver + `repository: Repository`) remains required.
| Union ctor | `v#(field=$, …)` | wire keys resolve to union field wires in brace maps |
| Fetch filter | `e#{field=…}` | wire is the query param/filter for that entity+capability |
| Search filter | `e#~"…"{field=…}` | wire is the **Search**-capability param (homograph-safe vs Create/Update params) |
| Relation hop | `receiver.r#` (or wire) | `r#` resolves to a declared relation wire; a filter wire after `.` yields `RelationSegmentWrongRole` except LHS-binding coercion (see [Binding RHS shapes](#binding-rhs-shapes-label--)) |
| Projection / postfix | `[field,…]`, `.sort(field)`, `.group_by(field)`, `.with{col: expr}`, `.dedupe(…)`, `.distinct(…)`, … | wire names resolve to `rows:` field symbols under the row entity |

---

Examples (program shape):

```text
report = commits[sha,message] <<RPT
…
RPT
sent = e3.m19(body=report.content)
```

The parser records `body` as a **typed input ref** to materialized output once `report` is bound. A row-to-text template (`… <<TAG … TAG`) produces a row whose generated text lives under **`content`**; string parameters must use **`report.content`**, not **`report`**, or type-checking sees value type **object** vs **String**.

For nodes whose materialized value is already a scalar row (surface query/get), **`body=report`** may still be correct—match the schema’s expected wire shape.

### `${binding.path}` in string literals and heredocs

Program string literals and tagged heredocs may also use **`${report.content}`**-style interpolation. At plan instantiation, the host resolves these against in-scope binding rows (same roots as `PlasmInputRef`). Prefer **`report.content`** when not embedding in a larger string; both forms are supported. Escape a literal dollar as **`$$`**. Plan-layer templates (`template\`…\``) and row templates (`{{ }}`) use their own substitution rules.

**UTF-8 text invariant:** program source is UTF-8; `${…}` interpolation copies literal spans as UTF-8 substrings (scalar Unicode preserved end-to-end). Wire JSON string params are UTF-8; hosts must not reinterpret bytes as Latin-1 when stitching templates.

Inside **`source => Effect(…)`** (`for_each`), the row cursor works in both expression slots and string/heredoc slots:

- Expression: `title=_.title` (typed `PlasmInputRef::RowBinding` hole)
- Heredoc / quoted string: `body=<<BODY\n${_.title}\nBODY` (resolved at template instantiation against the current source row)

Cross-binding references (`${stats.content}`, `body=report.content`) are also supported in `for_each` effect templates: upstream singleton bindings are wired into the template scope alongside the per-row cursor. Undeclared `${…}` roots fail plan validation (same rules as derive `=>` templates).

---

## Invariants

1. **Transforms are core postfix syntax** — `.limit(n)`, `.sort(field, desc)` / `.sort(field,dir)` (whitespace direction sugar accepted), `.filter{…}` / `.filter(…)`, `.aggregate(…)`, `.group_by(field).aggregate(specs)` (primary), `.group_by(field, …)` (comma sugar), `.with{…}` / `.with(…)` (derived columns), `.dedupe(…)` / `.distinct(…)`, `.singleton()`, `.page_size(n)`, bracket projections `[field,…]`, and row-to-text template blocks (`<<TAG … TAG`) are part of the same language as `e1{…}` / `e2(…)`.
2. **Wire field names are canonical** — in MCP/symbolic sessions, postfix field tokens (`.sort`, `.filter`, `.group_by`, `.dedupe`, `.distinct`, `[field,…]`) use **catalog wire names** copied from the teaching TSV left column under the row entity. Diagnostics must never imply opaque `p#` tokens are accepted (legacy `p#` is rejected at parse).
3. **Binding is optional** — `expr.limit(20)` is valid without a prior `commits = expr` line when `expr` is a complete surface expression or an in-scope label.
4. **Artifact-level semantics today** — transforms are applied to materialized row JSON in the plan executor unless an optimizer later pushes work to HTTP (the optimizer must never change what the surface language means).
5. **No second “DAG language” for users** — diagnostics, MCP copy, and teaching gloss refer to **Plasm programs** or **Plasm expressions**, not “Plasm-DAG” as a distinct syntax tier.
6. **Equivalence** — for any expression `E`, the program `x = E\nx.op(…)` and the single line `E.op(…)` (with the same postfix chain) must lower to the same executable plan shape (modulo synthetic node ids and display strings).

### Common agent pitfalls (not parser bugs)

- **Two filter planes:** `e1{field=value}` filters at **fetch** (HTTP/CML); `binding.filter{field=value}` filters **materialized rows**. See [plasm-row-compute.md](plasm-row-compute.md).
- **No `rows{…}` on bindings:** use `label.filter{…}` or `label.filter(…)` — not `rows{pred}`.
- **Bare `.filter` in paths:** without `{` or `(` after `.filter`, the segment is a **relation** name, not row compute.
- **`group_by`:** primary `group_by(p_key).aggregate(n=count)` (keys-only `.group_by` then `.aggregate`); bare `group_by(p_key)` is sugar for `count=count`; comma form `group_by(k1, k2, n=count)` remains sugar for fused keys+specs.
- **`.with`:** `.with{col: expr}` adds derived columns per row; expression language is documented in [plasm-row-compute.md](plasm-row-compute.md#derived-columns-with). `.with{` / `.with(` is row compute — not a relation hop. Path segments like `.join(…)` without `{`/`(` after a known postfix verb are not row compute.
- **`=>` on bindings (two uses only):** `source => { k: _.field }` (derive map) or `source => e1(…).update(…)` (for_each). There is no `.derive(…)` surface. Row-to-text uses postfix `rows <<TAG`, not `=>`.
- **Relation fanout:** `labels = issues.labels` **or** `labels = issues.r#` (opaque relation symbol from teaching TSV) — never `issues => e2.r#` or `source => binding.r#` (compile rejects relation hops on `=>`). A **filter wire after `.`** on a receiver is not a relation hop (use `.r#` or the relation wire). The RHS of `=>` is not `plasm_expr`; entity calls there stringify or fail compile.
- **Homograph wires:** query filters and relation hops may share a wire name (e.g. `labels`). In-grammar resolution at the nav position disambiguates: `receiver.r#` / `receiver.labels` is a relation hop; the same wire in `{…}` is a filter/param. Teaching exemplars prefer `.r#` or wire names in relation position.
- **teaching table Meaning column:** `relation e3 → e2` (or legacy `=>` in older TSV) is teaching gloss only — executable relation hops are `binding.r#` **or** the catalog wire name (e.g. `binding.labels`).
- **Federated sessions:** duplicate wire entity/method/relation/field names across catalogs are disambiguated by session **`e#` / `m#` / `r#`** (stamped with `catalog_entry_id` in IR), not bare wire tokens alone. `entry_id:Entity` appears in MCP seeds and reuse maps only — never in program surface.

---

## Binding RHS shapes (`label = …`)

### Referential transparency of bindings

A binding `label = E` names the plan node produced by `E`. **`label` and `E` are interchangeable** in continuation positions:

- `label.r#` ≡ `E.r#` (relation hop)
- **`label.m#(…)` / `label.<method>(…)` ≡ `E.m#(…)`** (method invoke on the bound row)
- Postfix on row lists (`label.filter{…}`, `label.with{…}`, `[field,…]`) when row-preserving

Side-effect invokes on **plural** bindings are rejected — use `rows => e#.m#(param=_.…)` or `.limit(1)` / `.singleton()` first. **`.content`** applies only to row-to-text **Render** bindings, not plain string/data bindings.

Binding forms:

| Form | Example | Lowers to |
|------|---------|-----------|
| Surface + postfix | `issues = e1{…}.page_size(100)` or `stale = issues.with{age_days: (now - updated_at)}` | Query / get + compute |
| Relation hop | `labels = issues.labels` or `labels = issues.r#` | `RelationTraversal` (per-row fanout when parent is plural) |
| Method invoke | `out = repo.m#(…)` when `repo = e#(…)` | Same invoke IR as `e#(…).m#(…)` |
| Derive map | `cards = rows => { t: _.title }` | `Derive` (`value_or_template` only) |
| for_each effect | `sync = rows => e1(…).update(…)` | `ForEach` (writes; `_` row cursor) |

**Plural parent → many-relation:** when `issues` is a list binding, `labels = issues.r2` (or `issues.labels`) executes the child relation **once per parent row** (`source_cardinality: many` in the plan). Opaque `r#` segments resolve through the session symbol map to the catalog relation wire before lowering. Pagination applies to the **parent** query (MCP: pass the page handle as **`run_ref`** on `plasm_run`; HTTP-only execute: `page(pgM)` in the POST body), then relation fanout runs on that page’s rows.

**Type-check admissibility (many-relation `.r#`):** chain `AutoGet` on a declared relation is valid when the target entity has **Get**, or when catalog `materialize` is **`from_parent_get`**, **`prefer_from_parent_get`**, **`query_scoped`**, or **`query_scoped_bindings`**. Embed-driven hops (e.g. Linear `Issue.labels` from `issue_get`) do **not** require a target Get capability. Teaching rows and extend-wave relation deltas are emitted only when a candidate exemplar passes parse + type-check.

**teaching vs grammar:** Meaning-column text such as `relation e3 → e2` in the teaching TSV is **pedagogy only** (relation target gloss, not program syntax). Executable navigation is `receiver.r#`, `binding.r#`, or the declared wire name (`binding.labels`), never `binding => Entity(…)`.

**Row-hole relation continuation:** when a plural binding (`issues = e1{…}`) continues with `issues.labels` / `issues.r#`, the compiler often cannot re-parse the anchor surface expression (federated catalogs, relation-sourced parents). It lowers **row-hole IR** instead: per-row `NodeInput` holes filled from the upstream binding at plan/runtime. Symbol resolution for relation segments (`r#` → wire) applies at the same DAG boundary as field projection (`binding[field]`). Anchor re-parse is used when the continuation anchor still allows text parse (e.g. singleton `issue = e1(…); issue.r#`).

**Federated catalog ownership (compile / type-check / plan):** schema lookup is always keyed by **catalog ownership** `(entry_id, entity)` — not wire name or opaque symbol alone. Teaching symbols (`e#`, `m#`, `r#`) are indices into the session TSV; executable IR carries `catalog_entry_id` on `Query` / `Get` / relation chain sources. Relation hops inherit the **source row’s** catalog for relation schema lookup (`children` on linear `Issue`, not github `Issue.sub_issues`). When the same wire entity exists in multiple loaded catalogs, bare wire heads (`Issue`, `Issue.create`, `parent.r#` without a stamped source) are rejected at parse or type-check unless disambiguated by session `e#`, binding continuation, or an explicit `catalog_entry_id` stamp on the IR node.

**Return position (ML `let` block):** a multi-line program is `{ BINDING_LINE }*` followed by a final **return expression** — syntactic analog of `let b1 = e1 in let b2 = e2 in … in RETURN`. The **last non-binding line** is authoritative: `limited[title,body]` after `issue = …` / `comments = …` / `limited = …` returns the projected `limited` node, not the first binding. Pre-compile staging must never rewrite an explicit trailing roots line. Multiple return roots belong on **that one line**, comma-separated (`a, b, c`) — not as separate bare-label lines (each extra line is a second return and fails validation).

**Agent diagnostics:** intermediate compute steps must use bindings (`filtered = comments.filter{…}`), not extra roots-only lines. Only the **last line** is the return. Stacking bound labels on separate lines (`a` then `b`) is rejected with comma-separation guidance. Compiler errors for MCP/HTTP agents are single imperative lines (optional `help:` rewrite), without internal node ids (`return_1`) or duplicate parser offset dumps.

**Program default-return coercion (two sugars only):**

1. **Flat single-line (space inference):** on one physical/logical line only, when `line_has_flattened_program_shape` detects space-separated bindings, split into statements. Coercion to the **first binding** applies only when (a) the line ends on a binding with no return yet (append first label), (b) the trailing token is a **bare label** echoing a side binding (e.g. `issues = e1{…} labels = issues.r# labels` → return `issues`), or (c) the trailing token is a **fresh-entity** expression whose head is not an in-scope binding (e.g. `item = LangItem(…) LangItem.sort(…).limit(2)` → return `item`). A trailing **projection or postfix on an in-scope binding** (`comments[title,body]`, `comments.limit(5)[title,body]`, `issue[id,title]`) is the explicit return and is never rewritten. Rule applies **within that line's split parts only** — never across newlines.
2. **Binding-only omission:** when **every** logical statement is a `BINDING_LINE` and no return line exists, append the **last** binding label as roots (e.g. `hits = e4(…)` alone, or `hits = …` + `labels = hits.labels` with no final line → return **`labels`**).

First-binding **replacement** of an existing roots expression never applies at program scope. Plan metadata records `coerced_default_return` when either sugar applies.

**teaching table `r#` vs wire fields:** declared relations allocate **`r#`** symbols; fields, capability params, and query filters use **catalog wire names** in the teaching TSV left column. Relation-nav exemplars use `.r#` (or wire); they do not emit a second standalone gloss row per relation.

**`=>` is not a row-map or relation operator:** it appears only in the binding form `label = source => rhs`. Do not use it for read fanout, relation hops, or Minijinja templates.

---

## Host continuations (`page`, `wait`, `cancel`)

These are **host-only** surface expressions — not CGS entity operations. The parser accepts them as top-level program bodies; the **agent host** (MCP / HTTP execute) dispatches them before plan compile. The runtime rejects direct execution with a host-delegation error.

| Expression | Handle shape | Purpose |
|------------|--------------|---------|
| `page(pgN)` | `pgM` (HTTP-only paging) | Resume paginated query batch in POST body |
| (MCP paging) | `l_<token>_pgM` passed as **`run_ref`** on `plasm_run` | Resume paginated query batch without a second `plasm` call |
| `wait(oN)` | `oM` (HTTP) | Poll in-flight async plan run |
| `cancel(oN)` | `oM` (HTTP) | Cooperative cancel of that operation |

**MCP:** `plasm_run` awaits server-side and does not accept operation continuations. Agents author programs only through `plasm`; `plasm_run` executes via reviewed **`run_ref`** (`pcN` or page handle). Legacy transport slots (`s0`, …) are rejected.

**HTTP execute:** long-op and paging handles are plain **`oN`** / **`pgN`** on the same execute session row.

**Async plan runs:** HTTP execute may start live work with `?wait=false`; the accept response includes `wait(…)` in Markdown and `_meta.plasm.operation`. Poll with `wait(…)`; cancel with `cancel(…)`. MCP `plasm_run` awaits internally and returns a terminal response.

**Review gate:** MCP live execute requires **`run_ref`** (`pcN`) from `plasm`. HTTP live execute may use query `plan_commit_ref=pcN` or `force=true`. Commit ids hash the **semantic plan DAG** (`version`, `steps`, `bind`, `return`) — not session-local plan names or dry-run summary metadata. See [plasm-long-operations.md](plasm-long-operations.md).

IR types: [`PageExpr`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr.rs), [`WaitExpr`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr.rs), [`CancelExpr`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr.rs).

---

## Parser modules (reference implementation)

Surface scanning lives in **`plasm-oss/crates/plasm-core/src/expr_parser/`**:

| Module | Responsibility |
|--------|----------------|
| [`heredoc_surface.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/heredoc_surface.rs) | Tagged `<<TAG …` open/close detection shared by values, postfix render tails, and multi-line program staging. |
| [`program_surface.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/program_surface.rs) | Physical-line merging across heredocs (`collect_program_statement_lines`), `;;` stripping, top-level comma/`=>` splitting (`split_top_level`, `split_token_top_level`), binding `=` splitting (`split_assignment_at_top_level` / `split_assignment_for_binding`), program label validation. |
| [`predicate_surface.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/predicate_surface.rs) | Query `{…}` predicate list: same comma splitting as `split_top_level`, plus quote/heredoc-aware comparison-operator scan for [`expr_correction`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_correction/mod.rs) (no duplicate lexer). |
| [`program.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/program.rs) | Optional shape AST: bindings + postfix-peeled primaries (`parse_program_shape`). Does not attach CGS typing. |
| [`postfix.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/postfix.rs) | Postfix peel (`.limit`, `.sort`, `[projection]`, row-to-text `<<TAG`). |
| [`mod.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/mod.rs) (path parser) | CGS-aware **path expression** → [`Expr`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr.rs) + optional trailing `[projection]`. |
| [`entity_ref_parse.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/entity_ref_parse.rs) | Session `e#` + wire entity constructor head resolution (referential transparency across value positions). |
| [`value.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/value.rs) | Scalar/collection literals, strict vs lenient RHS, structured heredocs, `PlasmInputRef` holes when program context is enabled. |

Multi-line **program → Plan/DAG** lowering remains in [**`plasm_dag.rs`**](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/plasm_dag.rs), which calls **`program_surface`** and **`postfix`**, then [`parse_with_cgs_layers_program`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/mod.rs) on each author's surface primary.

**Lenient single-expression parse:** `expr_parser::parse` reads **one** path expression from the start of a string and **ignores trailing non-whitespace** (noisy LLM paste tolerance). Whole-program compilation uses **statement-collected lines** and does not apply that tail-ignore rule to binding/root lines.

**Plan IR:** the program **Plan** serializes losslessly; archived traces use that shape for provenance. Fields such as `metadata.language` are **IR metadata**, not a separate user-facing language name.

---

## Grammar (EBNF)

> **Teaching prompts:** MCP/HTTP execute sessions embed a **Core surface** contract (program shape, postfix ops, continuations, worked examples, pitfalls)—not this full EBNF block. Canonical formal grammar for tooling and language conformance remains here and in [`plasm_language_matrix`](https://github.com/PlasmTools/plasm-core/tree/main/fixtures/schemas/plasm_language_matrix) / `cargo test -p plasm-e2e --test plasm_language_matrix`.

Notation: `…` repetition, `[ … ]` optional, `{ … }` grouping. Productions are **layered**. Several nonterminals are **catalog-parameterised** (valid `Entity`, `field`, `method` names come from loaded CGS + session symbol map).

### Lexical helpers

```ebnf
WS_CHAR       = ? ASCII space or tab ? ;
NEWLINE       = ? U+000A ? ;
LINE_COMMENT  = ";;" , { ? any codepoint except NEWLINE ? } ;
IDENT_START   = ? ASCII letter ? | "_" ;
IDENT_CONT    = IDENT_START | ? ASCII digit ? ;
IDENT         = IDENT_START , { IDENT_CONT } ;
DOMAIN_SYM    = ( "e" | "p" | "m" ) , { ? ASCII digit ? } ;
PROGRAM_LABEL = IDENT | (* must NOT match DOMAIN_SYM *) ;
TAG           = IDENT_START , { IDENT_CONT } ;
```

### Tagged structured heredoc (formal shell)

Opener/close rules are implemented in [`heredoc_surface.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/heredoc_surface.rs). **Operational discipline** for choosing `TAG` (collision-safe payloads) is under [Tagged heredocs and tag collision](#tagged-heredocs-and-tag-collision) below.

```ebnf
HEREDOC_OPEN_LINE  = "<<" , TAG , { WS_CHAR } , NEWLINE ;
HEREDOC_CLOSE_TAIL = { WS_CHAR | ")" | "]" | "}" | "," } ;
HEREDOC_CLOSE_LINE = TAG , HEREDOC_CLOSE_TAIL ;
STRUCTURED_HEREDOC = HEREDOC_OPEN_LINE , HEREDOC_BODY , HEREDOC_CLOSE_LINE ;
(* HEREDOC_BODY / HEREDOC_CLOSE_LINE: first trimmed matching close line wins; delimiter tail is parser-owned. *)
```

**Three heredoc uses (disjoint):**

| Use | Surface | Pass to string params |
|-----|---------|------------------------|
| Inline value | `param=<<TAG` … `TAG` inside `{…}` or method args | N/A (body is the param value) |
| String binding | `label = <<TAG` … `TAG` | **`param=label`** — not `label.content` |
| Row-to-text template | `label = source[field,…] <<TAG` … `TAG` (Minijinja) | **`label.content`** |

### Program shape (multi-line)

Logical statements come from [`collect_program_statement_lines`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/program_surface.rs) (heredocs may span physical lines).

```ebnf
PROGRAM       = { BINDING_LINE } , ROOTS_LINE ;
ROOTS_LINE    = LINE_COMMENT? , ROOT , { "," , ROOT } ;
ROOT          = RHS ;
(* Semantic: ML-style `let` block — last non-binding line is the program value. *)
BINDING_LINE  = LINE_COMMENT? , PROGRAM_LABEL , WS_CHAR? , "=" , WS_CHAR? , RHS ;
PHYSICAL_LINE = { ? any codepoint except NEWLINE ? } , NEWLINE? ;
```

Binding lines use `split_assignment_at_top_level` then **`validate_program_label`** — **`e1` / `m3`-style teaching symbols are rejected** as binding names.

### Postfix chain (per `RHS` fragment)

After [`peel_postfix_suffixes`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/postfix.rs), surface postfix applies **inner-to-outer** per the [chaining order](#chaining-order) invariant.

```ebnf
POSTFIX_OP    = "singleton"
              | "limit" , "(" , INTEGER , ")"
              | "page_size" , "(" , INTEGER , ")"
              | "sort" , "(" , SORT_ARGS , ")"
              | "filter" , ( "{" , PRED_LIST , "}" | "(" , PRED_LIST , ")" )
              | "aggregate" , "(" , AGG_ARGS , ")"
              | "group_by" , "(" , GROUP_ARGS , ")"
              | "with" , ( "{" , WITH_BODY , "}" | "(" , WITH_BODY , ")" )
              | "dedupe" , [ "(" , FIELD_LIST , ")" ]
              | "distinct" , [ "(" , FIELD_LIST , ")" ]
              | "[" , FIELD_LIST , "]" ;
WITH_BODY     = WITH_COLUMN , { "," , WITH_COLUMN } ;
WITH_COLUMN   = IDENT , ":" , WITH_EXPR ;
WITH_EXPR     = (* v1: field paths, literals, `now`, `len(field)`, `when(cmp, then, else)`, `+ - * /` — see plasm-row-compute.md *)
              ;
FIELD_LIST    = IDENT , { "," , IDENT } ;
```

**Row-to-text:** optional render tail after the postfix head — `… [ fields ]? <<TAG …`; see [`try_parse_render_tail`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/postfix.rs) and [Row-to-Text Templates](#row-to-text-templates-content-and-minijinja).

### Path expression (CGS-aware)

Abbreviated from [`expr_parser/mod.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/mod.rs).

```ebnf
EXPR          = SOURCE , { PIPE_SEGMENT } , [ "[" , FIELD_LIST , "]" ] ;
SOURCE        = Entity , "(" , ARG_LIST , ")"
              | Entity , "{" , PRED_LIST , "}"
              | Entity , "~" , SEARCH_PHRASE , [ "{" , PRED_LIST , "}" ]
              | Entity
              | PAGE_CALL ;
PIPE_SEGMENT  = "." , FIELD_NAME
              | "." , METHOD , [ "(" , METHOD_ARGS , ")" ]
              | "." , METHOD , "()"
              | ".^" , Entity , [ "{" , PRED_LIST , "}" ] ;
METHOD_ARGS   = DOTTED_ARG_LIST | UNION_CTOR_PAYLOAD ;
DOTTED_ARG_LIST = (* empty *) | ".." | KEY , "=" , VALUE , { "," , KEY , "=" , VALUE } , [ "," , ".." ] ;
UNION_CTOR_PAYLOAD = "v" , DIGITS , "{" , ARG_MAP , "}" ;
(* ARG_MAP: same key/value surface as dotted-call args — see value.rs; no leading `v`+digits+`{` form inside the map. *)
PRED          = FIELD_NAME , COMP_OP , [ VALUE ]
              | ForeignEntity , "." , FIELD_NAME , COMP_OP , [ VALUE ] ;
COMP_OP       = "=" | "!=" | ">" | "<" | ">=" | "<=" | "~" ;
VALUE         = QUOTED_STRING | STRUCTURED_HEREDOC | UUID | NUMBER | BARE_WORD
              | "[" , { VALUE , "," } , VALUE , "]"
              | (* phrase / lenient regions — see value.rs *)
              ;
```

**Method arguments (`METHOD_ARGS`)** are **catalog-sensitive** after the left-hand receiver and method label resolve to a capability:

- If that capability’s merged `input_schema.input_type` is **`InputType::Object`**, only **`DOTTED_ARG_LIST`** is valid (`key=value`, optional `..` ellipsis).
- If it is **`InputType::Union`** (a *root* tagged union), the parentheses may contain **either** a full **`DOTTED_ARG_LIST`** (including a wire-style object with the variant discriminator field, when applicable) **or** exactly one **`UNION_CTOR_PAYLOAD`**: `v` + ASCII digits + `{` … `}` matching a variant’s `constructor_symbol` and body fields.
- **Mixed forms are rejected** (e.g. `method(v111{…}, title=$)`): `UNION_CTOR_PAYLOAD` must be the sole contents of `( … )` for that overload.

**Lowering:** a sole `UNION_CTOR_PAYLOAD` is stored on the invoke IR as raw [`Value::UnionCtor`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/value.rs) inside [`InvokeInputPayload`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/typed_invoke.rs) (deserialized as `InvokeInputPayload::Raw`). The runtime lifts it with the capability `input_schema` into wire JSON (discriminator merged per variant `wire`) before CML template evaluation — same path as nested union rows inside object bodies.

**Context sensitivity:** classification into field navigation vs invoke vs zero-arity depends on **`CGS`**. Federation uses [`parse_with_cgs_layers_program`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/mod.rs) with the session [`SymbolMap`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/symbol_tuning.rs).

**Predicate lists (`Entity{ … }`):** comma-separated clauses use [`split_top_level`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/program_surface.rs) (`()`, `[]`, `{}`, quotes, tagged heredocs). Within each clause, the first top-level comparison operator (`!=`, `>=`, `<=`, `=`, `~`, `>`, `<`) is located with the same nesting rules — see [`predicate_surface.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/predicate_surface.rs). [`try_auto_correct`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_correction/auto_correct.rs) delegates to that module so correction never runs a parallel comma/`=` scanner.

---

## Tagged heredocs and tag collision

Structured string values may use **tagged heredocs** (`<<TAG` … closing line `TAG` / `TAG)` / `TAG, arg=…` / `TAG})` / …), implemented in `plasm_core::expr_parser` ([`value.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/value.rs), shared close rules in [`heredoc_surface.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/heredoc_surface.rs)). The close delimiter is recognized on the **first** line (after the opener) whose **trimmed** content equals `TAG`, or `TAG` followed by optional ASCII space and either a comma (more call arguments on the same line) or a parser-owned delimiter tail containing only `)`, `]`, `}`, and/or `,` on the same line. The heredoc scanner closes the string at `TAG`; the enclosing parser then consumes and validates the suffix. There is no “last closing tag wins” scan.

**Unified object-expression rule:** heredocs are **value atoms**, not statement terminators. Program staging must keep accumulating physical lines until the heredoc is closed **and** the enclosing expression delimiters balance. This makes direct arguments and nested object/union payloads equivalent:

```text
Document(x).comment(text=<<T
hello
T)

Document(x).suggest(v111{content=<<T
hello
T})
```

**Implication:** pick a `TAG` that **cannot** appear as a trimmed line anywhere inside the intended payload. Short tags (`RFC`, `END`, `BODY`) are unsafe for arbitrary RFC822/MIME or markdown blobs because a real line may equal `TAG` and **truncate** the value early. Prefer high-entropy labels such as `PLASM_MAIL_9c2e` or `GMAIL_RAW_EOF`.

For multi-line `program` fields in JSON (HTTP execute, MCP `plasm` / `plasm_run`), the wire string must decode to **actual newline characters** between statements and heredoc lines—not only the two-character escape `\n` inside the JSON source without decoding.

---

## Row-to-Text Templates, `.content`, and Minijinja

**Surface:** `source[field,…] <<TAG` newline body newline closing `TAG`, or `source <<TAG` when columns can be inferred. Comma-separated **in-scope binding labels** before `<<TAG` (`label1,label2 <<TAG`) merge additional bindings into the Minijinja context (first label remains the primary `rows` source). The compiler projects each source row to the selected fields, then evaluates the template.

**Template engine:** bodies are **Minijinja** templates. The projected source rows are bound as **`rows`**: a JSON array of objects, one entry per source row, with keys taken from the projected **wire field names**. When the render source is a simple in-scope binding label, the **same array is also bound under that label** (e.g. `report = sorted <<TAG` → iterate `{% for r in sorted %}` or `{% for r in rows %}`). **Cross-binding** (`report = a,b <<TAG`): each listed label is also bound by name — singleton/get rows as a **row object** for `{{ a.field }}`; plural rows as an **array** for `{% for r in b %}`. The body is evaluated **once** over the whole list (not a per-row map). Typical patterns:

- `{{ rows | length }}`
- `{% for r in rows %}{{ r.sha }} — {{ r.message }}{% endfor %}`
- Per-field access matching your projection list; nullable fields: `{{ r.power or "—" }}` or `{{ r.power | default("—", true) }}`.

Free-form text **without** loops works only where the body does **not** accidentally contain Jinja fragments (`{{`, `{%`, `{#`). Use **`{% raw %}…{% endraw %}`** for passages that must contain those sequences literally. The output string may be **any** textual format—plain text, markdown, HTML fragments, CSV-like lines, JSON **text**, etc.—not markdown-specific.

**Program value shape:** the bound result is one row equivalent to `{"content": "<rendered string>"}`. When a later dotted-call parameter is typed as **String** (or similar scalar text), pass **`binding.content`**, not **`binding`**, so the type checker receives a string rather than an object.

---

## Chaining order

Postfix operators apply **left-to-right** on the primary: `a.limit(10).sort(x)` means *sort(limit(a))* — peel from the **right** when reconstructing the primary, then apply collected ops from inner to outer (limit then sort).

---

## Typed semantic core (Lean-oriented sketch)

Not a complete Lean formalisation; judgement forms intended to be mechanisable (e.g. Lean 4).

### Sorts and carriers

- **`Catalog`** — loaded CGS slice(s) + mappings metadata (entities, fields, capabilities, parameter slots).
- **`Γ`** — program environment: labels → node / value types.
- **`Value`** — literals + structured objects + **`Hole`** (`PlasmInputRef`).
- **`Expr`** — path IR ([`Expr`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr.rs)).
- **`Plan`** — lowered DAG (opaque; host-defined).

### Representative judgements

```text
⊢_cat Σ
Σ ; Γ ⊢ rhs : τ
Σ ; Γ ⊢ bind ℓ = rhs  ⇝  Γ, ℓ:τ
Σ ; Γ ⊢ program ok
⟦ e ⟧_Σ ↝ π
Σ ⊢ τ₁ ≤ τ₂   (* optional projection width *)
```

**Effects:** HTTP / live invokes as **`IO PlanValue`** (or abstract **`M`**). **Minijinja** as oracle **`render : Template → List Row → String`**.

### Binding environment `Γ` (continuation + cardinality)

Each program label `ℓ` bound by `ℓ = rhs` carries a compile-time **binding contract** (lowered in `plasm-agent-core`):

| Component | Meaning |
|-----------|---------|
| **row entity** | Catalog `QualifiedEntityKey` for the rows this label denotes (for projections and relation targets). |
| **row cardinality proof** | `static_singleton` (get / one-cardinality relation from singleton parent), `static_plural` (query / many-relation), `bounded_singleton` (`.limit(1)` / `.singleton()`), or `runtime_checked`. |
| **continuation mode** | Whether `ℓ.<relation>` is legal, postfix-only, `ℓ.content` scalar, or terminal (no `ℓ.` extension). |

**Continuation rules**

- `ℓ.<relation>` requires **relation-dot** continuation on `ℓ`. Surface **get/query** bindings expand the stored anchor Plasm (`Repository(…).<relation>`); **relation** bindings use typed single-segment lowering when the source is already a relation row.
- **Cardinality lattice (source × relation → result).** The traversal result is **singleton iff the source is singleton *and* the relation is one-cardinality**; otherwise it is **plural**. There are four cases:
  - **one relation, singleton source** (`source_cardinality: single`) → **singleton** target. A one-cardinality hop from a statically singleton parent yields another statically singleton row — enabling multi-hop chains such as `species = item.<one_rel>; next = species.<one_rel>`.
  - **one relation, plural source** (`source_cardinality: many`) → **flat-map (1:1)**: the runtime executes the relation IR **once per source row** and collects **one target per parent**, yielding a list aligned with the parents. This is an **inferable map** — it lowers to per-row fanout exactly like the many-relation case and **does not require `Plan.singleton(...)`**. Result cardinality is **plural**.
  - **many relation, plural source** → **fanout**: one scoped query per parent (`query_scoped` / `query_scoped_bindings`), target rows concatenated. Result plural.
  - **many relation, singleton source** → one scoped query, target rows as a list. Result plural.
- **`Plan.singleton(...)` / `.singleton()` is a narrowing assertion, not a traversal prerequisite.** It exists to **assert a plural binding is genuinely length-1** (runtime-checked, `source_cardinality: runtime_checked_singleton`) when you want a *singleton* result from a list. It is **never** required merely to traverse a one-cardinality relation across many parents — that is the inferable flat-map above. (Historically the planner rejected `one relation × many source` with “requires a singleton source; wrap with `Plan.singleton(...)`”; that gate was an over-restriction and is removed — see `validate_relation_traversal` in [`plasm_plan.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/plasm_plan.rs).)
- **Materialization is catalog-authoritative:** pure `query_scoped_bindings` always runs one scoped query per parent row (ignores decoded `relations` on the parent). `prefer_from_parent_get` (wire path + declared scoped fallback) is the executable form of “use embed when present, else scoped capability”; per-row choice uses `plasm_core::resolve_relation_row_resolution` in plan and runtime (no cache-shape heuristics).
- **Row-preserving postfix** — bracket projection `[…]`, `.limit(1)`, and `.singleton()` preserve **row entity** and adjust cardinality; further `.relation` hops use the same rules.
- **Project-then-relate** — field projection on a binding does **not** destroy relation receiver identity. Relation hops on a projected binding resolve the **graph parent** keyed by [`RowIdentity.reference`], not the projected field subset:

```plasm-matrix
e1{title="alpha"}[id,title]
```

`pika.tags` and `pika = Pokemon(name=pikachu); tags = pika.tags` must agree on executable semantics (same graph parent lookup). Concurrent execute invariants (CEP-*) live in the product monorepo when present; OSS conformance is the language matrix suite above.
- **Anti-pattern:** chained relation expansion on a get anchor (`Entity(id).rel1.rel2`) — bind intermediate rows (`a = Entity(id).rel1; b = a.rel2`) so cardinality proofs compose.

**Relation binding proofs (`query_scoped_bindings`)**

Catalog `materialize.bindings` maps each **capability parameter** (LHS key) to a **parent entity field** (RHS wire name), e.g. GitHub `issue_number: number` under `Issue.labels`. Three witnesses enforce assignability:

| Witness | When checked | What it proves |
|---------|--------------|----------------|
| **Catalog static** | `plasm-cgs validate` / CGS load | Parent field type is assignable to the param type (not merely “field exists”). |
| **Row** | `cached_entity_row_json`, `RowIdentity`, chain materialize | Parent slots are encoded with catalog wire types (`Integer` → numeric JSON / `Value::Integer`, not default stringification). |
| **Instantiated** | After `__plasm_hole` fill, before HTTP compile | Concrete relation IR satisfies capability `input_schema` (same judgement as one-line execute). |

Plan relation nodes may carry serialized `binding_proofs` (param ← parent field) for agents and dry review; the matrix fixture `lang_relation_integer_scoped_bindings` exercises integer scope params.

**Equivalence:** `E.<relation>` and `x = E; x.<relation>` must agree on executable semantics (plan node shapes may differ until a normalize pass).

**Row identity (`RowIdentity`)** — every row-producing plan node carries a canonical identity handle (qualified entity + [`Ref`] + ambient scope slots) in materialization, not only JSON payload. Projection and `.limit(1)` preserve identity when the suffix pipeline folds [`RowSuffix`] segments; [`PlasmInputRef::NodeInput`] holes resolve via identity, not stripped JSON paths.

**Suffix pipeline** — after the path head (Get/Query/label), dot/bracket segments classify as [`RowSuffix`] (relation, limit, project, sort, filter, `.with`, dedupe, …) and lower through one fold (`lower_suffix_stream`), including interleaved forms such as `repo.commits.limit(1).author`.

**Dry-live parity (invariants 6–10)**

6. **Dry-live parity** — plan-only ingress runs the same preflight chain as live (type-check, placeholder rejection, projection hydration dry-run, ForEach template TC); live I/O requires the same gates.
7. **Type-check layers**
   - **7a Surface preflight** — first ingress line / program surface: federated TC on parsed Plasm (`PlasmInputRef` allowed where specified).
   - **7b Instantiated preflight** — per-row relation fanout and any path that fills `__plasm_hole`: must type-check the **instantiated** `ParsedExpr` before compile; `PreflightToken::VERIFIED` applies only after this gate on plan-internal execute paths.
8. **One ingress shape** — identical surface text lowers to identical validated plan whether entered via HTTP, MCP `plasm`, or MCP `plasm_run`.
9. **Relation materialization parity** — dry-run review records relation nodes with `source_cardinality: many` (per-row fanout cost). Live execution must perform the same per-row fanout; a plan that only type-checks hole-filled relation IR without fanout semantics is invalid for plural sources.
10. **Relation binding assignability** — dry `plasm` approving hole-IR is not sufficient for live `plasm_run` on scoped bindings; instantiated witnesses and catalog `binding_proofs` are the approval bar for typed params (e.g. integer `issue_number`).

**Capability inputs** judgements over **`InputType`** in `Σ` — scoped relation hops lower to capability predicates; values must use **`Value` shapes compatible with parameter `FieldType`**, with coercion driven by catalog types in `plasm_core::wire_coercion` (not path-name heuristics). See below.

---

## Capability inputs (CGS load-time semantics)

### Registry vs structural fields

- **Entity fields** (`FieldSchema`) always use `value_ref` → a row in top-level `values:`.
- **Capability object parameters** (`parameters:` entries) use **exactly one** of:
  - `value_ref` → `values:` (registry), or
  - `input_type` → inline structural [`InputType`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs) (object / array / union / value / none).
- **`input_schema.input_type.fields`** use the same XOR: each field is either registry-backed (`value_ref`) or structural (`input_type`). When both `parameters` and `input_schema` are present, loader-merged object fields must not duplicate names.

Structural inline fields are **not** `values:` slots; registry-only consumers may skip them when a `NamedValueSchema` is required.

### Tagged unions (`InputType::Union`)

- Each variant has **`wire`** (`field` + `value`) — the **discriminator** merged into HTTP/CML JSON when lowering ([`TypedInvokeInput::Union`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/typed_invoke.rs)).
- **Surface typing** matches the variant **body** only (no discriminator in the Plasm value before lowering).
- **Lifting** tries each variant’s body shape in order until one matches.
- When the union is the **root** `input_schema.input_type` of an invoke/update/create dotted call, the surface may use a **`UNION_CTOR_PAYLOAD`** as the entire parenthesized argument list (see [`METHOD_ARGS` above](#path-expression-cgs-aware)); the parser records [`Value::UnionCtor`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/value.rs) with `constructor_symbol` matching the variant.

### Surface constructor literals (`v` + digits + `{…}`)

A token **`v`** plus ASCII digits plus a braced map parses as a **union constructor literal** [`Value::UnionCtor`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/value.rs) when it appears in value positions that accept constructors (including **`UNION_CTOR_PAYLOAD`** in method calls, and **standalone** teaching rows as [`Expr::TeachingValue`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr.rs)). Digits align with teaching table `constructor_symbol` mnemonics; the type checker ties them to `InputType::Union` variants in scope.

---

## Proof catalog

[`apis/proof/`](https://github.com/PlasmTools/plasm-core/tree/main/apis/proof) ships split **`domain.yaml`** + **`mappings.yaml`**. See [`apis/proof/README.md`](https://github.com/PlasmTools/plasm-core/blob/main/apis/proof/README.md) for regeneration and exploration.
