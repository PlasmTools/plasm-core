# Plasm language definition

This document is the **canonical specification** of the user-facing Plasm surface language: path expressions, multi-line programs (bindings, postfix transforms, roots), structured values and heredocs, row-to-text templates, and the **CGS load-time rules** for structured capability inputs. It aligns with the reference implementation in **`plasm_core::expr_parser`** and with program lowering in **`plasm-agent-core`** (Plan/DAG).

For API authoring (YAML catalogs, transport), use the OSS documentation **Connect an API → Reference**, or in the Plasm monorepo the skill file `plasm-oss/skills/plasm-authoring/reference.md` (repository root–relative).

---

## Programs and typed holes (`PlasmInputRef`)

Inside **multi-line Plasm programs** (bindings + roots), method and predicate RHS positions accept **typed references** to prior bindings, not only concrete literals. The compiler represents these as `plasm_core::value::PlasmInputRef` inside the in-memory `Value` tree (serializes to plan `__plasm_hole` objects). **HTTP one-line execute** keeps concrete-only parsing unless the host opts into the same program context.

**Holes are deferral, not proof:** `PlasmInputRef` and plan `__plasm_hole__` validation placeholders tell the type checker to wait until row materialization. They do **not** certify that a parent field’s wire type matches a scoped-query capability parameter. After holes are filled, the **instantiated** expression must type-check like a concrete surface line (see **Relation binding proofs** and invariant 7b below).

- **Whole binding / node output:** `p91=report` means “argument `p91` receives the value produced by the program node bound to `report`” — it is **not** reparsed as a string literal after macro-style substitution.
- **Field paths:** `p91=report.content` means “`content` on the materialized output of `report`” when `report` names an in-scope program node. Path segments may use opaque `p#` tokens (`issue.p27` → wire field `number` after session map resolution); same RT rules as compound ctor keys.
- **`for_each` row:** inside `source => Effect(…)`, the row cursor is `_`; use `_.id`, `_.field`, etc. for per-row holes (same hole kind as the plan template contract).

**Entity references in brace predicates:** A field typed as **entity reference** toward another entity expects **identity** for that target. **Referential transparency:** any spelling that denotes that identity is valid in **every value position** (top-level get, brace predicate, method arg, nested compound slot) — session symbolic compound `e3(p5=…, p13=…)`, wire compound `Repository(owner=…, repo=…)`, label ref from a prior binding (`repo` after `repo=e3(…)`), scalar key fields, or `anchor.<relation>`. Inline and decomposed forms must parse to the same IR when equivalent. teaching table `$` is a teaching placeholder only, not a runtime value. When a binding yields a **typed row** for that same target entity, the type checker may **narrow** to identity using the catalog’s key fields (`key_vars` / id) when those scalars appear at the **top level** of the row value.

**Display ≠ parse:** plan dry-run may show wire names after symbol expansion; MCP/HTTP programs keep opaque `e#` and must still parse on the session path (`expand_path_symbols_with_options(…, expand_entity_symbols: false)`).

---

## Referential transparency

An expression that denotes a value denotes the **same value** wherever that type is expected:

| Surface | Example | Role |
|---------|---------|------|
| Session symbols | `e3`, `p5`, `m14` | teaching table + program tokens |
| Wire names | `Repository`, `owner` | Catalog truth; plan display |
| Compound ctor | `e3(p5=o, p13=n)` or `Repository(owner=o, repo=n)` | Multi-key entity identity |
| Label ref | `repo`, `issue.p27`, `body.content` (program context) | Prior binding / field path — `p#` segments normalize to wire |

**Substitution laws (testable):**

1. **Inline ≡ decomposed:** `e1{p14=e3(…), …}` ≡ `repo=e3(…); e1{p14=repo, …}` when `repo` is typed as that entity.
2. **Symbolic ≡ wire:** after session `p#`/`m#` expansion (with `e#` opaque), compound keys normalize to the same `Value::Object`.
3. **Position independence:** if `e3(p5=x, p13=y)` parses as a get binding, the same surface must parse in brace predicates and method args.

Implementation: unified entity constructor head resolution in [`entity_ref_parse.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/entity_ref_parse.rs).

---

Examples (program shape):

```text
report = commits[p59,p43] <<RPT
…
RPT
sent = e3.m19(p91=report.content)
```

The parser records `p91` as a **typed input ref** to materialized output once `report` is bound. A row-to-text template (`… <<TAG … TAG`) produces a row whose generated text lives under **`content`**; string parameters must use **`report.content`**, not **`report`**, or type-checking sees value type **object** vs **String**.

For nodes whose materialized value is already a scalar row (surface query/get), **`p91=report`** may still be correct—match the schema’s expected wire shape.

### `${binding.path}` in string literals and heredocs

Program string literals and tagged heredocs may also use **`${report.content}`**-style interpolation. At plan instantiation, the host resolves these against in-scope binding rows (same roots as `PlasmInputRef`). Prefer **`report.content`** when not embedding in a larger string; both forms are supported. Escape a literal dollar as **`$$`**. Plan-layer templates (`template\`…\``) and row templates (`{{ }}`) use their own substitution rules.

