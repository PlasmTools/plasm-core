# Plasm language definition

This document is the **canonical specification** of the user-facing Plasm surface language: path expressions, multi-line programs (bindings, postfix transforms, roots), structured values and heredocs, row-to-text templates, and the **CGS load-time rules** for structured capability inputs. It aligns with the reference implementation in **`plasm_core::expr_parser`** and with program lowering in **`plasm-agent-core`** (Plan/DAG).

For API authoring (YAML catalogs, transport), use the OSS documentation **Connect an API → Reference**, or in the Plasm monorepo the skill file `.cursor/skills/plasm-forge/reference.md` (repository root–relative).

---

## Programs and typed holes (`PlasmInputRef`)

Inside **multi-line Plasm programs** (bindings + roots), method and predicate RHS positions accept **typed references** to prior bindings, not only concrete literals. The compiler represents these as `plasm_core::value::PlasmInputRef` inside the in-memory `Value` tree (serializes to plan `__plasm_hole` objects). **HTTP one-line execute** keeps concrete-only parsing unless the host opts into the same program context.

- **Whole binding / node output:** `p91=report` means “argument `p91` receives the value produced by the program node bound to `report`” — it is **not** reparsed as a string literal after macro-style substitution.
- **Field paths:** `p91=report.content` means “`content` on the materialized output of `report`” when `report` names an in-scope program node.
- **`for_each` row:** inside `source => Effect(…)`, the row cursor is `_`; use `_.id`, `_.field`, etc. for per-row holes (same hole kind as the plan template contract).

**Entity references in brace predicates:** A field typed as **entity reference** toward another entity expects **identity** for that target (DOMAIN constructor, scalar key fields, or `anchor.<relation>`), not an arbitrary decoded row. When a binding yields a **typed row** for that same target entity, the type checker may **narrow** to identity using the catalog’s key fields (`key_vars` / id) when those scalars appear at the **top level** of the row value.

Examples (program shape):

```text
report = commits[p59,p43] <<RPT
…
RPT
sent = e3.m19(p91=report.content)
```

The parser records `p91` as a **typed input ref** to materialized output once `report` is bound. A row-to-text template (`… <<TAG … TAG`) produces a row whose generated text lives under **`content`**; string parameters must use **`report.content`**, not **`report`**, or type-checking sees value type **object** vs **String**.

For nodes whose materialized value is already a scalar row (surface query/get), **`p91=report`** may still be correct—match the schema’s expected wire shape.

---

## Invariants

1. **Transforms are core postfix syntax** — `.limit(n)`, `.sort(field[, desc])`, `.aggregate(…)`, `.group_by(field, …)`, `.singleton()`, `.page_size(n)`, bracket projections `[…]`, and row-to-text template blocks (`<<TAG … TAG`) are part of the same language as `e1{…}` / `e2(…)`.
2. **Binding is optional** — `expr.limit(20)` is valid without a prior `commits = expr` line when `expr` is a complete surface expression or an in-scope label.
3. **Artifact-level semantics today** — transforms are applied to materialized row JSON in the plan executor unless an optimizer later pushes work to HTTP (the optimizer must never change what the surface language means).
4. **No second “DAG language” for users** — diagnostics, MCP copy, and DOMAIN gloss refer to **Plasm programs** or **Plasm expressions**, not “Plasm-DAG” as a distinct syntax tier.
5. **Equivalence** — for any expression `E`, the program `x = E\nx.op(…)` and the single line `E.op(…)` (with the same postfix chain) must lower to the same executable plan shape (modulo synthetic node ids and display strings).

---

## Parser modules (reference implementation)

Surface scanning lives in **`plasm-oss/crates/plasm-core/src/expr_parser/`**:

| Module | Responsibility |
|--------|----------------|
| [`heredoc_surface.rs`](../plasm-oss/crates/plasm-core/src/expr_parser/heredoc_surface.rs) | Tagged `<<TAG …` open/close detection shared by values, postfix render tails, and multi-line program staging. |
| [`program_surface.rs`](../plasm-oss/crates/plasm-core/src/expr_parser/program_surface.rs) | Physical-line merging across heredocs (`collect_program_statement_lines`), `;;` stripping, top-level comma/`=>` splitting (`split_top_level`, `split_token_top_level`), binding `=` splitting (`split_assignment_at_top_level` / `split_assignment_for_binding`), program label validation. |
| [`program.rs`](../plasm-oss/crates/plasm-core/src/expr_parser/program.rs) | Optional shape AST: bindings + postfix-peeled primaries (`parse_program_shape`). Does not attach CGS typing. |
| [`postfix.rs`](../plasm-oss/crates/plasm-core/src/expr_parser/postfix.rs) | Postfix peel (`.limit`, `.sort`, `[projection]`, row-to-text `<<TAG`). |
| [`mod.rs`](../plasm-oss/crates/plasm-core/src/expr_parser/mod.rs) (path parser) | CGS-aware **path expression** → [`Expr`](../plasm-oss/crates/plasm-core/src/expr.rs) + optional trailing `[projection]`. |
| [`value.rs`](../plasm-oss/crates/plasm-core/src/expr_parser/value.rs) | Scalar/collection literals, strict vs lenient RHS, structured heredocs, `PlasmInputRef` holes when program context is enabled. |

Multi-line **program → Plan/DAG** lowering remains in [**`plasm_dag.rs`**](../plasm-oss/crates/plasm-agent-core/src/plasm_dag.rs), which calls **`program_surface`** and **`postfix`**, then [`parse_with_cgs_layers_program`](../plasm-oss/crates/plasm-core/src/expr_parser/mod.rs) on each session-expanded primary.

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