Inside **`source => Effect(…)`** (`for_each`), the row cursor works in both expression slots and string/heredoc slots:

- Expression: `p24=_.title` (typed `PlasmInputRef::RowBinding` hole)
- Heredoc / quoted string: `p20=<<BODY\n${_.title}\nBODY` (resolved at template instantiation against the current source row)

Cross-binding references (`${stats.content}`, `p91=report.content`) are also supported in `for_each` effect templates: upstream singleton bindings are wired into the template scope alongside the per-row cursor. Undeclared `${…}` roots fail plan validation (same rules as derive `=>` templates).

---

## Invariants

1. **Transforms are core postfix syntax** — `.limit(n)`, `.sort(field[, desc])`, `.filter{…}` / `.filter(…)`, `.aggregate(…)`, `.group_by(field, …)`, `.singleton()`, `.page_size(n)`, bracket projections `[…]`, and row-to-text template blocks (`<<TAG … TAG`) are part of the same language as `e1{…}` / `e2(…)`.
2. **Binding is optional** — `expr.limit(20)` is valid without a prior `commits = expr` line when `expr` is a complete surface expression or an in-scope label.
3. **Artifact-level semantics today** — transforms are applied to materialized row JSON in the plan executor unless an optimizer later pushes work to HTTP (the optimizer must never change what the surface language means).
4. **No second “DAG language” for users** — diagnostics, MCP copy, and teaching gloss refer to **Plasm programs** or **Plasm expressions**, not “Plasm-DAG” as a distinct syntax tier.
5. **Equivalence** — for any expression `E`, the program `x = E\nx.op(…)` and the single line `E.op(…)` (with the same postfix chain) must lower to the same executable plan shape (modulo synthetic node ids and display strings).

### Common agent pitfalls (not parser bugs)

- **Two filter planes:** `e1{field=value}` filters at **fetch** (HTTP/CML); `binding.filter{field=value}` filters **materialized rows**. See [plasm-row-compute.md](plasm-row-compute.md).
- **No `rows{…}` on bindings:** use `label.filter{…}` or `label.filter(…)` — not `rows{pred}`.
- **Bare `.filter` in paths:** without `{` or `(` after `.filter`, the segment is a **relation** name, not row compute.
- **`group_by`:** canonical `group_by(key, count=count)`; bare `group_by(key)` is sugar for `count=count`.
- **`=>` on bindings (two uses only):** `source => { k: _.field }` (derive map) or `source => e1(…).update(…)` (for_each). There is no `.derive(…)` surface. Row-to-text uses postfix `rows <<TAG`, not `=>`.
- **Relation fanout:** `labels = issues.labels` **or** `labels = issues.r#` (opaque relation symbol from teaching TSV) — never `issues => e2(…)`. **`p#` is not a relation hop** after `.` on a receiver (use wire or `r#`); `labels = issues.p#` is coerced when the binding name matches the relation wire. The RHS of `=>` is not `plasm_expr`; entity calls there stringify or fail compile.
- **Homograph `p#` vs `r#`:** query filters and relation hops may share a wire name (e.g. `labels`). **teaching table expansion** rewrites opaque `p#` to wire before parse, so expanded surfaces use `.labels` or `.r#` in exemplars — not bare homograph `p#` in relation position. **Unexpanded** parse rejects `receiver.p#` when that token is a field/param (`RelationSegmentWrongRole`); program lowering may forgive via LHS binding coercion (`labels = issues.p99` when the binding name matches the relation wire).
- **teaching table Meaning column:** `relation e3 → e2` (or legacy `=>` in older TSV) is teaching gloss only — executable relation hops are `binding.r#` **or** the catalog wire name (e.g. `binding.labels`).
- **Federated sessions:** duplicate wire entity names across catalogs are disambiguated by session **`e#`** (stamped with `catalog_entry_id` in IR), not bare `Entity` alone.

---

## Binding RHS shapes (`label = …`)

Three **disjoint** binding forms:

| Form | Example | Lowers to |
|------|---------|-----------|
| Surface + postfix | `issues = e1{…}.page_size(100)` | Query / get + compute |
| Relation hop | `labels = issues.labels` or `labels = issues.r#` | `RelationTraversal` (per-row fanout when parent is plural) |
| Derive map | `cards = rows => { t: _.title }` | `Derive` (`value_or_template` only) |
| for_each effect | `sync = rows => e1(…).update(…)` | `ForEach` (writes; `_` row cursor) |