Opener/close rules are implemented in [`heredoc_surface.rs`](../plasm-oss/crates/plasm-core/src/expr_parser/heredoc_surface.rs). **Operational discipline** for choosing `TAG` (collision-safe payloads) is under [Tagged heredocs and tag collision](#tagged-heredocs-and-tag-collision) below.

```ebnf
HEREDOC_OPEN_LINE = "<<" , TAG , { WS_CHAR } , NEWLINE ;
STRUCTURED_HEREDOC = HEREDOC_OPEN_LINE , HEREDOC_BODY , HEREDOC_CLOSE_LINE ;
(* HEREDOC_BODY / HEREDOC_CLOSE_LINE: first trimmed matching close line wins — see implementation. *)
```

### Program shape (multi-line)

Logical statements come from [`collect_program_statement_lines`](../plasm-oss/crates/plasm-core/src/expr_parser/program_surface.rs) (heredocs may span physical lines).

```ebnf
PROGRAM       = { STATEMENT } , ROOTS_LINE ;
STATEMENT     = LINE_COMMENT? , BINDING_LINE ;
BINDING_LINE  = PROGRAM_LABEL , WS_CHAR? , "=" , WS_CHAR? , RHS ;
ROOTS_LINE    = LINE_COMMENT? , ROOT , { "," , ROOT } ;
ROOT          = RHS ;
RHS           = (* postfix peel then path parse *)
PHYSICAL_LINE = { ? any codepoint except NEWLINE ? } , NEWLINE? ;
```

Binding lines use `split_assignment_at_top_level` then **`validate_program_label`** — **`e1` / `p2` / `m3`-style DOMAIN symbols are rejected** as binding names.

### Postfix chain (per `RHS` fragment)

After [`peel_postfix_suffixes`](../plasm-oss/crates/plasm-core/src/expr_parser/postfix.rs), surface postfix applies **inner-to-outer** per the [chaining order](#chaining-order) invariant.

```ebnf
POSTFIX_OP    = "singleton"
              | "limit" , "(" , INTEGER , ")"
              | "page_size" , "(" , INTEGER , ")"
              | "sort" , "(" , SORT_ARGS , ")"
              | "aggregate" , "(" , AGG_ARGS , ")"
              | "group_by" , "(" , GROUP_ARGS , ")"
              | "[" , FIELD_LIST , "]" ;
FIELD_LIST    = IDENT , { "," , IDENT } ;
```

**Row-to-text:** optional render tail after the postfix head — `… [ fields ]? <<TAG …`; see [`try_parse_render_tail`](../plasm-oss/crates/plasm-core/src/expr_parser/postfix.rs) and [Row-to-Text Templates](#row-to-text-templates-content-and-minijinja).

### Path expression (CGS-aware)

Abbreviated from [`expr_parser/mod.rs`](../plasm-oss/crates/plasm-core/src/expr_parser/mod.rs).

```ebnf
EXPR          = SOURCE , { PIPE_SEGMENT } , [ "[" , FIELD_LIST , "]" ] ;
SOURCE        = Entity , "(" , ARG_LIST , ")"
              | Entity , "{" , PRED_LIST , "}"
              | Entity , "~" , SEARCH_PHRASE , [ "{" , PRED_LIST , "}" ]
              | Entity
              | PAGE_CALL ;
PIPE_SEGMENT  = "." , FIELD_NAME
              | "." , METHOD , [ "(" , DOTTED_ARGS , ")" ]
              | "." , METHOD , "()"
              | ".^" , Entity , [ "{" , PRED_LIST , "}" ] ;
PRED          = FIELD_NAME , COMP_OP , [ VALUE ]
              | ForeignEntity , "." , FIELD_NAME , COMP_OP , [ VALUE ] ;
COMP_OP       = "=" | "!=" | ">" | "<" | ">=" | "<=" | "~" ;
VALUE         = QUOTED_STRING | STRUCTURED_HEREDOC | UUID | NUMBER | BARE_WORD
              | "[" , { VALUE , "," } , VALUE , "]"
              | (* phrase / lenient regions — see value.rs *)
              ;
```

**Context sensitivity:** classification into field navigation vs invoke vs zero-arity depends on **`CGS`**. Federation uses [`parse_with_cgs_layers_program`](../plasm-oss/crates/plasm-core/src/expr_parser/mod.rs) with the session [`SymbolMap`](../plasm-oss/crates/plasm-core/src/symbol_tuning.rs).

---

## Tagged heredocs and tag collision

Structured string values may use **tagged heredocs** (`<<TAG` … closing line `TAG` / `TAG)` / …), implemented in `plasm_core::expr_parser` ([`value.rs`](../plasm-oss/crates/plasm-core/src/expr_parser/value.rs), shared close rules in [`heredoc_surface.rs`](../plasm-oss/crates/plasm-core/src/expr_parser/heredoc_surface.rs)). The close delimiter is recognized on the **first** line (after the opener) whose **trimmed** content equals `TAG` or `TAG` followed by optional ASCII space and a single `)` / `,` / `}` on the same line. There is no “last closing tag wins” scan.

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
- **`Expr`** — path IR ([`Expr`](../plasm-oss/crates/plasm-core/src/expr.rs)).
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

**Capability inputs** judgements over **`InputType`** in `Σ` — see below.

---

## Capability inputs (CGS load-time semantics)

### Registry vs structural fields

- **Entity fields** (`FieldSchema`) always use `value_ref` → a row in top-level `values:`.
- **Capability object parameters** (`parameters:` entries) use **exactly one** of:
  - `value_ref` → `values:` (registry), or
  - `input_type` → inline structural [`InputType`](../plasm-oss/crates/plasm-core/src/schema.rs) (object / array / union / value / none).
- **`input_schema.input_type.fields`** use the same XOR: each field is either registry-backed (`value_ref`) or structural (`input_type`). When both `parameters` and `input_schema` are present, loader-merged object fields must not duplicate names.

Structural inline fields are **not** `values:` slots; registry-only consumers may skip them when a `NamedValueSchema` is required.

### Tagged unions (`InputType::Union`)

- Each variant has **`wire`** (`field` + `value`) — the **discriminator** merged into HTTP/CML JSON when lowering ([`TypedInvokeInput::Union`](../plasm-oss/crates/plasm-core/src/typed_invoke.rs)).
- **Surface typing** matches the variant **body** only (no discriminator in the Plasm value before lowering).
- **Lifting** tries each variant’s body shape in order until one matches.

### Surface constructor literals (`v` + digits + `{…}`)

A token **`v`** plus ASCII digits plus a braced map parses as JSON-like [`Value::Object`](../plasm-oss/crates/plasm-core/src/expr_parser/value.rs). Digits may align with DOMAIN constructor mnemonics; the runtime does not reinterpret them beyond parsing.

---

## Proof catalog

[`apis/proof/`](../apis/proof/) ships split **`domain.yaml`** + **`mappings.yaml`**. See [`apis/proof/README.md`](../apis/proof/README.md) for regeneration and exploration.