**Plural parent → many-relation:** when `issues` is a list binding, `labels = issues.r2` (or `issues.labels`) executes the child relation **once per parent row** (`source_cardinality: many` in the plan). Opaque `r#` segments resolve through the session symbol map to the catalog relation wire before lowering. Pagination applies to the **parent** query (`page(sN_pgM)`), then relation fanout runs on that page’s rows.

**teaching vs grammar:** Meaning-column text such as `relation e3 → e2` in the teaching TSV is **pedagogy only** (relation target gloss, not program syntax). Executable navigation is `receiver.r#`, `binding.r#`, or the declared wire name (`binding.labels`), never `binding => Entity(…)`.

**Row-hole relation continuation:** when a plural binding (`issues = e1{…}`) continues with `issues.labels` / `issues.r#`, the compiler often cannot re-parse the anchor surface expression (federated catalogs, relation-sourced parents). It lowers **row-hole IR** instead: per-row `NodeInput` holes filled from the upstream binding at plan/runtime. Symbol resolution for relation segments (`r#` → wire) applies at the same DAG boundary as field projection (`binding[p#]`). Anchor re-parse is used when the continuation anchor still allows text parse (e.g. singleton `issue = e1(…); issue.r#`).

**Flattened single-liner coercion:** space-separated bindings on one physical line are split into logical statements; when roots are omitted or a trailing root duplicates a side binding, the **first binding** is the default return (e.g. `issues = e1{…} labels = issues.r# labels` → return `issues`). Plan metadata records `coerced_default_return` when coercion applies.

**teaching table `r#` vs `p#` pools:** declared relations allocate **`r#`** symbols; fields, capability params, and query filters stay in **`p#`**. Teaching TSV relation-nav exemplars use `.r#` (or wire); they do not emit a second standalone gloss row per relation — net prompt size stays ~flat aside from `p#` renumbering when relations leave the `p#` sequence.

**`=>` is not a row-map or relation operator:** it appears only in the binding form `label = source => rhs`. Do not use it for read fanout, relation hops, or Minijinja templates.

---

## Host continuations (`page`, `wait`, `cancel`)

These are **host-only** surface expressions — not CGS entity operations. The parser accepts them as top-level program bodies; the **agent host** (MCP / HTTP execute) dispatches them before plan compile. The runtime rejects direct execution with a host-delegation error.

| Expression | Handle shape | Purpose |
|------------|--------------|---------|
| `page(s0_pgN)` | `sN_pgM` (MCP) or `pgM` (HTTP-only paging) | Resume paginated query batch |
| `wait(s0_oN)` | `sN_oM` | Poll in-flight async plan run |
| `cancel(s0_oN)` | `sN_oM` | Cooperative cancel of that operation |

**MCP:** handles are namespaced with the logical session ref from `plasm_context` (`s0_o1`, `s0_pg2`, …).

**HTTP execute:** when no MCP logical session exists, long-op handles use synthetic session slot **`s0`** (`s0_o1`, …) on the same execute session.

**Async plan runs:** start live execute with `wait: false` (MCP `plasm_run` arg or HTTP `?wait=false`). The accept response includes `wait(s0_oN)` in Markdown and `_meta.plasm.operation`. Poll with `wait(…)`; cancel with `cancel(…)`.

**Review gate:** plans with dry verdict **review** require **`plan_commit_ref`** (`pcN`) from a matching plan dry-run or **`force: true`** before live execute. Commit ids hash the **semantic plan DAG** (`version`, `nodes`, `edges`, `topological_order`, `returns`) — not session-local plan names or dry-run summary metadata. See [plasm-long-operations.md](plasm-long-operations.md).

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

Multi-line **program → Plan/DAG** lowering remains in [**`plasm_dag.rs`**](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/plasm_dag.rs), which calls **`program_surface`** and **`postfix`**, then [`parse_with_cgs_layers_program`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/mod.rs) on each session-expanded primary.

**Lenient single-expression parse:** `expr_parser::parse` reads **one** path expression from the start of a string and **ignores trailing non-whitespace** (noisy LLM paste tolerance). Whole-program compilation uses **statement-collected lines** and does not apply that tail-ignore rule to binding/root lines.

**Plan IR:** the program **Plan** serializes losslessly; archived traces use that shape for provenance. Fields such as `metadata.language` are **IR metadata**, not a separate user-facing language name.

---

## Grammar (EBNF)

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

### Program shape (multi-line)

Logical statements come from [`collect_program_statement_lines`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/program_surface.rs) (heredocs may span physical lines).

```ebnf
PROGRAM       = { STATEMENT } , ROOTS_LINE ;
STATEMENT     = LINE_COMMENT? , BINDING_LINE ;
BINDING_LINE  = PROGRAM_LABEL , WS_CHAR? , "=" , WS_CHAR? , RHS ;
ROOTS_LINE    = LINE_COMMENT? , ROOT , { "," , ROOT } ;
ROOT          = RHS ;
RHS           = (* postfix peel then path parse *)
PHYSICAL_LINE = { ? any codepoint except NEWLINE ? } , NEWLINE? ;
```

Binding lines use `split_assignment_at_top_level` then **`validate_program_label`** — **`e1` / `p2` / `m3`-style teaching symbols are rejected** as binding names.

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
              | "[" , FIELD_LIST , "]" ;
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
- **Mixed forms are rejected** (e.g. `method(v111{…}, p2=$)`): `UNION_CTOR_PAYLOAD` must be the sole contents of `( … )` for that overload.

**Lowering:** a sole `UNION_CTOR_PAYLOAD` is stored on the invoke IR as raw [`Value::UnionCtor`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/value.rs) inside [`InvokeInputPayload`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/typed_invoke.rs) (deserialized as `InvokeInputPayload::Raw`). The runtime lifts it with the capability `input_schema` into wire JSON (discriminator merged per variant `wire`) before CML template evaluation — same path as nested union rows inside object bodies.

**Context sensitivity:** classification into field navigation vs invoke vs zero-arity depends on **`CGS`**. Federation uses [`parse_with_cgs_layers_program`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/mod.rs) with the session [`SymbolMap`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/symbol_tuning.rs).

**Predicate lists (`Entity{ … }`):** comma-separated clauses use [`split_top_level`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/program_surface.rs) (`()`, `[]`, `{}`, quotes, tagged heredocs). Within each clause, the first top-level comparison operator (`!=`, `>=`, `<=`, `=`, `~`, `>`, `<`) is located with the same nesting rules — see [`predicate_surface.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/predicate_surface.rs). [`try_auto_correct`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_correction/auto_correct.rs) delegates to that module so correction never runs a parallel comma/`=` scanner.

---

## Tagged heredocs and tag collision

Structured string values may use **tagged heredocs** (`<<TAG` … closing line `TAG` / `TAG)` / `TAG})` / …), implemented in `plasm_core::expr_parser` ([`value.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/value.rs), shared close rules in [`heredoc_surface.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/expr_parser/heredoc_surface.rs)). The close delimiter is recognized on the **first** line (after the opener) whose **trimmed** content equals `TAG` or `TAG` followed by optional ASCII space and a parser-owned delimiter tail containing only `)`, `]`, `}`, and/or `,` on the same line. The heredoc scanner closes the string at `TAG`; the enclosing parser then consumes and validates the suffix delimiters. There is no “last closing tag wins” scan.

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

**Surface:** `source[p#,…] <<TAG` newline body newline closing `TAG`, or `source <<TAG` when columns can be inferred. The compiler projects each source row to the selected fields, then evaluates the template.

**Template engine:** bodies are **Minijinja** templates. The only binding guaranteed today is **`rows`**: a JSON array of objects, one per source row, with keys taken from the projected field names (wire/`p#` paths normalized as in bracket projection). Typical patterns:

- `{{ rows | length }}`
- `{% for r in rows %}{{ r.sha }} — {{ r.message }}{% endfor %}`
- Per-field access matching your projection list.

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
- **Cardinality-one** relations require a **singleton source proof** on `ℓ` (`source_cardinality: single` in the plan). A one-cardinality hop from a statically singleton parent yields another statically singleton row — enabling multi-hop chains such as `species = item.<one_rel>; next = species.<one_rel>`.
- **Cardinality-many** relations from a **plural** parent (`source_cardinality: many`) fan out: the runtime executes the relation IR **once per source row** (scoped `query_scoped` / `query_scoped_bindings`), then concatenates target rows.
- **Materialization is catalog-authoritative:** pure `query_scoped_bindings` always runs one scoped query per parent row (ignores decoded `relations` on the parent). `prefer_from_parent_get` (wire path + declared scoped fallback) is the executable form of “use embed when present, else scoped capability”; per-row choice uses `plasm_core::resolve_relation_row_resolution` in plan and runtime (no cache-shape heuristics).
- **Row-preserving postfix** — bracket projection `[…]`, `.limit(1)`, and `.singleton()` preserve **row entity** and adjust cardinality; further `.relation` hops use the same rules.
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

**Suffix pipeline** — after the path head (Get/Query/label), dot/bracket segments classify as [`RowSuffix`] (relation, limit, project, sort, …) and lower through one fold (`lower_suffix_stream`), including interleaved forms such as `repo.commits.limit(1).author`.

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

[`apis/proof/`](https://github.com/PlasmTools/plasm-core/tree/main/apis/proof/) ships split **`domain.yaml`** + **`mappings.yaml`**. See [`apis/proof/README.md`](https://github.com/PlasmTools/plasm-core/tree/main/apis/proof/README.md) for regeneration and exploration.
